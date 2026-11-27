use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use eframe::egui;

use crate::{
    actions::{Navigation, PreparedAction},
    agent_layout::MainDial,
    claude,
    codex::{self, Activity, Agent, Binding, KeyStyle, Mode, ResolvedLayout, Status},
    deck_model::LCD_PIXELS,
    device::{self, Deck, Input},
    easter_egg::{self, EasterEgg},
    foreground::Foreground,
    grok,
    plugins::{self, Instance, Shared, server::lock},
    profile::{self, Control, DIALS, DISPLAY_KEYS, Direction, PreparedLayout},
    theme, usage,
    workspace::{PreparedWorkspace, Scene},
};

const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);
const STEP_GAP: Duration = Duration::from_millis(60);
const MODE_REPLY_POLL: Duration = Duration::from_millis(50);

pub enum Request {
    Apply(PreparedWorkspace),
    Brightness(u8),
    Select(Scene),
    Disconnect,
    SetMode(ModeState),
    AgentMainDial(MainDial),
    EasterEgg(bool),
    CodexLayout(Box<ResolvedLayout>),
    GrokSelection(Option<String>, bool),
    ClaudeLayout(Arc<claude::Layout>),
}

pub enum Update {
    Connected(Scene),
    /// The deck stopped responding. DeckX keeps the session and reopens the deck when it returns.
    Reconnecting(String),
    Applied(Scene),
    Brightness(u8),
    Changed(Scene),
    Input(Control),
    EasterEgg(bool),
    ActionError(String),
    Finished(Result<()>),
    Mode(u64, ModeUpdate),
}

pub(crate) enum ModeUpdate {
    Exited,
    CycleAgentMode(Direction),
    CodexRendered(KeyStyle, usize),
    CodexSelected(Option<usize>),
    GrokSelected(Option<String>, bool),
    ClaudeSelected(Option<(usize, uuid::Uuid)>),
    ShowUsage,
}

#[derive(Clone, Copy)]
pub enum Connection {
    Opening,
    Open,
    Reconnecting,
    Closing,
}

pub struct Session {
    pub requests: Sender<Request>,
    pub updates: Receiver<Update>,
    pub connection: Connection,
    thread: Option<JoinHandle<()>>,
}

impl Session {
    pub fn start(
        workspace: PreparedWorkspace,
        ctx: egui::Context,
        hub: Option<Shared>,
        activity: Activity,
        mode: ModeState,
        usage: usage::Readings,
    ) -> Result<Self> {
        let (requests, incoming) = mpsc::channel();
        let (outgoing, updates) = mpsc::channel();
        let notifier = Notifier { outgoing, ctx };
        let thread = std::thread::Builder::new()
            .name("DeckX HID".into())
            .spawn(move || {
                let result =
                    run_device(workspace, mode, &incoming, &notifier, hub, activity, usage);
                notifier.send(Update::Finished(result));
            })?;
        Ok(Self {
            requests,
            updates,
            connection: Connection::Opening,
            thread: Some(thread),
        })
    }

    /// Asks the device thread to blank the deck and stop, and waits up to `wait` for it.
    pub fn shut_down(&mut self, wait: Duration) {
        let _ = self.requests.send(Request::Disconnect);
        let Some(thread) = self.thread.take() else {
            return;
        };
        let deadline = Instant::now() + wait;
        while !thread.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if thread.is_finished() {
            let _ = thread.join();
        }
    }
}

#[derive(Clone)]
struct Notifier {
    outgoing: Sender<Update>,
    ctx: egui::Context,
}

impl Notifier {
    /// Returns false once the UI has gone away.
    fn send(&self, update: Update) -> bool {
        let sent = self.outgoing.send(update).is_ok();
        self.ctx.request_repaint();
        sent
    }

    fn action_error(&self, control: Control, error: &anyhow::Error) {
        self.send(Update::ActionError(format!(
            "{}: {error:#}",
            control.label()
        )));
    }

    fn mode(&self, generation: u64, update: ModeUpdate) {
        self.send(Update::Mode(generation, update));
    }
}

/// Runs bound actions off the HID thread, so a slow app launch or shell call cannot stall
/// key reads and the deck's keep-alive.
struct Actions {
    single: Sender<(Job, Control)>,
    sequences: Sender<(Vec<PreparedAction>, Control, u64)>,
    running: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
}

impl Actions {
    fn start(notifier: &Notifier) -> Result<Self> {
        let running = Arc::new(AtomicBool::new(false));
        let generation = Arc::new(AtomicU64::new(0));
        let (single, jobs) = mpsc::channel::<(Job, Control)>();
        let report = notifier.clone();
        std::thread::Builder::new()
            .name("DeckX actions".into())
            .spawn(move || {
                let _com = Com::initialize();
                for (job, control) in jobs {
                    if let Err(error) = job() {
                        report.action_error(control, &error);
                    }
                }
            })?;
        let (sequences, jobs) = mpsc::channel::<(Vec<PreparedAction>, Control, u64)>();
        let report = notifier.clone();
        let (finished, current) = (running.clone(), generation.clone());
        std::thread::Builder::new()
            .name("DeckX multi actions".into())
            .spawn(move || {
                let _com = Com::initialize();
                for (steps, control, started) in jobs {
                    let cancelled = || current.load(Ordering::Acquire) != started;
                    if let Err(error) = run_steps(steps, cancelled) {
                        report.action_error(control, &error);
                    }
                    finished.store(false, Ordering::Release);
                }
            })?;
        Ok(Self {
            single,
            sequences,
            running,
            generation,
        })
    }

