use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::mpsc::TryRecvError,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, ensure};
use eframe::egui::{
    self, Align, Color32, Context, FontId, Id, Layout, Pos2, Rect, Response, RichText, Sense,
    Stroke, StrokeKind, TextureHandle, Ui, Vec2, pos2, vec2,
};

use crate::{
    actions::{Action, ActionKind},
    agent_layout::MainDial,
    claude,
    codex::{self, Activity},
    deck_model::AKP03E,
    grok,
    marketplace::Marketplace,
    plugins::{CatalogItem, Instance, PluginHost, server::lock},
    profile::{self, Control, DISPLAY_KEYS, TILE_COLORS},
    runtime::{
        Active, Connection, ModeKind as View, ModeState, ModeUpdate, Request, Session, Update,
    },
    theme::{
        self, BODY, CANVAS, CAPTION, CONTROL_COMPACT_HEIGHT, DETAIL, DIM, DIVIDER, DialogAction,
        ERROR, FIELD_HEIGHT, FOOTER_MARGIN, GAP, GOLD, INSET, LINE, MINT, MUTED, PAGE_CHIP_WIDTH,
        PAGE_INSET, PALETTE_MARGIN, PANEL, PREVIEW_BORDER, PREVIEW_INSET, PREVIEW_RADIUS,
        PREVIEW_SIZE, SECTION_GAP, Symbol, TEXT, TIGHT, TOOL_WINDOW_WIDTH, TOOLBAR_MARGIN,
        TOOLBAR_TITLE, WELL,
    },
    workspace::{Page, Scene, Workspace},
};

mod claude_page;
mod codex_page;
mod deck_face;
mod dialogs;
mod grok_page;
mod inspector;
mod mode_editor;
mod mode_page;
use deck_face::{DeckFace, FaceControl};
use inspector::action_fields;
mod mcp_api;
mod mcp_settings;
mod plugin_window;
mod settings;
mod workspace_editor;

const GROUPS: [(&str, &[ActionKind], Symbol); 6] = [
    ("Multi Action", &[ActionKind::MultiAction], Symbol::Stack),
    (
        "Navigation",
        &[
            ActionKind::NextPage,
            ActionKind::PreviousPage,
            ActionKind::GoToPage,
            ActionKind::OpenFolder,
            ActionKind::Back,
        ],
        Symbol::Navigate,
    ),
    ("Media", &ActionKind::MEDIA, Symbol::Media),
    ("DeckX", &[ActionKind::SwitchProfile], Symbol::Plugins),
    ("System", &ActionKind::SYSTEM, Symbol::System),
    ("ChatGPT & Codex", &ActionKind::DESKTOP, Symbol::Open),
];

#[derive(Clone)]
enum Drag {
    Action(ActionKind),
    Control(Control),
    Plugin(String),
}

#[derive(Clone)]
struct CopiedControl {
    action: Action,
    display: Option<profile::Button>,
}

#[derive(Clone, Copy)]
enum Dialog {
    NewProfile,
    NewPage,
    NewFolder,
    RenameProfile,
    RenamePage,
    DeleteProfile,
    DeletePage,
}

impl Dialog {
    fn title(self) -> &'static str {
        match self {
            Self::NewProfile => "New profile",
            Self::NewPage => "New page",
            Self::NewFolder => "New folder",
            Self::RenameProfile => "Rename profile",
            Self::RenamePage => "Rename page or folder",
            Self::DeleteProfile => "Remove profile?",
            Self::DeletePage => "Remove page or folder?",
        }
    }
    fn removes(self) -> bool {
        matches!(self, Self::DeleteProfile | Self::DeletePage)
    }
}

struct ScenePrompt {
    dialog: Dialog,
    target: Scene,
    name: String,
    error: Option<String>,
    focus_name: bool,
}

#[derive(Default)]
struct Targets {
    pages: Vec<(u64, String)>,
    folders: Vec<(u64, String)>,
    profiles: Vec<(u64, String)>,
}

struct KeyPreview {
    scene: Scene,
    index: usize,
    button: profile::Button,
    texture: Option<TextureHandle>,
}

struct PluginPreview {
    context: String,
    version: (u64, bool),
    label: String,
    color: [u8; 3],
    texture: TextureHandle,
}

