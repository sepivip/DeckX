use std::{
    sync::{Arc, RwLock, Weak},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use image::RgbaImage;
use serde::{Deserialize, Serialize};
use uiautomation::{
    UIAutomation,
    core::UIElement,
    patterns::UIInvokePattern,
    types::{ControlType, TreeScope, UIProperty},
};
use windows::{
    Win32::UI::WindowsAndMessaging::{FindWindowW, IsIconic},
    core::w,
};

use crate::profile::{Control, DISPLAY_KEYS};

mod automation;
mod avatar;
mod avatar_source;
mod bindings;
pub use bindings::{Binding, Bindings};
pub(crate) mod key;
mod roster;
mod usage;
pub(crate) use key::key_tile;
pub(crate) use usage::read as read_usage;

const POLL: Duration = Duration::from_secs(2);
pub const FRAME_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bot {
    pub id: String,
    #[serde(default, skip_serializing)]
    pub name: String,
    #[serde(default, skip_serializing)]
    title: String,
    #[serde(default)]
    avatar_shape: String,
    #[serde(default)]
    avatar_color: String,
    #[serde(default, skip_serializing)]
    has_unread: bool,
    #[serde(default, skip_serializing)]
    unread_thread_count: u32,
    #[serde(default, skip_serializing)]
    created_at: Option<u64>,
    #[serde(default, skip_serializing)]
    local_activity_at: Option<u64>,
    #[serde(default, skip_serializing)]
    last_activity_at: Option<u64>,
    #[serde(default, skip_serializing)]
    is_hidden_from_sidebar: bool,
    #[serde(default, skip_serializing)]
    is_removed_from_sidebar: bool,
    #[serde(skip)]
    missing: bool,
}

impl Bot {
    pub fn label(&self) -> &str {
        [&self.title, &self.name]
            .into_iter()
            .find(|text| !text.trim().is_empty())
            .unwrap_or(&self.id)
    }

    pub fn unread(&self) -> bool {
        self.has_unread || self.unread_thread_count > 0
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    keys: [Option<Bot>; DISPLAY_KEYS],
    main_id: Option<String>,
}

impl Layout {
    pub fn bot(&self, index: usize) -> Option<&Bot> {
        self.keys.get(index)?.as_ref()
    }

    pub fn is_main(&self, index: usize) -> bool {
        self.bot(index)
            .is_some_and(|bot| Some(bot.id.as_str()) == self.main_id.as_deref())
    }

    pub fn iter(&self) -> impl Iterator<Item = (usize, &Bot)> {
        self.keys
            .iter()
            .enumerate()
            .filter_map(|(index, bot)| bot.as_ref().map(|bot| (index, bot)))
    }

    fn resolve(catalog: &Catalog, bindings: &Bindings) -> Self {
        let main_id = catalog.main_id.as_deref();
        let pinned: std::collections::HashSet<_> = (0..DISPLAY_KEYS)
            .filter_map(|index| match bindings.binding(Control::Button(index)) {
                Binding::PinnedBot { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        let mut recent: Vec<_> = catalog
            .bots
            .iter()
            .filter(|bot| Some(bot.id.as_str()) != main_id && !pinned.contains(bot.id.as_str()))
            .collect();
        recent.sort_by_key(|bot| {
            std::cmp::Reverse(
                bot.created_at
                    .unwrap_or_default()
                    .max(bot.local_activity_at.unwrap_or_default())
                    .max(bot.last_activity_at.unwrap_or_default()),
            )
        });
        let mut recent = recent.into_iter();
        let find = |id: &str| catalog.bots.iter().find(|bot| bot.id == id).cloned();
        Self {
            keys: std::array::from_fn(|index| {
                match bindings.active_binding(Control::Button(index))? {
                    Binding::MainBot => main_id.and_then(find),
                    Binding::RecentBot => recent.next().cloned(),
                    Binding::PinnedBot { id, label } => Some(find(id).unwrap_or_else(|| Bot {
                        id: id.clone(),
                        name: label.clone(),
                        missing: true,
                        ..Bot::default()
                    })),
                    _ => None,
                }
            }),
            main_id: catalog.main_id.clone(),
        }
    }

    #[cfg(test)]
    fn classic(bots: &[Bot], main_id: Option<&str>) -> Self {
        Self::resolve(
            &Catalog {
                bots: Arc::from(bots),
                main_id: main_id.map(str::to_owned),
            },
            &Bindings::default(),
        )
    }
}

#[derive(Default)]
#[cfg_attr(test, derive(Clone))]
struct Catalog {
    bots: Arc<[Bot]>,
    main_id: Option<String>,
}

type Frames = [Option<Arc<RgbaImage>>; DISPLAY_KEYS];

#[derive(Default)]
#[cfg_attr(test, derive(Clone))]
struct Snapshot {
    catalog: Catalog,
    bindings: Arc<Bindings>,
    layout: Arc<Layout>,
    frames: Frames,
    roster_error: Option<String>,
    command_error: Option<String>,
    avatar_error: Option<String>,
    /// Woken when the layout changes so an idle avatar thread can start.
    renderer: Option<std::thread::Thread>,
}

impl Snapshot {
    fn resolve_layout(&mut self) {
        let layout = Layout::resolve(&self.catalog, &self.bindings);
        if *self.layout != layout {
            for index in 0..DISPLAY_KEYS {
                let old = self.layout.bot(index);
                let new = layout.bot(index);
                if !old.zip(new).is_some_and(|(old, new)| {
                    old.id == new.id
                        && old.avatar_shape == new.avatar_shape
                        && old.avatar_color == new.avatar_color
                        && old.missing == new.missing
                }) {
                    self.frames[index] = None;
                }
            }
            self.layout = Arc::new(layout);
            if let Some(renderer) = &self.renderer {
                renderer.unpark();
            }
        }
    }
}

#[derive(Clone)]
pub struct Mode {
    pub layout: Arc<Layout>,
    pub catalog: Arc<[Bot]>,
    pub bindings: Arc<Bindings>,
    pub selected: Option<usize>,
    pub highlight_selection: bool,
    /// The roster error, or else the last failed New Bot or Search command.
    pub error: Option<String>,
    pub avatar_error: Option<String>,
    snapshot: Arc<RwLock<Snapshot>>,
    frames: Frames,
}

impl Mode {
    #[cfg(test)]
    pub(crate) fn independent_fixture(&self) -> Self {
        let snapshot = self.snapshot.read().unwrap().clone();
        Self {
            snapshot: Arc::new(RwLock::new(snapshot)),
            ..self.clone()
        }
    }

    pub fn new(bindings: Bindings) -> Self {
        let bindings = Arc::new(bindings);
        let mut initial = Snapshot {
            bindings: Arc::clone(&bindings),
            ..Snapshot::default()
        };
        initial.resolve_layout();
        let layout = Arc::clone(&initial.layout);
        let snapshot = Arc::new(RwLock::new(initial));
        let worker = Arc::downgrade(&snapshot);
        let roster_worker = worker.clone();
        std::thread::spawn(move || watch_roster(roster_worker));
        let renderer = std::thread::spawn(move || render_bots(worker));
        if let Ok(mut snapshot) = snapshot.write() {
            snapshot.renderer = Some(renderer.thread().clone());
        }
        Self {
            layout,
            bindings,
            catalog: Arc::default(),
            selected: None,
            highlight_selection: false,
            error: None,
            avatar_error: None,
            snapshot,
            frames: Default::default(),
        }
    }

    pub fn set_bindings(&mut self, bindings: Bindings) {
        if let Ok(mut snapshot) = self.snapshot.write() {
            snapshot.bindings = Arc::new(bindings);
            snapshot.resolve_layout();
        }
        self.refresh();
    }

    pub fn refresh(&mut self) {
        let Ok(snapshot) = self.snapshot.read() else {
            return;
        };
        let changed = !Arc::ptr_eq(&self.layout, &snapshot.layout);
        let selected = if changed {
            let id = self.selected_id();
            self.layout = Arc::clone(&snapshot.layout);
            id
        } else {
            None
        };
        self.catalog = Arc::clone(&snapshot.catalog.bots);
        self.bindings = Arc::clone(&snapshot.bindings);
        self.frames.clone_from(&snapshot.frames);
        self.error = snapshot
            .roster_error
            .clone()
            .or_else(|| snapshot.command_error.clone());
        self.avatar_error.clone_from(&snapshot.avatar_error);
        drop(snapshot);
        if changed {
            self.select_id(selected.as_deref());
        }
    }

    pub fn avatar(&self, index: usize) -> Option<&Arc<RgbaImage>> {
        self.frames.get(index)?.as_ref()
    }

    pub fn selected_id(&self) -> Option<String> {
        self.selected
            .and_then(|index| self.layout.bot(index))
            .map(|bot| bot.id.clone())
    }

    pub fn select_id(&mut self, id: Option<&str>) {
        self.selected = id.and_then(|id| {
            self.layout
                .iter()
                .find_map(|(index, bot)| (bot.id == id).then_some(index))
        });
        self.highlight_selection &= self.selected.is_some();
    }

    pub fn selection(&self) -> (Option<String>, bool) {
        (self.selected_id(), self.highlight_selection)
    }

    /// Handles a deck control. `open_agent` opens a bot and `run` runs a bound action, so the
    /// caller can do both away from the HID thread.
    pub fn input_with(
        &mut self,
        control: Control,
        open_agent: impl FnOnce(&str) -> Result<()>,
        run: impl FnOnce(crate::actions::PreparedAction) -> Result<()>,
    ) -> Result<()> {
        if let Control::Button(index) = control
            && self.layout.bot(index).is_some()
        {
            return self.activate_key(index, open_agent);
        }
        let Some(binding) = self.bindings.active_binding(control) else {
            return Ok(());
        };
        match binding {
            Binding::NewBot => self.invoke("New Bot", control),
            Binding::Search => self.invoke("Search", control),
            Binding::OpenSelected => {
                self.highlight_selection = false;
                self.selected
                    .and_then(|index| self.layout.bot(index))
                    .map_or(Ok(()), |bot| open_agent(&bot.id))
            }
            Binding::PinnedBot { id, .. } => open_agent(id),
            Binding::PreviousBot => {
                self.select(false);
                Ok(())
            }
            Binding::NextBot => {
                self.select(true);
                Ok(())
            }
            Binding::Action(action) => run(action.prepare()?),
            Binding::None | Binding::MainBot | Binding::RecentBot | Binding::ExitMode => Ok(()),
        }
    }

    pub fn activate_key(
        &mut self,
        index: usize,
        open_agent: impl FnOnce(&str) -> Result<()>,
    ) -> Result<()> {
        if let Some(bot) = self.layout.bot(index) {
            self.selected = Some(index);
            self.highlight_selection = false;
            open_agent(&bot.id)?;
        }
        Ok(())
    }

    /// Queues a Grok Bot command on the accessibility thread so deck input
    /// keeps flowing. A failure shows up in `error` after the next refresh.
    fn invoke(&self, name: &'static str, control: Control) -> Result<()> {
        let snapshot = Arc::downgrade(&self.snapshot);
        automation::spawn(move |automation| {
            let result = automation.and_then(|automation| invoke_named(automation, name));
            if let Some(snapshot) = snapshot.upgrade()
                && let Ok(mut snapshot) = snapshot.write()
            {
                snapshot.command_error = result
                    .err()
                    .map(|error| format!("{}: {error:#}", control.label()));
            }
        })
    }

    fn select(&mut self, forward: bool) {
        self.selected = crate::agent_layout::next_key(self.selected, forward, |index| {
            self.layout.bot(index).is_some()
        });
        self.highlight_selection = self.selected.is_some();
    }
}

fn watch_roster(target: Weak<RwLock<Snapshot>>) {
    let mut roster = roster::Roster::default();
    while let Some(target) = target.upgrade() {
        let result = roster.refresh();
        if let Ok(mut snapshot) = target.write() {
            match result {
                Ok(catalog) => {
                    snapshot.catalog = catalog;
                    snapshot.resolve_layout();
                    snapshot.roster_error = None;
                }
                Err(error) => snapshot.roster_error = Some(format!("{error:#}")),
            }
        }
        drop(target);
        std::thread::sleep(POLL);
    }
}

fn render_bots(target: Weak<RwLock<Snapshot>>) {
    let mut last_frame = Instant::now();
    let mut engine = avatar::Engine::default();
    let mut layout = Arc::new(Layout::default());
    let mut inputs = String::from("[]");
    let mut slots = Vec::new();
    let mut previous: Frames = Default::default();
    while let Some(target) = target.upgrade() {
        let now = Instant::now();
        let Ok(snapshot) = target.read() else { return };
        if !Arc::ptr_eq(&snapshot.layout, &layout) {
            layout = Arc::clone(&snapshot.layout);
            inputs = avatar::avatar_inputs(
                layout
                    .iter()
                    .filter_map(|(_, bot)| (!bot.missing).then_some(bot)),
            );
            slots = layout
                .iter()
                .filter_map(|(index, bot)| (!bot.missing).then_some(index))
                .collect();
        }
        drop(snapshot);

        let result = if slots.is_empty() {
            None
        } else {
            match engine.renderer(now) {
                Ok(Some(renderer)) => {
                    let seconds = now.duration_since(last_frame).as_secs_f64();
                    let result = render_frames(renderer, &inputs, seconds, &slots, &previous);
                    if result.is_ok() {
                        engine.rendered();
                    } else {
                        engine.failed(now);
                    }
                    Some(result)
                }
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            }
        };
        if let Some(result) = result
            && let Ok(mut snapshot) = target.write()
            && Arc::ptr_eq(&snapshot.layout, &layout)
        {
            match result {
                Ok(frames) => {
                    previous.clone_from(&frames);
                    snapshot.frames = frames;
                    snapshot.avatar_error = engine.load_error().map(str::to_owned);
                }
                Err(error) => snapshot.avatar_error = Some(format!("{error:#}")),
            }
        }
        last_frame = now;
        drop(target);
        if slots.is_empty() || !engine.is_loaded() {
            // Nothing animates, so wait for a layout change or a Grok Bot update.
            std::thread::park_timeout(POLL);
        } else {
            std::thread::sleep(FRAME_INTERVAL.saturating_sub(now.elapsed()));
        }
    }
}

fn render_frames(
    renderer: &mut avatar::Renderer,
    inputs: &str,
    seconds: f64,
    slots: &[usize],
    previous: &Frames,
) -> Result<Frames> {
    let svgs = renderer.render(inputs, seconds)?;
    anyhow::ensure!(
        svgs.len() == slots.len(),
        "Grok returned the wrong number of avatar frames"
    );
    let mut next = Frames::default();
    for (&index, svg) in slots.iter().zip(&svgs) {
        let frame = avatar::rasterize(svg)?;
        next[index] = if previous[index].as_ref().is_some_and(|old| old == &frame) {
            previous[index].clone()
        } else {
            Some(frame)
        };
    }
    Ok(next)
}

pub fn launch() -> Result<()> {
    crate::actions::open_grok_bot().context("Could not launch Grok Bot")
}

fn window(automation: &UIAutomation) -> Result<UIElement> {
    let hwnd =
        unsafe { FindWindowW(None, w!("Grok Bot")) }.context("Grok Bot window unavailable")?;
    automation
        .element_from_handle(hwnd.into())
        .context("Could not read the Grok Bot accessibility window")
}

fn find_command(automation: &UIAutomation, name: &str) -> Result<UIElement> {
    let named = automation.create_property_condition(UIProperty::Name, name.into(), None)?;
    let button = automation.create_property_condition(
        UIProperty::ControlType,
        (ControlType::Button as i32).into(),
        None,
    )?;
    let condition = automation.create_and_condition(named, button)?;
    window(automation)?
        .find_first(TreeScope::Descendants, &condition)
        .with_context(|| format!("Could not find Grok Bot control {name}"))
}

fn invoke_named(automation: &UIAutomation, name: &str) -> Result<()> {
    if unsafe { FindWindowW(None, w!("Grok Bot")) }
        .map_or(true, |hwnd| unsafe { IsIconic(hwnd).as_bool() })
    {
        launch()?;
    }
    let deadline = Instant::now() + Duration::from_millis(2500);
    loop {
        match find_command(automation, name) {
            Ok(button) => return Ok(button.get_pattern::<UIInvokePattern>()?.invoke()?),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

#[cfg(test)]
pub(crate) fn visual_fixture(populated: bool, render_avatars: bool) -> Mode {
    let bots: Vec<Bot> = if populated {
        let fixtures = [
            ("Boss", "blob", "blue"),
            ("Build and review the deployment", "flower", "green"),
            ("Plan", "star", "blue"),
            ("Docs", "heart", "magenta"),
            ("LongUnbrokenNameWithoutSpaces", "squircle", "violet"),
            ("Notes", "blob", "orange"),
            ("Archive", "blob", "yellow"),
        ];
        fixtures
            .into_iter()
            .enumerate()
            .map(|(index, (name, shape, color))| {
                serde_json::from_value(serde_json::json!({
                    "id": format!("visual-fixture-{index}"), "name": name, "avatarShape": shape,
                    "avatarColor": color, "hasUnread": index == 0 || index == 3,
                }))
                .unwrap()
            })
            .collect()
    } else {
        Vec::new()
    };
    let layout = Arc::new(Layout::classic(
        &bots,
        populated.then_some("visual-fixture-0"),
    ));
    let mut frames = Frames::default();
    if populated && render_avatars {
        let mut engine = avatar::Renderer::open().unwrap();
        for (index, svg) in engine
            .render(
                &avatar::avatar_inputs(layout.iter().map(|(_, bot)| bot)),
                0.08,
            )
            .unwrap()
            .iter()
            .enumerate()
        {
            frames[index] = Some(avatar::rasterize(svg).unwrap());
        }
    }
    let bots: Arc<[Bot]> = bots.into();
    Mode {
        layout: Arc::clone(&layout),
        selected: populated.then_some(0),
        highlight_selection: populated,
        bindings: Arc::default(),
        catalog: Arc::clone(&bots),
        error: None,
        avatar_error: None,
        snapshot: Arc::new(RwLock::new(Snapshot {
            catalog: Catalog {
                bots,
                main_id: populated.then(|| "visual-fixture-0".to_owned()),
            },
            layout,
            frames: frames.clone(),
            ..Default::default()
        })),
        frames,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Direction;

    #[test]
    fn classic_layout_reserves_main_and_orders_recent_bots_by_activity() {
        let bots: Vec<Bot> = serde_json::from_value(serde_json::json!([
            {"id":"old", "name":"Old", "lastActivityAt":1},
            {"id":"main", "name":"Main", "lastActivityAt":12},
            {"id":"newest", "name":"Newest", "localActivityAt":90},
            {"id":"new", "name":"New", "createdAt":80},
            {"id":"older", "name":"Older", "lastActivityAt":30},
            {"id":"recent", "name":"Recent", "lastActivityAt":60},
            {"id":"middle", "name":"Middle", "lastActivityAt":40}
        ]))
        .unwrap();
        let layout = Layout::classic(&bots, Some("main"));
        assert_eq!(
            layout
                .iter()
                .map(|(_, bot)| bot.id.as_str())
                .collect::<Vec<_>>(),
            ["main", "newest", "new", "recent", "middle", "older"],
        );
        assert!(layout.is_main(0));
        assert!(Layout::classic(&bots, None).bot(0).is_none());

        let mut mode = visual_fixture(false, false);
        mode.layout = Arc::new(layout);
        let mut opened = Vec::new();
        mode.activate_key(1, |id| {
            opened.push(id.to_owned());
            Ok(())
        })
        .unwrap();
        assert_eq!(opened, ["newest"]);
        assert_eq!(mode.selected, Some(1));
    }

    #[test]
    fn saved_layout_remaps_live_modes_and_opening_clears_dial_highlight() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grok-layout.json");
        let defaults = Bindings::load(&path).unwrap();
        let mut custom = defaults.clone();
        let side = Control::Button(7);
        let dial = Control::Dial(1, Direction::Press);
        let pinned = Binding::PinnedBot {
            id: "archive".to_owned(),
            label: "Archive".to_owned(),
        };
        *custom.binding_mut(Control::Button(1)) = pinned.clone();
        *custom.binding_mut(side) = pinned;
        *custom.binding_mut(dial) = Binding::OpenSelected;
        custom.save(&path, &defaults).unwrap();
        let saved = Bindings::load(&path).unwrap();
        assert_eq!(saved, custom);
        *custom.binding_mut(side) = Binding::RecentBot;
        assert!(custom.save(&path, &saved).is_err());
        assert_eq!(Bindings::load(&path).unwrap(), saved);

        let mut mode = visual_fixture(false, false);
        mode.snapshot.write().unwrap().catalog = Catalog {
            bots: Arc::from(["main", "archive", "recent"].map(|id| Bot {
                id: id.into(),
                name: id.into(),
                ..Bot::default()
            })),
            main_id: Some("main".into()),
        };
        let mut deck = mode.clone();
        mode.set_bindings(saved);
        deck.refresh();
        assert_eq!(deck.layout.bot(1).unwrap().id, "archive");
        assert_eq!(deck.layout.bot(2).unwrap().id, "recent");
        assert_eq!(deck.layout.iter().count(), 3);
        let mut opened = Vec::new();
        let mut open = |id: &str| {
            opened.push(id.to_owned());
            Ok(())
        };
        deck.input_with(side, &mut open, |action| action.run())
            .unwrap();
        let turn = Control::Dial(0, Direction::Clockwise);
        deck.input_with(turn, &mut open, |action| action.run())
            .unwrap();
        assert_eq!(deck.selection(), (Some("main".to_owned()), true));
        deck.input_with(dial, &mut open, |action| action.run())
            .unwrap();
        assert_eq!(deck.selection(), (Some("main".to_owned()), false));
        deck.input_with(turn, &mut open, |action| action.run())
            .unwrap();
        assert_eq!(deck.selection(), (Some("archive".to_owned()), true));
        deck.input_with(Control::Button(1), &mut open, |action| action.run())
            .unwrap();
        assert_eq!(deck.selection(), (Some("archive".to_owned()), false));
        assert_eq!(opened, ["archive", "main", "archive"]);
    }

    #[test]
    fn accessibility_runs_on_its_own_thread_for_sta_callers() {
        use windows::Win32::System::Com::{
            COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize,
        };

        std::thread::spawn(|| {
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
                .ok()
                .unwrap();
            let caller = std::thread::current().id();
            let result = automation::run(move |automation| {
                automation.get_root_element()?;
                Ok(std::thread::current().id() != caller)
            });
            unsafe { CoUninitialize() };
            assert!(result.as_ref().is_ok_and(|&other| other), "{result:?}");
        })
        .join()
        .unwrap();
    }

    #[test]
    fn failed_commands_surface_as_mode_errors() {
        let mut mode = visual_fixture(false, false);
        mode.snapshot.write().unwrap().command_error = Some("Side button 1: failed".into());
        mode.refresh();
        assert_eq!(mode.error.as_deref(), Some("Side button 1: failed"));
        mode.snapshot.write().unwrap().roster_error = Some("Open Grok Bot".into());
        mode.refresh();
        assert_eq!(mode.error.as_deref(), Some("Open Grok Bot"));
    }

    #[test]
    #[ignore = "requires the installed Grok Bot window"]
    fn installed_command_controls_are_invokable() {
        automation::run(|automation| {
            for name in ["New Bot", "Search"] {
                find_command(automation, name)?.get_pattern::<UIInvokePattern>()?;
            }
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn recognizes_only_roster_cache_keys() {
        let name = |key: &str| {
            std::path::PathBuf::from(format!(
                "{}.blob",
                data_encoding::BASE32_NOPAD
                    .encode(key.as_bytes())
                    .to_ascii_lowercase()
            ))
        };
        assert!(roster::is_roster_file(&name(
            "sand.client.slice.user.roster.last-roster"
        )));
        assert!(!roster::is_roster_file(&name(
            "sand.client.slice.user.roster.last-agent"
        )));
    }

    #[test]
    #[ignore = "requires a locally cached Grok Bot roster"]
    fn cached_roster_renders_animated_keys() {
        let mut mode = Mode::new(Bindings::default());
        let start = Instant::now();
        let mut first = None;
        loop {
            mode.refresh();
            assert!(mode.error.is_none(), "{:?}", mode.error);
            assert!(mode.avatar_error.is_none(), "{:?}", mode.avatar_error);
            if let Some(frame) = mode.avatar(1) {
                if first.as_ref().is_some_and(|initial| initial != frame) {
                    break;
                }
                first.get_or_insert_with(|| frame.clone());
            }
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "No animated roster frames arrived"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn fallback_key_keeps_unread_dot() {
        let bot: Bot =
            serde_json::from_str(r#"{"id":"test","name":"Bot","unreadThreadCount":1}"#).unwrap();
        let tile = key_tile(Some(&bot), None, false, false);
        let side = crate::deck_model::LCD_PIXELS;
        assert_eq!(tile.dimensions(), (side, side));
        let [x, y] = key::unread_center(false).map(|ratio| (ratio * tile.width() as f32) as u32);
        assert_eq!(tile.get_pixel(x, y).0, key::UNREAD_COLOR);
    }

    #[test]
    fn main_badge_and_unread_dot_are_both_on_the_lcd() {
        let bot: Bot = serde_json::from_value(serde_json::json!({
            "id": "main", "name": "Main", "hasUnread": true
        }))
        .unwrap();
        let tile = key_tile(Some(&bot), None, false, true);
        let at = |[x, y]: [f32; 2]| {
            tile.get_pixel(
                (x * tile.width() as f32) as u32,
                (y * tile.height() as f32) as u32,
            )
            .0
        };
        assert_eq!(at(key::STAR_CENTER), [255; 3]);
        assert_eq!(at(key::unread_center(false)), key::UNREAD_COLOR);
        assert!(tile.pixels().any(|pixel| pixel.0 == key::STAR_BACKGROUND));
    }
}