    fn run(&self, action: PreparedAction, control: Control) {
        self.spawn(control, move || action.run());
    }

    fn open_bot(&self, id: &str, control: Control) {
        let id = id.to_owned();
        self.spawn(control, move || crate::actions::open_grok_agent(&id));
    }

    fn spawn(&self, control: Control, job: impl FnOnce() -> Result<()> + Send + 'static) {
        let _ = self.single.send((Box::new(job), control));
    }

    /// Starts a multi action. Returns false while another one is still running.
    fn start_sequence(&self, steps: &[PreparedAction], control: Control) -> bool {
        if self.running.swap(true, Ordering::AcqRel) {
            return false;
        }
        let started = self.generation.load(Ordering::Acquire);
        if self
            .sequences
            .send((steps.to_vec(), control, started))
            .is_err()
        {
            self.running.store(false, Ordering::Release);
        }
        true
    }

    /// Stops the running multi action before its next step.
    fn cancel(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    fn busy(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }
}

impl Drop for Actions {
    fn drop(&mut self) {
        self.cancel();
    }
}

type Job = Box<dyn FnOnce() -> Result<()> + Send>;

fn run_steps(steps: Vec<PreparedAction>, cancelled: impl Fn() -> bool) -> Result<()> {
    for step in steps {
        let pause = match step {
            PreparedAction::Delay(duration) => duration,
            action => {
                if cancelled() {
                    return Ok(());
                }
                action.run()?;
                STEP_GAP
            }
        };
        let until = Instant::now() + pause;
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            if cancelled() {
                return Ok(());
            }
            std::thread::sleep(left.min(Duration::from_millis(20)));
        }
    }
    Ok(())
}

/// Keeps a COM apartment open on an action thread. ShellExecute can hand work to shell
/// extensions that expect one.
struct Com(bool);

impl Com {
    fn initialize() -> Self {
        use windows::Win32::System::Com::{
            COINIT, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx,
        };
        let flags = COINIT(COINIT_APARTMENTTHREADED.0 | COINIT_DISABLE_OLE1DDE.0);
        // The apartment is released by `Drop` on the same thread.
        Self(unsafe { CoInitializeEx(None, flags) }.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModeKind {
    Workspace,
    Codex,
    Grok,
    Claude,
}

impl ModeKind {
    pub(crate) const ORDER: [Self; 4] = [Self::Workspace, Self::Codex, Self::Grok, Self::Claude];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Workspace => "Workspace",
            Self::Codex => "Codex",
            Self::Grok => "Grok Bot",
            Self::Claude => "Claude",
        }
    }

    pub(crate) fn cycle_agent(self, direction: Direction) -> Option<Self> {
        let agents = &Self::ORDER[1..];
        let index = agents.iter().position(|&mode| mode == self)?;
        let step = match direction {
            Direction::Clockwise => 1,
            Direction::Counterclockwise => agents.len() - 1,
            Direction::Press => return None,
        };
        Some(agents[(index + step) % agents.len()])
    }
}

#[derive(Clone, Default)]
pub(crate) enum Active {
    #[default]
    Workspace,
    Codex(Box<Mode>),
    Grok(grok::Mode),
    Claude(claude::Mode),
}

impl Active {
    pub(crate) fn kind(&self) -> ModeKind {
        match self {
            Self::Workspace => ModeKind::Workspace,
            Self::Codex(_) => ModeKind::Codex,
            Self::Grok(_) => ModeKind::Grok,
            Self::Claude(_) => ModeKind::Claude,
        }
    }

    pub(crate) fn usage_key(&self) -> Option<(usize, usage::Provider)> {
        let (preset, provider) = match self {
            Self::Workspace => return None,
            Self::Codex(mode) => (mode.layout.bindings.preset(), usage::Provider::Codex),
            Self::Grok(mode) => (mode.bindings.preset, usage::Provider::GrokBot),
            Self::Claude(mode) => (mode.layout.preset(), usage::Provider::Claude),
        };
        Some((preset.usage_key(DISPLAY_KEYS)?, provider))
    }