/// An inspector texture for a key photo, valid for one tile color and file version.
struct PhotoTexture {
    color: [u8; 3],
    version: Option<(Option<std::time::SystemTime>, u64)>,
    texture: Option<TextureHandle>,
}

struct GrokPreview {
    pixels: std::sync::Arc<image::RgbaImage>,
    texture: TextureHandle,
}

pub struct DeckX {
    path: Result<PathBuf>,
    /// The workspace file exists but could not be read. `saved` is a stand-in default.
    unreadable: bool,
    saved: Workspace,
    draft: Workspace,
    scene: Scene,
    live: Option<Scene>,
    live_brightness: Option<u8>,
    live_codex: Option<(codex::KeyStyle, usize)>,
    easter_egg_visible: bool,
    mcp: Option<crate::mcp::AppEndpoint>,
    selected: Control,
    session: Option<Session>,
    recent: Option<(Scene, Control, Instant)>,
    input_count: u64,
    message: String,
    error: bool,
    query: String,
    clipboard: Option<CopiedControl>,
    photos: HashMap<PathBuf, PhotoTexture>,
    preview: Option<KeyPreview>,
    dialog: Option<ScenePrompt>,
    settings: bool,
    settings_tab: mcp_settings::Tab,
    settings_error: Option<String>,
    startup: crate::startup::State,
    tray: Option<crate::tray::Tray>,
    mcp_settings: mcp_settings::State,
    plugins: Option<PluginHost>,
    plugin_error: Option<String>,
    plugin_notice: Option<String>,
    plugin_window: bool,
    plugin_enable: Option<String>,
    marketplace: Marketplace,
    marketplace_tab: bool,
    plugin_previews: [Option<PluginPreview>; DISPLAY_KEYS],
    grok_previews: [Option<GrokPreview>; DISPLAY_KEYS],
    native_theme: bool,
    codex: Activity,
    mode: ModeState,
    grok_agent: mode_editor::AgentState<grok::Bindings>,
    codex_agent: mode_editor::AgentState<codex::Layout>,
    claude_agent: mode_editor::AgentState<claude::Layout>,
    usage: crate::usage::Monitor,
    usage_details: bool,
    codex_catalog: Vec<codex::RecentChat>,
    codex_last_refresh: Option<Instant>,
    /// Why live status or the chat list is unavailable, refreshed with the chat list.
    codex_error: Option<String>,
    /// The brightness shown while its slider is dragged. It is saved when the drag ends.
    brightness_drag: Option<u8>,
    /// When a Grok avatar last showed a new frame.
    grok_frame_at: Option<Instant>,
    /// A plugin package being extracted for import.
    plugin_import: Option<std::sync::mpsc::Receiver<Result<crate::plugins::Staged>>>,
}

impl DeckX {
    pub fn new(
        context: &eframe::CreationContext<'_>,
        connect_on_start: bool,
        view: View,
        startup: crate::startup::State,
    ) -> Self {
        let ctx = &context.egui_ctx;
        let mut app = Self::load(ctx, profile::path(), Activity::start(ctx.clone()));
        app.usage = crate::usage::Monitor::start(ctx.clone());
        app.startup = startup;
        app.tray = crate::tray::Tray::attach(context);
        match PluginHost::new() {
            Ok(host) => app.plugins = Some(host),
            Err(error) => app.plugin_error = Some(format!("{error:#}")),
        }
        app.set_view(view);
        app.start_saved_mcp(ctx);
        if connect_on_start
            && !app.error
            && let Err(error) = app.connect(ctx)
        {
            app.report(format!("{error:#}"), true);
        }
        app
    }

    fn load(ctx: &Context, path: Result<PathBuf>, codex: Activity) -> Self {
        theme::install(ctx);
        let loaded = match &path {
            Ok(path) => Workspace::load(path),
            Err(error) => Err(anyhow::anyhow!("{error:#}")),
        };
        let (saved, message, error) = match loaded {
            Ok(saved) => (saved, "Deck disconnected".into(), false),
            Err(error) => (Workspace::default(), format!("{error:#}"), true),
        };
        let unreadable = error && path.is_ok();
        let codex_agent = mode_editor::AgentState::load(&path);
        let grok_agent = mode_editor::AgentState::load(&path);
        let claude_agent = mode_editor::AgentState::load(&path);
        Self {
            path,
            unreadable,
            draft: saved.clone(),
            scene: saved.home,
            saved,
            live: None,
            live_brightness: None,
            live_codex: None,
            easter_egg_visible: false,
            mcp: None,
            selected: Control::Button(0),
            session: None,
            recent: None,
            input_count: 0,
            message,
            error,
            query: String::new(),
            clipboard: None,
            photos: HashMap::new(),
            preview: None,
            dialog: None,
            settings: false,
            settings_tab: mcp_settings::Tab::General,
            settings_error: None,
            startup: crate::startup::State::default(),
            tray: None,
            mcp_settings: mcp_settings::State::default(),
            plugins: None,
            plugin_error: None,
            plugin_notice: None,
            plugin_window: false,
            plugin_enable: None,
            marketplace: Marketplace::default(),
            marketplace_tab: false,
            plugin_previews: std::array::from_fn(|_| None),
            grok_previews: std::array::from_fn(|_| None),
            native_theme: false,
            codex,
            mode: ModeState::default(),
            grok_agent,
            codex_agent,
            claude_agent,
            usage: crate::usage::Monitor::default(),
            usage_details: false,
            codex_catalog: Vec::new(),
            codex_last_refresh: None,
            codex_error: None,
            brightness_drag: None,
            grok_frame_at: None,
            plugin_import: None,
        }
    }

    fn page(&self) -> &Page {
        self.draft.page(self.scene).expect("editor has a page")
    }

    fn page_mut(&mut self) -> &mut Page {
        self.draft.page_mut(self.scene).expect("editor has a page")
    }

    fn report(&mut self, message: impl Into<String>, error: bool) {
        self.message = message.into();
        self.error = error;
    }

    fn connect(&mut self, ctx: &Context) -> Result<()> {
        let mut prepared = self.saved.prepare()?;
        if prepared.pages.contains_key(&self.scene) {
            prepared.home = self.scene;
        }
        self.session = Some(Session::start(
            prepared,
            ctx.clone(),
            self.plugins.as_ref().map(PluginHost::hub),
            self.codex.clone(),
            self.mode.clone(),
            self.usage.readings.clone(),
        )?);
        self.report("Opening the AKP03E…", false);
        Ok(())
    }

    fn save(&mut self) -> Result<()> {
        let mut next = self.draft.clone();
        next.home = self.scene;
        self.save_workspace(next)
    }

    fn can_save(&self) -> bool {
        self.session.as_ref().is_none_or(|session| {
            // While reconnecting, the runtime applies a save once the deck returns.
            matches!(
                session.connection,
                Connection::Open | Connection::Reconnecting
            )
        })
    }

    fn save_workspace(&mut self, next: Workspace) -> Result<()> {
        let prepared = next.prepare()?;
        let path = self
            .path
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error:#}"))?;
        let mut kept = None;
        if self.unreadable {
            // The editor holds a stand-in workspace. Never write it over the user's file.
            ensure!(
                Workspace::load(path).is_err(),
                "Your saved workspace can be read again. Restart DeckX to edit it. Nothing was saved"
            );
            if path.exists() {
                kept = Some(Workspace::set_aside(path)?);
            }
            self.unreadable = false;
        }
        next.replace(path)?;
        if self.scene != next.home {
            self.scene = next.home;
            self.selected = Control::Button(0);
        }
        self.preview = None;
        self.plugin_previews.fill_with(|| None);
        self.saved = next.clone();
        self.draft = next;
        if let Some(session) = &self.session {
            session
                .requests
                .send(Request::Apply(prepared))
                .map_err(|_| anyhow::anyhow!("Workspace saved. Reconnect the deck to apply it"))?;
            self.report("Applying workspace…", false);
        } else {
            self.report("Workspace saved", false);
        }
        if let Some(kept) = kept {
            let name = kept.file_name().unwrap_or_default().to_string_lossy();
            self.report(
                format!("Workspace saved. The unreadable workspace was kept as {name}"),
                false,
            );
        }
        Ok(())
    }

    /// Shows a brightness on the deck without saving it.
    fn preview_brightness(&mut self, brightness: u8) {
        self.brightness_drag = Some(brightness);
        if let Some(session) = &self.session
            && matches!(
                session.connection,
                Connection::Open | Connection::Reconnecting
            )
        {
            let _ = session.requests.send(Request::Brightness(brightness));
        }
    }