    fn exits_on(&self, control: Control) -> bool {
        match self {
            Self::Workspace => false,
            Self::Codex(mode) => {
                mode.layout.bindings.active_binding(control) == Some(&Binding::ExitMode)
            }
            Self::Grok(mode) => {
                mode.bindings.active_binding(control) == Some(&grok::Binding::ExitMode)
            }
            Self::Claude(mode) => {
                mode.layout.active_binding(control) == Some(&claude::Binding::ExitMode)
            }
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct ModeState {
    pub active: Active,
    /// Tags deck updates so an earlier visit to a mode can't change the current view.
    pub generation: u64,
}

fn run_device(
    workspace: PreparedWorkspace,
    mode: ModeState,
    requests: &Receiver<Request>,
    notifier: &Notifier,
    hub: Option<Shared>,
    activity: Activity,
    usage: usage::Readings,
) -> Result<()> {
    let mut deck = Deck::connect(workspace.brightness)?;
    let mut runtime = Runtime::new(workspace, mode, hub, activity, usage, notifier.clone())?;
    let mut deferred = VecDeque::new();
    loop {
        match runtime.run(&mut deck, requests, &mut deferred) {
            Err(error) if device::is_transport_error(&error) => {
                drop(deck);
                runtime.lost();
                notifier.send(Update::Reconnecting(format!("{error:#}")));
                match reconnect(requests, &mut deferred, runtime.workspace.brightness) {
                    Some(next) => deck = next,
                    None => return Ok(()),
                }
            }
            result => {
                // Blank the keys rather than leave bindings on screen that no longer run.
                let _ = deck.clear();
                return result;
            }
        }
    }
}

/// Polls for the deck until it opens again. Requests that arrive meanwhile run once it does.
fn reconnect(
    requests: &Receiver<Request>,
    deferred: &mut VecDeque<Request>,
    brightness: u8,
) -> Option<Deck> {
    loop {
        let retry = Instant::now() + RECONNECT_INTERVAL;
        while let Some(wait) = retry.checked_duration_since(Instant::now()) {
            match requests.recv_timeout(wait) {
                Ok(Request::Disconnect) | Err(RecvTimeoutError::Disconnected) => return None,
                Ok(request) => deferred.push_back(request),
                Err(RecvTimeoutError::Timeout) => break,
            }
        }
        if let Ok(deck) = Deck::connect(brightness) {
            return Some(deck);
        }
    }
}

struct Runtime {
    workspace: PreparedWorkspace,
    scene: Scene,
    manual_scene: Scene,
    following_app: bool,
    foreground: Foreground,
    history: Vec<Scene>,
    mode: ModeState,
    awaiting_mode: bool,
    easter_egg: EasterEgg,
    plugins: PluginDeck,
    codex_display: CodexDisplay,
    grok_display: GrokDisplay,
    claude_display: ClaudeDisplay,
    usage: usage::Readings,
    usage_display: UsageDisplay,
    activity: Activity,
    actions: Actions,
    notifier: Notifier,
}

impl Runtime {
    fn new(
        workspace: PreparedWorkspace,
        mode: ModeState,
        hub: Option<Shared>,
        activity: Activity,
        usage: usage::Readings,
        notifier: Notifier,
    ) -> Result<Self> {
        let scene = workspace.home;
        Ok(Self {
            workspace,
            scene,
            manual_scene: scene,
            following_app: false,
            foreground: Foreground::new(),
            history: Vec::new(),
            mode,
            awaiting_mode: false,
            easter_egg: EasterEgg::default(),
            plugins: PluginDeck::new(hub),
            codex_display: CodexDisplay::default(),
            grok_display: GrokDisplay::default(),
            claude_display: ClaudeDisplay::default(),
            usage,
            usage_display: UsageDisplay::default(),
            activity,
            actions: Actions::start(&notifier)?,
            notifier,
        })
    }

    /// Drives an open deck until the session ends or the deck fails.
    fn run(
        &mut self,
        deck: &mut Deck,
        requests: &Receiver<Request>,
        deferred: &mut VecDeque<Request>,
    ) -> Result<()> {
        if !self.resume(deck)? {
            return Ok(());
        }
        // Take queued requests one at a time, so any not yet handled survive another failure.
        while let Some(request) = deferred.pop_front() {
            if !self.handle(deck, request)? {
                return Ok(());
            }
        }
        loop {
            loop {
                let request = if self.awaiting_mode {
                    // Leave later HID reports queued until the new key images can be drawn.
                    deck.keep_alive_if_due()?;
                    match requests.recv_timeout(MODE_REPLY_POLL) {
                        Ok(request) => request,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => return Ok(()),
                    }
                } else {
                    match requests.try_recv() {
                        Ok(request) => request,
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => return Ok(()),
                    }
                };
                if !self.handle(deck, request)? {
                    return Ok(());
                }
            }
            if !self.tick(deck)? {
                return Ok(());
            }
        }
    }

    /// Redraws everything on a newly opened deck.
    fn resume(&mut self, deck: &Deck) -> Result<bool> {
        self.plugins.attach();
        self.invalidate_agent_displays();
        if matches!(self.mode.active, Active::Workspace) {
            show(deck, &self.workspace, self.scene, &mut self.plugins)?;
        }
        if !self.notifier.send(Update::Connected(self.scene)) {
            return Ok(false);
        }
        self.notifier
            .send(Update::Brightness(self.workspace.brightness));
        Ok(true)
    }

    /// The deck stopped responding. Releases what it was holding.
    fn lost(&mut self) {
        self.easter_egg = EasterEgg::default();
        self.plugins.release_held();
        self.plugins.detach();
    }

    /// Applies a request from the UI. State changes first, so a reconnect redraws it.
    fn handle(&mut self, deck: &Deck, request: Request) -> Result<bool> {
        match request {
            Request::Apply(next) => {
                if self.workspace.agent_main_dial != next.agent_main_dial {
                    self.awaiting_mode = false;
                }
                self.workspace = next;
                self.scene = self.workspace.home;
                self.manual_scene = self.scene;
                self.following_app = false;
                self.foreground = Foreground::new();
                self.history.clear();
                self.actions.cancel();
                if matches!(self.mode.active, Active::Workspace) && !self.easter_egg.visible() {
                    show(deck, &self.workspace, self.scene, &mut self.plugins)?;
                } else {
                    self.invalidate_agent_displays();
                    deck.set_brightness(self.workspace.brightness)?;
                }
                self.notifier.send(Update::Applied(self.scene));
                self.notifier
                    .send(Update::Brightness(self.workspace.brightness));
            }
            Request::Brightness(brightness) => {
                self.workspace.brightness = brightness;
                deck.set_brightness(brightness)
                    .context("Could not set deck brightness")?;
                self.notifier.send(Update::Brightness(brightness));
            }
            Request::Select(next) if self.workspace.pages.contains_key(&next) => {
                self.manual_scene = next;
                self.following_app = false;
                self.history.clear();
                self.actions.cancel();
                if self.scene != next {
                    self.scene = next;
                    if matches!(self.mode.active, Active::Workspace) && !self.easter_egg.visible() {
                        show(deck, &self.workspace, next, &mut self.plugins)?;
                    }
                    self.notifier.send(Update::Changed(next));
                }
            }
            Request::Select(_) => {}
            Request::SetMode(next) => self.set_mode(deck, next)?,
            Request::AgentMainDial(behavior) => {
                self.workspace.agent_main_dial = behavior;
                self.awaiting_mode = false;
            }
            Request::EasterEgg(enabled) => self.workspace.easter_egg = enabled,
            Request::CodexLayout(layout) => {
                if let Active::Codex(mode) = &mut self.mode.active {
                    let previous = mode.selected;
                    mode.set_layout(*layout);
                    if previous != mode.selected {
                        self.notifier.mode(
                            self.mode.generation,
                            ModeUpdate::CodexSelected(mode.selected),
                        );
                    }
                }
            }
            Request::GrokSelection(id, highlight) => {
                if let Active::Grok(mode) = &mut self.mode.active {
                    mode.refresh();
                    mode.select_id(id.as_deref());
                    mode.highlight_selection = highlight && mode.selected.is_some();
                }
            }
            Request::ClaudeLayout(layout) => {
                if let Active::Claude(mode) = &mut self.mode.active {
                    let previous = mode.selection();
                    mode.set_layout(layout);
                    let selected = mode.selection();
                    if previous != selected {
                        self.notifier
                            .mode(self.mode.generation, ModeUpdate::ClaudeSelected(selected));
                    }
                }
            }
            Request::Disconnect => return Ok(false),
        }
        Ok(true)
    }

    fn invalidate_agent_displays(&mut self) {
        self.codex_display = CodexDisplay::default();
        self.grok_display = GrokDisplay::default();
        self.claude_display = ClaudeDisplay::default();
        self.usage_display = UsageDisplay::default();
    }

    fn set_mode(&mut self, deck: &Deck, next: ModeState) -> Result<()> {
        self.mode = next;
        self.awaiting_mode = false;
        self.actions.cancel();
        self.invalidate_agent_displays();
        if matches!(self.mode.active, Active::Workspace) {
            self.foreground = Foreground::new();
            if !self.easter_egg.visible() {
                show(deck, &self.workspace, self.scene, &mut self.plugins)?;
            }
        } else {
            self.plugins.release_held();
            self.plugins.hide();
        }
        Ok(())
    }

    fn tick(&mut self, deck: &mut Deck) -> Result<bool> {
        if !self.easter_egg.visible() {
            if matches!(self.mode.active, Active::Workspace) {
                self.refresh_workspace(deck)?;
            } else {
                self.refresh_agent_displays(deck)?;
            }
        }
        let input = deck.read()?;
        if let Some(press) = input.filter(|input| input.down)
            && !self.notifier.send(Update::Input(press.control))
        {
            return Ok(false);
        }
        let step = self
            .easter_egg
            .poll(self.workspace.easter_egg, Instant::now(), input);
        match step.display {
            Some(easter_egg::Display::Sydney) => {
                self.plugins.release_held();
                let images = easter_egg::sydney(self.workspace.rotation)?;
                deck.set_key_images(&images)?;
                self.notifier.send(Update::EasterEgg(true));
            }
            Some(easter_egg::Display::Restore) => {
                self.invalidate_agent_displays();
                if matches!(self.mode.active, Active::Workspace) {
                    show(deck, &self.workspace, self.scene, &mut self.plugins)?;
                    self.refresh_workspace(deck)?;
                } else {
                    self.refresh_agent_displays(deck)?;
                }
                self.notifier.send(Update::EasterEgg(false));
            }
            None => {}
        }
        for input in step.inputs.into_iter().flatten() {
            if matches!(self.mode.active, Active::Workspace) {
                self.workspace_input(deck, input)?;
            } else if input.down && !self.agent_input(deck, input.control)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn refresh_agent_displays(&mut self, deck: &Deck) -> Result<()> {
        let rotation = self.workspace.rotation;
        let generation = self.mode.generation;
        if let Active::Grok(mode) = &mut self.mode.active {
            mode.refresh();
        }
        let usage_key = self.mode.active.usage_key();
        let reserved = usage_key.map(|(index, _)| index);
        match &mut self.mode.active {
            Active::Workspace => return Ok(()),
            Active::Grok(mode) => {
                self.grok_display.refresh(deck, mode, reserved, rotation)?;
            }
            Active::Codex(mode) => {
                let count =
                    self.codex_display
                        .refresh(deck, mode, &self.activity, reserved, rotation)?;
                if count > 0 {
                    self.notifier.mode(
                        generation,
                        ModeUpdate::CodexRendered(mode.layout.bindings.key_style, count),
                    );
                }
            }
            Active::Claude(mode) => self.claude_display.refresh(deck, mode, rotation)?,
        }
        self.usage_display.refresh(
            deck,
            usage_key.map(|(index, provider)| {
                (index, usage::KeyFace::from_report(self.usage.get(provider)))
            }),
            rotation,
        )
    }

    fn agent_input(&mut self, deck: &Deck, control: Control) -> Result<bool> {
        let generation = self.mode.generation;
        let usage_key = self.mode.active.usage_key();
        if let Some(turn) = self.workspace.agent_main_dial.mode_turn(control) {
            self.awaiting_mode = true;
            return Ok(self
                .notifier
                .send(Update::Mode(generation, ModeUpdate::CycleAgentMode(turn))));
        }
        if self.mode.active.exits_on(control) {
            // Tell the UI first, so it still follows if redrawing the workspace fails and the
            // deck reconnects. The exit key's release reaches the workspace, which ignores
            // releases without a matching press.
            self.notifier.mode(generation, ModeUpdate::Exited);
            self.set_mode(
                deck,
                ModeState {
                    active: Active::Workspace,
                    generation,
                },
            )?;
            return Ok(true);
        }
        let run = |action| {
            self.actions.run(action, control);
            Ok(())
        };
        match &mut self.mode.active {
            Active::Workspace => {}
            Active::Grok(mode) => {
                grok_input(mode, control, &self.notifier, &self.actions, generation)
            }
            Active::Codex(mode) => {
                let selected = mode.selected;
                if let Err(error) = mode.input_with(control, run) {
                    self.notifier.action_error(control, &error);
                }
                if selected != mode.selected {
                    self.notifier
                        .mode(generation, ModeUpdate::CodexSelected(mode.selected));
                }
            }
            Active::Claude(mode) => {
                let previous = mode.selection();
                if let Err(error) = mode.input_with(control, run) {
                    self.notifier.action_error(control, &error);
                }
                let selected = mode.selection();
                if previous != selected {
                    self.notifier
                        .mode(generation, ModeUpdate::ClaudeSelected(selected));
                }
            }
        }
        if usage_key.is_some_and(|(index, _)| control == Control::Button(index)) {
            self.notifier.mode(generation, ModeUpdate::ShowUsage);
        }
        Ok(true)
    }

    fn refresh_workspace(&mut self, deck: &Deck) -> Result<()> {
        if !self.actions.busy()
            && !self.workspace.app_profiles.is_empty()
            && let Some(app) = self.foreground.changed_app()
        {
            let association = self.workspace.app_profiles.get(&app);
            let next = match association {
                Some(page) if page.profile == self.scene.profile => self.scene,
                Some(page) => *page,
                None => self.manual_scene,
            };
            self.following_app = association.is_some();
            if next != self.scene {
                self.scene = next;
                self.history.clear();
                show(deck, &self.workspace, next, &mut self.plugins)?;
                self.notifier.send(Update::Changed(next));
            }
        }
        let layout = &self.workspace.pages[&self.scene];
        self.plugins.refresh(deck, layout, self.workspace.rotation)
    }

    fn workspace_input(&mut self, deck: &Deck, input: Input) -> Result<()> {
        let layout = &self.workspace.pages[&self.scene];
        let control = input.control;
        if self
            .plugins
            .dispatch(&input, layout.action(control), &self.notifier)
            || !input.down
        {
            return Ok(());
        }
        match layout.action(control).clone() {
            PreparedAction::Navigate(navigation) => {
                let next = navigate(&self.workspace, self.scene, &mut self.history, navigation);
                if next != self.scene {
                    self.scene = next;
                    if !self.following_app || matches!(navigation, Navigation::Profile(_)) {
                        self.manual_scene = next;
                        self.following_app = false;
                    }
                    self.actions.cancel();
                    show(deck, &self.workspace, next, &mut self.plugins)?;
                    self.notifier.send(Update::Changed(next));
                }
            }
            PreparedAction::Sequence(steps) => {
                if !self.actions.start_sequence(&steps, control) {
                    self.notifier.send(Update::ActionError(
                        "A multi action is already running".into(),
                    ));
                }
            }
            action => self.actions.run(action, control),
        }
        Ok(())
    }
}

fn grok_input(
    mode: &mut grok::Mode,
    control: Control,
    notifier: &Notifier,
    actions: &Actions,
    generation: u64,
) {
    let previous = mode.selection();
    let open = |id: &str| {
        actions.open_bot(id, control);
        Ok(())
    };
    let run = |action| {
        actions.run(action, control);
        Ok(())
    };
    if let Err(error) = mode.input_with(control, open, run) {
        notifier.action_error(control, &error);
    }
    let (id, highlight) = mode.selection();
    if previous != (id.clone(), highlight) {
        notifier.mode(generation, ModeUpdate::GrokSelected(id, highlight));
    }
}

#[derive(Default)]
struct ClaudeDisplay {
    layout: Option<Arc<claude::Layout>>,
    selected: Option<usize>,
}

impl ClaudeDisplay {
    fn refresh(&mut self, deck: &Deck, mode: &claude::Mode, rotation: u16) -> Result<()> {
        if self.selected == mode.selected
            && self
                .layout
                .as_ref()
                .is_some_and(|layout| Arc::ptr_eq(layout, &mode.layout))
        {
            return Ok(());
        }
        for index in 0..DISPLAY_KEYS {
            let control = Control::Button(index);
            let Some(binding) = mode.layout.active_binding(control) else {
                continue;
            };
            let selected = mode.selected == Some(index);
            if self
                .layout
                .as_ref()
                .is_some_and(|layout| layout.active_binding(control) == Some(binding))
                && (self.selected == Some(index)) == selected
            {
                continue;
            }
            let tile = match mode.layout.pinned(index) {
                Some((_, title)) => claude::title_style(selected).render(title),
                None if binding.tile_label().is_empty() => {
                    image::RgbImage::new(LCD_PIXELS, LCD_PIXELS)
                }
                None => profile::label_tile([26, 28, 33], binding.tile_label()),
            };
            let image = profile::encode_tile(tile, rotation)?;
            deck.set_key_image(index, &image)
                .with_context(|| format!("Could not update Claude key {}", index + 1))?;
        }
        self.layout = Some(Arc::clone(&mode.layout));
        self.selected = mode.selected;
        Ok(())
    }
}

#[derive(Default)]
struct UsageDisplay {
    current: Option<(usize, usage::KeyFace)>,
}

impl UsageDisplay {
    fn refresh(
        &mut self,
        deck: &Deck,
        next: Option<(usize, usage::KeyFace)>,
        rotation: u16,
    ) -> Result<()> {
        if self.current != next {
            if let Some((index, face)) = next {
                let image = profile::encode_tile(face.render(theme::LINE), rotation)?;
                deck.set_key_image(index, &image)
                    .with_context(|| format!("Could not update usage key {}", index + 1))?;
            }
            self.current = next;
        }
        Ok(())
    }
}

#[derive(Default)]
struct CodexDisplay {
    keys: [Option<CodexKey>; DISPLAY_KEYS],
}

struct CodexKey {
    chat: Option<Agent>,
    action_label: &'static str,
    status: Status,
    selected: bool,
    style: KeyStyle,
}

#[derive(Default)]
struct GrokDisplay {
    keys: [Option<GrokKey>; DISPLAY_KEYS],
}

struct GrokKey {
    id: String,
    label: String,
    unread: bool,
    main: bool,
    selected: bool,
    avatar: Option<std::sync::Arc<image::RgbaImage>>,
}

impl GrokDisplay {
    fn refresh(
        &mut self,
        deck: &Deck,
        mode: &grok::Mode,
        reserved: Option<usize>,
        rotation: u16,
    ) -> Result<()> {
        for (index, previous) in self.keys.iter_mut().enumerate() {
            if reserved == Some(index) {
                *previous = None;
                continue;
            }
            let bot = mode.layout.bot(index);
            let id = bot.map_or("", |bot| bot.id.as_str());
            let label = bot.map_or_else(
                || mode.bindings.binding(Control::Button(index)).tile_label(),
                grok::Bot::label,
            );
            let unread = bot.is_some_and(grok::Bot::unread);
            let selected = mode.highlight_selection && mode.selected == Some(index);
            let main = mode.layout.is_main(index);
            let avatar = mode.avatar(index);
            if previous.as_ref().is_some_and(|prev| {
                prev.id == id
                    && prev.label == label
                    && prev.unread == unread
                    && prev.main == main
                    && prev.selected == selected
                    && match (&prev.avatar, avatar) {
                        (Some(a), Some(b)) => std::sync::Arc::ptr_eq(a, b),
                        (None, None) => true,
                        _ => false,
                    }
            }) {
                continue;
            }
            let tile = if bot.is_some() {
                grok::key_tile(bot, avatar.map(|frame| frame.as_ref()), selected, main)
            } else {
                grok::key::command_tile(label)
            };
            let image = profile::encode_tile(tile, rotation)?;
            deck.set_key_image(index, &image)
                .with_context(|| format!("Could not update Grok Bot key {}", index + 1))?;
            *previous = Some(GrokKey {
                id: id.into(),
                label: label.into(),
                unread,
                main,
                selected,
                avatar: avatar.cloned(),
            });
        }
        Ok(())
    }
}

impl CodexDisplay {
    fn refresh(
        &mut self,
        deck: &Deck,
        mode: &Mode,
        activity: &Activity,
        reserved: Option<usize>,
        rotation: u16,
    ) -> Result<usize> {
        let mut written = 0;
        for (index, previous) in self.keys.iter_mut().enumerate() {
            if reserved == Some(index) {
                *previous = None;
                continue;
            }
            let chat = mode.layout.agents[index].as_ref();
            let status = chat.map_or(Status::Waiting, |chat| activity.status(chat.id));
            let selected = mode.selected == Some(index);
            let style = mode.layout.bindings.key_style;
            let action_label = mode
                .layout
                .bindings
                .binding(Control::Button(index))
                .tile_label();
            if previous.as_ref().is_some_and(|key| {
                key.chat.as_ref() == chat
                    && key.action_label == action_label
                    && key.status == status
                    && key.selected == selected
                    && key.style == style
            }) {
                continue;
            }
            let image = profile::encode_tile(
                codex::tile(chat, action_label, status, selected, style),
                rotation,
            )?;
            deck.set_key_image(index, &image)
                .with_context(|| format!("Could not update Codex agent key {}", index + 1))?;
            written += 1;
            *previous = Some(CodexKey {
                chat: chat.cloned(),
                action_label,
                status,
                selected,
                style,
            });
        }
        Ok(written)
    }
}

fn show(
    deck: &Deck,
    workspace: &PreparedWorkspace,
    scene: Scene,
    plugins: &mut PluginDeck,
) -> Result<()> {
    let page = workspace
        .pages
        .get(&scene)
        .context("The selected page was removed")?;
    deck.apply(page, workspace.brightness)
        .context("Could not update the deck display")?;
    plugins.show(page);
    Ok(())
}

struct PluginDeck {
    hub: Option<Shared>,
    versions: [Option<(u64, bool, bool)>; DISPLAY_KEYS],
    pressed: [bool; DIALS],
    /// The plugin instance each held key or dial received its press on.
    held: HashMap<Control, Instance>,
}

impl PluginDeck {
    fn new(hub: Option<Shared>) -> Self {
        Self {
            hub,
            versions: [None; DISPLAY_KEYS],
            pressed: [false; DIALS],
            held: HashMap::new(),
        }
    }

    fn attach(&mut self) {
        if let Some(hub) = &self.hub {
            lock(hub).device(true);
        }
    }

    fn detach(&mut self) {
        if let Some(hub) = &self.hub {
            lock(hub).device(false);
        }
    }

    fn show(&mut self, layout: &PreparedLayout) {
        if let Some(hub) = &self.hub {
            lock(hub).set_visible(layout.instances());
        }
        self.versions.fill(None);
    }

    fn hide(&mut self) {
        if let Some(hub) = &self.hub {
            lock(hub).set_visible(Vec::new());
        }
    }

    /// Sends the release for every press a plugin is still waiting on.
    fn release_held(&mut self) {
        self.pressed.fill(false);
        let Some(hub) = &self.hub else {
            self.held.clear();
            return;
        };
        let mut hub = lock(hub);
        for (control, instance) in self.held.drain() {
            hub.input(&instance, release_event(control), serde_json::json!({}));
        }
    }

    fn dispatch(&mut self, input: &Input, action: &PreparedAction, notifier: &Notifier) -> bool {
        use serde_json::json;
        let control = input.control;
        if let Control::Dial(index, Direction::Press) = control {
            self.pressed[index] = input.down;
        }
        if !input.down {
            // A release goes to the plugin that got the press, even after a page change.
            // A release without a matching press is dropped.
            if let Some(instance) = self.held.remove(&control)
                && let Some(hub) = &self.hub
            {
                lock(hub).input(&instance, release_event(control), json!({}));
            }
            return true;
        }
        let PreparedAction::Plugin { action, context } = action else {
            return false;
        };
        let instance = Instance {
            action: action.clone(),
            context: context.clone(),
            control,
        };
        let (event, extras) = match control {
            Control::Button(_) => ("keyDown", json!({})),
            Control::Dial(_, Direction::Press) => ("dialDown", json!({})),
            Control::Dial(index, direction) => (
                "dialRotate",
                json!({
                    "ticks": if direction == Direction::Clockwise { 1 } else { -1 },
                    "pressed": self.pressed[index],
                }),
            ),
        };
        let delivered = self
            .hub
            .as_ref()
            .is_some_and(|hub| lock(hub).input(&instance, event, extras));
        if !delivered {
            notifier.send(Update::ActionError(format!(
                "Enable the plugin for {} from Plugins",
                control.label()
            )));
        } else if matches!(
            control,
            Control::Button(_) | Control::Dial(_, Direction::Press)
        ) {
            self.held.insert(control, instance);
        }
        true
    }

    fn refresh(&mut self, deck: &Deck, layout: &PreparedLayout, rotation: u16) -> Result<()> {
        let Some(hub) = &self.hub else {
            return Ok(());
        };
        for index in 0..DISPLAY_KEYS {
            let control = Control::Button(index);
            let PreparedAction::Plugin { action, context } = layout.action(control) else {
                continue;
            };
            if layout.custom_images[index] {
                continue;
            }
            let instance = Instance {
                action: action.clone(),
                context: context.clone(),
                control,
            };
            let (version, spec) = {
                let hub = lock(hub);
                let (revision, flashing) = hub.version(context, true);
                let available = hub.available(action);
                let version = (revision, flashing, available);
                if self.versions[index] == Some(version) {
                    continue;
                }
                (version, hub.render_spec(&instance, true))
            };
            let jpeg = match spec {
                Some(spec) => profile::encode_tile(
                    plugins::render(&spec, layout.colors[index], &layout.labels[index]),
                    rotation,
                )?,
                None if self.versions[index].is_none() => {
                    self.versions[index] = Some(version);
                    continue;
                }
                None => layout.images[index].clone(),
            };
            deck.set_key_image(index, &jpeg)?;
            self.versions[index] = Some(version);
        }
        Ok(())
    }
}

impl Drop for PluginDeck {
    fn drop(&mut self) {
        self.release_held();
        self.hide();
        self.detach();
    }
}

fn release_event(control: Control) -> &'static str {
    match control {
        Control::Dial(..) => "dialUp",
        Control::Button(_) => "keyUp",
    }
}

fn navigate(
    workspace: &PreparedWorkspace,
    scene: Scene,
    history: &mut Vec<Scene>,
    action: Navigation,
) -> Scene {
    let pages = &workspace.top_level[&scene.profile];
    match action {
        Navigation::Next | Navigation::Previous => {
            let index = pages.iter().position(|page| *page == scene);
            match (action, index) {
                (_, None) => pages[0],
                (Navigation::Next, Some(index)) => pages[(index + 1) % pages.len()],
                (_, Some(index)) => pages[(index + pages.len() - 1) % pages.len()],
            }
        }
        Navigation::Back => history.pop().unwrap_or(pages[0]),
        Navigation::Folder(id) => {
            let next = Scene { page: id, ..scene };
            if scene != next && history.len() < 32 {
                history.push(scene);
                next
            } else {
                scene
            }
        }
        Navigation::Page(id) => {
            history.clear();
            Scene { page: id, ..scene }
        }
        Navigation::Profile(id) => {
            history.clear();
            workspace.top_level[&id][0]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene(profile: u64, page: u64) -> Scene {
        Scene { profile, page }
    }

    fn workspace() -> PreparedWorkspace {
        PreparedWorkspace {
            agent_main_dial: MainDial::default(),
            easter_egg: false,
            brightness: 50,
            rotation: 0,
            home: scene(1, 10),
            pages: HashMap::new(),
            top_level: HashMap::from([
                (1, vec![scene(1, 10), scene(1, 11), scene(1, 12)]),
                (2, vec![scene(2, 20)]),
            ]),
            app_profiles: HashMap::new(),
        }
    }

    #[test]
    fn next_and_previous_wrap_within_the_profile() {
        let workspace = workspace();
        let mut history = Vec::new();
        let last = scene(1, 12);
        assert_eq!(
            navigate(&workspace, last, &mut history, Navigation::Next),
            scene(1, 10)
        );
        assert_eq!(
            navigate(&workspace, scene(1, 10), &mut history, Navigation::Previous),
            last
        );
        // A folder is not a top-level page, so Next starts from the first page.
        assert_eq!(
            navigate(&workspace, scene(1, 99), &mut history, Navigation::Next),
            scene(1, 10)
        );
    }

    #[test]
    fn folders_remember_the_way_back() {
        let workspace = workspace();
        let mut history = Vec::new();
        let folder = navigate(
            &workspace,
            scene(1, 11),
            &mut history,
            Navigation::Folder(50),
        );
        assert_eq!(folder, scene(1, 50));
        assert_eq!(
            navigate(&workspace, folder, &mut history, Navigation::Back),
            scene(1, 11)
        );
        // With no history, Back goes to the profile's first page.
        assert_eq!(
            navigate(&workspace, folder, &mut history, Navigation::Back),
            scene(1, 10)
        );
    }

    #[test]
    fn page_and_profile_jumps_clear_history() {
        let workspace = workspace();
        let mut history = vec![scene(1, 11)];
        assert_eq!(
            navigate(&workspace, scene(1, 50), &mut history, Navigation::Page(12)),
            scene(1, 12)
        );
        assert!(history.is_empty());
        history.push(scene(1, 11));
        assert_eq!(
            navigate(
                &workspace,
                scene(1, 12),
                &mut history,
                Navigation::Profile(2)
            ),
            scene(2, 20)
        );
        assert!(history.is_empty());
    }

    #[test]
    fn folder_history_is_bounded() {
        let workspace = workspace();
        let mut history = vec![scene(1, 10); 32];
        assert_eq!(
            navigate(
                &workspace,
                scene(1, 11),
                &mut history,
                Navigation::Folder(50)
            ),
            scene(1, 11)
        );
        assert_eq!(history.len(), 32);
    }

    #[test]
    fn cancelled_multi_action_stops_during_a_delay() {
        let started = Instant::now();
        let steps = vec![
            PreparedAction::Delay(Duration::from_secs(30)),
            PreparedAction::None,
        ];
        let checks = std::cell::Cell::new(0);
        let cancelled = || {
            checks.set(checks.get() + 1);
            checks.get() > 2
        };
        run_steps(steps, cancelled).unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn failing_step_ends_the_multi_action() {
        let steps = vec![
            PreparedAction::Navigate(Navigation::Next),
            PreparedAction::Delay(Duration::from_secs(30)),
        ];
        let started = Instant::now();
        assert!(run_steps(steps, || false).is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