    fn save_brightness(&mut self, brightness: u8) -> Result<()> {
        self.brightness_drag = None;
        ensure!(brightness <= 100, "Brightness must be between 0 and 100");
        self.save_workspace_setting(|workspace| workspace.brightness = brightness)?;
        if self.draft.brightness == self.saved.brightness {
            self.draft.brightness = brightness;
        }
        self.saved.brightness = brightness;
        if let Some(session) = &self.session
            && !matches!(session.connection, Connection::Closing)
        {
            session
                .requests
                .send(Request::Brightness(brightness))
                .map_err(|_| anyhow::anyhow!("Brightness saved. Reconnect the deck to apply it"))?;
        }
        Ok(())
    }

    fn save_agent_main_dial(&mut self, behavior: MainDial) -> Result<()> {
        self.save_workspace_setting(|workspace| workspace.agent_main_dial = behavior)?;
        if self.draft.agent_main_dial == self.saved.agent_main_dial {
            self.draft.agent_main_dial = behavior;
        }
        self.saved.agent_main_dial = behavior;
        if let Some(session) = &self.session
            && !matches!(session.connection, Connection::Closing)
        {
            session
                .requests
                .send(Request::AgentMainDial(behavior))
                .map_err(|_| {
                    anyhow::anyhow!("Main dial setting saved. Reconnect the deck to apply it")
                })?;
        }
        Ok(())
    }

    /// Persists one setting while keeping any uncommitted workspace edits in the draft.
    fn save_workspace_setting(&self, change: impl FnOnce(&mut Workspace)) -> Result<()> {
        let path = self
            .path
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error:#}"))?;
        let mut next = Workspace::load(path)?;
        change(&mut next);
        next.save(path)
    }

    fn select_scene(&mut self, scene: Scene) {
        self.scene = scene;
        self.selected = Control::Button(0);
        if self.saved.page(scene).is_some()
            && let Some(session) = &self.session
            && matches!(session.connection, Connection::Open)
        {
            let _ = session.requests.send(Request::Select(scene));
        }
    }

    fn read_updates(&mut self) {
        while let Some(session) = &mut self.session {
            match session.updates.try_recv() {
                Ok(Update::Connected(scene)) => {
                    session.connection = Connection::Open;
                    self.live = Some(scene);
                    self.report("Deck connected", false);
                }
                Ok(Update::Reconnecting(error)) => {
                    session.connection = Connection::Reconnecting;
                    self.live = None;
                    self.live_codex = None;
                    self.easter_egg_visible = false;
                    self.recent = None;
                    self.report(
                        format!("Lost the deck ({error}). Reconnecting when it is available…"),
                        true,
                    );
                }
                Ok(Update::Applied(scene)) => {
                    self.live = Some(scene);
                    self.report("Workspace saved and applied", false);
                }
                Ok(Update::Brightness(brightness)) => self.live_brightness = Some(brightness),
                Ok(Update::Changed(scene)) => {
                    if self.live == Some(self.scene)
                        && self.draft == self.saved
                        && self.draft.page(scene).is_some()
                    {
                        self.scene = scene;
                    }
                    self.live = Some(scene);
                    self.report("Page active", false);
                }
                Ok(Update::Input(control)) => {
                    self.input_count += 1;
                    if let Some(scene) = self.live {
                        self.recent = Some((scene, control, Instant::now()));
                    }
                }
                Ok(Update::EasterEgg(visible)) => self.easter_egg_visible = visible,
                Ok(Update::Mode(generation, update)) => self.apply_mode_update(generation, update),
                Ok(Update::ActionError(error)) => self.report(error, true),
                Ok(Update::Finished(result)) => {
                    self.session = None;
                    self.live = None;
                    self.live_brightness = None;
                    self.live_codex = None;
                    self.easter_egg_visible = false;
                    self.recent = None;
                    match result {
                        Ok(()) => self.report("Deck disconnected", false),
                        Err(error) => self.report(format!("{error:#}"), true),
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    self.session = None;
                    self.live = None;
                    self.live_brightness = None;
                    self.live_codex = None;
                    self.easter_egg_visible = false;
                    self.recent = None;
                    self.report("Deck connection ended. Select Connect to retry.", true);
                }
                Err(TryRecvError::Empty) => break,
            }
        }
    }

    fn toolbar(&mut self, ctx: &Context) {
        egui::TopBottomPanel::top("app-toolbar")
            .frame(
                theme::frame(PANEL, 0)
                    .inner_margin(TOOLBAR_MARGIN)
                    .stroke(Stroke::new(1.0, LINE)),
            )
            .show(ctx, |ui| {
                let (bar, _) = ui
                    .allocate_exact_size(vec2(ui.available_width(), FIELD_HEIGHT), Sense::hover());
                let widths = [82.0, 56.0, 76.0, 62.0];
                let gap = TIGHT;
                let modes_width = widths.iter().sum::<f32>() + gap * (widths.len() - 1) as f32;
                let modes = Rect::from_center_size(bar.center(), vec2(modes_width, bar.height()));
                let mut identity = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt("device-name")
                        .max_rect(Rect::from_min_max(
                            bar.min,
                            pos2(modes.left() - INSET, bar.bottom()),
                        ))
                        .layout(Layout::left_to_right(Align::Center)),
                );
                identity.label(RichText::new(AKP03E.name).font(theme::bold(TOOLBAR_TITLE)));

                let mut navigation = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt("app-modes")
                        .max_rect(modes)
                        .layout(Layout::left_to_right(Align::Center)),
                );
                navigation.spacing_mut().item_spacing.x = gap;
                let current = self.view();
                for (view, width) in View::ORDER.into_iter().zip(widths) {
                    let selected = view == current;
                    let response = theme::tab(&mut navigation, view.label(), width, selected);
                    if response.clicked() && !selected {
                        self.set_view(view);
                    }
                }

                let mut actions = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt("device-actions")
                        .max_rect(Rect::from_min_max(
                            pos2(modes.right() + INSET, bar.top()),
                            bar.max,
                        ))
                        .layout(Layout::right_to_left(Align::Center)),
                );
                actions.spacing_mut().item_spacing.x = TIGHT;
                if theme::icon_button(&mut actions, Symbol::Settings, "Settings").clicked() {
                    self.settings = !self.settings;
                    if self.settings {
                        self.plugin_window = false;
                        self.plugin_enable = None;
                        self.settings_error = None;
                        self.startup.refresh(true);
                        self.mcp_settings.refresh_registration();
                    }
                }
                if theme::icon_button(&mut actions, Symbol::Plugins, "Plugins").clicked() {
                    self.plugin_window = !self.plugin_window;
                    self.settings = false;
                    self.plugin_enable = None;
                }
                let (action, available) = self.connection_control();
                if actions
                    .add_enabled_ui(available, |ui| theme::icon_button(ui, Symbol::Plug, action))
                    .inner
                    .clicked()
                {
                    self.toggle_connection(ctx);
                }
                let (label, color) = match self.session.as_ref().map(|session| session.connection) {
                    Some(Connection::Open) => ("Connected", MINT),
                    Some(Connection::Opening) => ("Connecting…", MUTED),
                    Some(Connection::Reconnecting) => ("Reconnecting…", GOLD),
                    Some(Connection::Closing) => ("Disconnecting…", MUTED),
                    None => ("Disconnected", MUTED),
                };
                actions.label(RichText::new(label).size(DETAIL).color(color));
            });
    }

    fn profile_picker(&mut self, ui: &mut Ui) {
        let name = self.draft.profile(self.scene.profile).unwrap().name.clone();
        let profiles: Vec<_> = self
            .draft
            .profiles
            .iter()
            .map(|p| (p.first_page(), p.name.clone()))
            .collect();
        theme::select(ui, "profiles", &name, 152.0, |ui| {
            for (scene, name) in &profiles {
                if theme::option(ui, name, self.scene.profile == scene.profile) {
                    self.select_scene(*scene);
                }
            }
            ui.separator();
            self.menu_dialog(ui, Dialog::NewProfile, "New profile…");
            self.menu_dialog(ui, Dialog::RenameProfile, "Rename profile…");
            if profiles.len() > 1 {
                self.menu_dialog(ui, Dialog::DeleteProfile, "Remove profile…");
            }
        });
    }

    fn connection_control(&self) -> (&'static str, bool) {
        match self.session.as_ref().map(|session| session.connection) {
            None => ("Connect", true),
            Some(Connection::Open | Connection::Reconnecting) => ("Disconnect", true),
            Some(Connection::Opening) => ("Connecting…", false),
            Some(Connection::Closing) => ("Disconnecting…", false),
        }
    }

    fn toggle_connection(&mut self, ctx: &Context) {
        if let Some(session) = &mut self.session {
            let _ = session.requests.send(Request::Disconnect);
            session.connection = Connection::Closing;
        } else if let Err(error) = self.connect(ctx) {
            self.report(format!("{error:#}"), true);
        }
    }

    fn view(&self) -> View {
        self.mode.active.kind()
    }

    fn apply_mode_update(&mut self, generation: u64, update: ModeUpdate) {
        if generation != self.mode.generation {
            return;
        }
        match (update, &mut self.mode.active) {
            (ModeUpdate::Exited, _) => self.leave_modes(),
            (ModeUpdate::CycleAgentMode(direction), active)
                if self.saved.agent_main_dial == MainDial::SwitchModes =>
            {
                if let Some(next) = active.kind().cycle_agent(direction) {
                    self.set_view(next);
                }
            }
            (ModeUpdate::CodexRendered(style, written), Active::Codex(_)) => {
                self.live_codex = Some((style, written));
            }
            (ModeUpdate::CodexSelected(slot), Active::Codex(mode)) => {
                mode.selected = slot.filter(|&index| mode.layout.agents[index].is_some());
            }
            (ModeUpdate::GrokSelected(id, highlight), Active::Grok(mode)) => {
                mode.refresh();
                mode.select_id(id.as_deref());
                mode.highlight_selection = highlight && mode.selected.is_some();
            }
            (ModeUpdate::ClaudeSelected(selection), Active::Claude(mode)) => {
                mode.set_selection(selection)
            }
            (ModeUpdate::ShowUsage, active) => self.usage_details = active.usage_key().is_some(),
            _ => {}
        }
    }

    /// Switches between the workspace and the agent modes. All mode cleanup happens here, and
    /// the device gets a single request, so it never redraws the workspace in between.
    fn set_view(&mut self, next: View) {
        if next == self.view() {
            return;
        }
        self.leave_modes();
        self.mode.generation = self.mode.generation.wrapping_add(1);
        self.mode.active = match next {
            View::Workspace => Active::Workspace,
            View::Codex => {
                self.codex_last_refresh = Some(Instant::now());
                self.codex_catalog = codex::recent_chats();
                let layout = self.codex_agent.saved.resolve(&self.codex_catalog);
                self.codex.track(layout.chat_ids());
                let mut mode = codex::Mode::default();
                mode.set_layout(layout);
                Active::Codex(Box::new(mode))
            }
            View::Grok => Active::Grok(grok::Mode::new(self.grok_agent.saved.clone())),
            View::Claude => Active::Claude(claude::Mode::new(self.claude_agent.saved.clone())),
        };
        self.sync_usage();
        if let Some(session) = &self.session {
            let _ = session.requests.send(Request::SetMode(self.mode.clone()));
        }
    }

    fn sync_usage(&mut self) {
        let usage = self.mode.active.usage_key();
        self.usage.track(usage.map(|(_, provider)| provider));
        if usage.is_none() {
            self.usage_details = false;
        }
    }

    /// Clears agent mode state in the editor. The device side is updated separately.
    fn leave_modes(&mut self) {
        // The mode page closes before its slider reports the end of a drag.
        if let Some(brightness) = self.brightness_drag
            && let Err(error) = self.save_brightness(brightness)
        {
            self.report(format!("{error:#}"), true);
        }
        self.mode.active = Active::Workspace;
        self.sync_usage();
        self.codex_agent.editor = None;
        self.live_codex = None;
        self.codex.track(Default::default());
        self.grok_agent.editor = None;
        self.claude_agent.editor = None;
        self.grok_previews.fill_with(|| None);
    }

    /// Uploads new Grok avatar frames. Returns whether any frame changed.
    fn refresh_grok_previews(&mut self, ctx: &Context) -> bool {
        let Active::Grok(mode) = &mut self.mode.active else {
            return false;
        };
        mode.refresh();
        let mut changed = false;
        for (index, cached) in self.grok_previews.iter_mut().enumerate() {
            let Some(pixels) = mode.avatar(index) else {
                changed |= cached.take().is_some();
                continue;
            };
            if cached
                .as_ref()
                .is_some_and(|entry| std::sync::Arc::ptr_eq(&entry.pixels, pixels))
            {
                continue;
            }
            changed = true;
            let color = egui::ColorImage::from_rgba_unmultiplied(
                [pixels.width() as usize, pixels.height() as usize],
                pixels.as_raw(),
            );
            if let Some(entry) = cached {
                entry.texture.set(color, egui::TextureOptions::LINEAR);
                entry.pixels = pixels.clone();
            } else {
                *cached = Some(GrokPreview {
                    pixels: pixels.clone(),
                    texture: ctx.load_texture(
                        format!("grok-avatar-{index}"),
                        color,
                        egui::TextureOptions::LINEAR,
                    ),
                });
            }
        }
        changed
    }

    fn save_grok_bindings(&mut self, bindings: grok::Bindings) -> Result<()> {
        if self.grok_agent.save(&self.path, bindings)?
            && let Active::Grok(mode) = &mut self.mode.active
        {
            mode.set_bindings(self.grok_agent.saved.clone());
        }
        Ok(())
    }

    fn refresh_codex_chats(&mut self) {
        if self
            .codex_last_refresh
            .is_some_and(|at| at.elapsed() < Duration::from_secs(2))
        {
            return;
        }
        self.codex_last_refresh = Some(Instant::now());
        self.codex_catalog = codex::recent_chats();
        self.codex_error = codex::recent_chats_error().or_else(|| self.codex.error());
        self.apply_codex_layout();
    }

    fn save_codex_layout(&mut self, layout: codex::Layout) -> Result<()> {
        if self.codex_agent.save(&self.path, layout)? {
            self.apply_codex_layout();
        }
        Ok(())
    }

    fn apply_codex_layout(&mut self) {
        let Active::Codex(mode) = &mut self.mode.active else {
            return;
        };
        let layout = self.codex_agent.saved.resolve(&self.codex_catalog);
        if mode.layout != layout {
            self.codex.track(layout.chat_ids());
            mode.set_layout(layout.clone());
            if let Some(session) = &self.session {
                let _ = session
                    .requests
                    .send(Request::CodexLayout(Box::new(layout)));
            }
        }
    }

    fn footer(&mut self, ctx: &Context) {
        let workspace = self.view() == View::Workspace;
        egui::TopBottomPanel::bottom("app-footer")
            .frame(theme::frame(CANVAS, 0).inner_margin(FOOTER_MARGIN))
            .show(ctx, |ui| {
                theme::right_aligned_row(ui, FIELD_HEIGHT, |ui| {
                    ui.label(
                        RichText::new(format!("v{}", crate::APP_VERSION))
                            .size(DETAIL)
                            .color(MUTED),
                    );
                    let dirty = self.draft != self.saved;
                    if workspace {
                        ui.add_space(GAP);
                        // Saving also makes the selected page the deck's home page.
                        let available = self.can_save() && (dirty || self.scene != self.saved.home);
                        let save = theme::action_button(ui, "Save workspace", dirty, available)
                            .on_hover_text("Ctrl+S");
                        let shortcut = !self.settings
                            && !self.plugin_window
                            && self.dialog.is_none()
                            && ctx.input_mut(|input| {
                                input.consume_key(egui::Modifiers::CTRL, egui::Key::S)
                            });
                        if (save.clicked() || available && shortcut)
                            && let Err(error) = self.save()
                        {
                            self.report(format!("{error:#}"), true);
                        }
                        if dirty && theme::action_button(ui, "Revert edits", false, true).clicked()
                        {
                            self.draft = self.saved.clone();
                            if self.draft.page(self.scene).is_none() {
                                self.select_scene(self.saved.home);
                            }
                            self.report("Edits reverted", false);
                        }
                    }
                    let (status, color) = self.footer_status(workspace);
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        theme::clipped_text(ui, &status, DETAIL, color);
                    });
                });
            });
    }

    fn footer_status(&self, workspace: bool) -> (String, Color32) {
        if self.error {
            return (self.message.clone(), ERROR);
        }
        if self.easter_egg_visible {
            return ("Sydney Sweeney".into(), MINT);
        }
        if workspace && self.draft != self.saved {
            return ("Unsaved changes".into(), GOLD);
        }
        let (mut status, color) = if workspace && self.live.is_some_and(|scene| scene != self.scene)
        {
            (format!("Preview · {}", self.message), MUTED)
        } else if self.live.is_some() {
            (self.message.clone(), MINT)
        } else {
            (self.message.clone(), MUTED)
        };
        if let Some((_, control, _)) = self.recent {
            status.push_str(&format!("   ·   Last input: {}", control.label()));
        }
        (status, color)
    }
}

fn editor_width(ui: &Ui) -> f32 {
    (ui.available_width() - 76.0).min(624.0)
}

impl eframe::App for DeckX {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(session) = &mut self.session {
            session.shut_down(Duration::from_millis(1500));
        }
    }

    fn update(&mut self, ctx: &Context, frame: &mut eframe::Frame) {
        let in_tray = self.tray.as_ref().is_some_and(crate::tray::Tray::is_hidden);
        let plugin_keys = !in_tray
            && self.live.is_some()
            && self.view() == View::Workspace
            && self
                .page()
                .layout
                .buttons
                .iter()
                .any(|button| button.action.kind == crate::actions::ActionKind::Plugin);
        self.poll_plugin_import(ctx);
        if let Some(plugins) = &mut self.plugins {
            plugins.poll();
            self.marketplace.poll(plugins);
            // Plugins draw their keys without notifying the UI, so poll while any are shown.
            if !in_tray && (self.plugin_window || plugin_keys) {
                ctx.request_repaint_after(Duration::from_millis(150));
            }
        }
        self.read_updates();
        self.poll_mcp(ctx);
        if let Some(tray) = &self.tray {
            tray.update(
                self.saved.minimize_to_tray,
                self.session
                    .as_ref()
                    .is_some_and(|session| matches!(session.connection, Connection::Open)),
            );
            if tray.is_hidden() {
                return;
            }
        }
        if self.view() == View::Grok && self.refresh_grok_previews(ctx) {
            self.grok_frame_at = Some(Instant::now());
        }
        let grok_animating = self
            .grok_frame_at
            .is_some_and(|at| at.elapsed() < Duration::from_secs(1));
        if !self.native_theme {
            self.native_theme = theme::window_caption(frame);
        }
        self.toolbar(ctx);
        self.footer(ctx);
        match self.view() {
            View::Codex => {
                self.refresh_codex_chats();
                ctx.request_repaint_after(Duration::from_secs(2));
                mode_page::show::<codex::Mode>(self, ctx, |target| target.run());
            }
            View::Grok => {
                // Follow avatar animation frames, and check for roster changes less often.
                ctx.request_repaint_after(if grok_animating {
                    grok::FRAME_INTERVAL
                } else {
                    Duration::from_millis(500)
                });
                mode_page::show::<grok::Mode>(self, ctx, |target| target.run());
            }
            View::Claude => mode_page::show::<claude::Mode>(self, ctx, |target| target.run()),
            View::Workspace => {
                self.palette(ctx);
                self.inspector(ctx);
                self.center(ctx);
                self.prompt(ctx);
            }
        }
        if self.settings {
            self.settings(ctx);
        }
        if self.plugin_window {
            self.plugin_manager(ctx);
        }
    }
}

/// Whether a plugin action can be bound to a control. Key actions also work on the side
/// buttons, which have no display.
fn plugin_fits(item: &CatalogItem, control: Control) -> bool {
    match control {
        Control::Button(_) => item.key,
        Control::Dial(..) => item.dial,
    }
}

fn glyph(kind: ActionKind) -> &'static str {
    use ActionKind::*;
    match kind {
        None => "+",
        Shortcut => "⌨",
        TypeText | CodexDraft => "T",
        OpenApp | CodexLaunch | CodexNewChat | CodexOpenChat => "↗",
        OpenFile | OpenFolder => "▰",
        OpenWebsite => "◎",
        VolumeUp => "+",
        VolumeDown => "−",
        Mute => "×",
        PlayPause => "▶",
        PreviousTrack | PreviousPage | Back => "‹",
        NextTrack | NextPage => "›",
        GoToPage => "→",
        SwitchProfile => "⇄",
        MultiAction => "≡",
        Delay => "◷",
        Plugin => "⊞",
    }
}

#[cfg(test)]
mod visual_checks;
