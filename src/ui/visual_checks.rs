use std::{
    num::NonZeroU32,
    path::{Path, PathBuf},
    sync::Arc,
};

use eframe::{
    egui::{
        Context, Event, FullOutput, Key, MouseWheelUnit, PointerButton, Pos2, RawInput, Rect,
        Shape, ViewportId, pos2, vec2,
    },
    egui_glow::Painter,
    glow::{self, HasContext},
};
use glutin::{
    api::egl::display::Display,
    config::{ConfigSurfaceTypes, ConfigTemplateBuilder},
    context::{ContextApi, ContextAttributesBuilder, Version},
    prelude::*,
    surface::{PbufferSurface, SurfaceAttributesBuilder},
};
use raw_window_handle::{RawDisplayHandle, WindowsDisplayHandle};
use uuid::Uuid;

use super::{
    DeckX,
    deck_face::DeckFace,
    mcp_settings,
    mode_editor::Editor,
    mode_page::{self, OpenTarget},
};
use crate::{
    actions::{Action, ActionKind},
    agent_layout::MainDial,
    claude, codex,
    deck_model::AKP03E,
    grok,
    marketplace::Marketplace,
    plugins::PluginHost,
    profile::{Control, Direction},
    runtime::{Active, ModeKind, ModeUpdate},
    workspace::Workspace,
};

const LCD: Control = Control::Button(1);
const SIDE: Control = Control::Button(7);
const DIAL: Control = Control::Dial(1, Direction::Press);

#[derive(Clone, Copy, Debug)]
enum View {
    Codex,
    Grok,
    Claude,
    Workspace,
}

impl View {
    fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Grok => "grok",
            Self::Claude => "claude",
            Self::Workspace => "workspace",
        }
    }

    fn open_label(self) -> &'static str {
        match self {
            Self::Codex | Self::Claude => "Open chat",
            Self::Grok => "Open bot",
            Self::Workspace => unreachable!(),
        }
    }

    fn side_default(self) -> &'static str {
        match self {
            Self::Codex | Self::Claude => "Open selected chat",
            Self::Grok => "Open selected bot",
            Self::Workspace => unreachable!(),
        }
    }
}

/// Sends input to the real egui widgets without a deck, a desktop window,
/// user settings, or any live integrations.
struct Harness {
    app: DeckX,
    ctx: Context,
    view: View,
    screen: Rect,
    scale: f32,
    time: f64,
    frames: Vec<FullOutput>,
    opened: Vec<String>,
    mode_content_height: f32,
}

impl Harness {
    fn new(
        view: View,
        path: PathBuf,
        width: u32,
        height: u32,
        scale: f32,
        populated: bool,
        grok_fixture: &grok::Mode,
    ) -> Self {
        let ctx = Context::default();
        let mut app = DeckX::load(&ctx, Ok(path), codex::Activity::fixture());
        match view {
            View::Codex => {
                if populated {
                    app.codex_catalog = [
                        "Inbox",
                        "Build and review the deployment",
                        "Plan",
                        "Docs",
                        "Check prerequisites",
                        "Notes",
                        "Archive",
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(i, name)| {
                        serde_json::from_value(serde_json::json!({
                            "id": Uuid::from_u128(i as u128 + 1).to_string(),
                            "thread_name": name,
                            "updated_at": "2026-01-01T00:00:00Z",
                        }))
                        .unwrap()
                    })
                    .collect();
                }
                let mut mode = codex::Mode::default();
                mode.set_layout(app.codex_agent.saved.resolve(&app.codex_catalog));
                mode.selected = populated.then_some(0);
                app.mode.active = Active::Codex(Box::new(mode));
            }
            View::Grok => {
                app.mode.active = Active::Grok(if populated {
                    grok_fixture.independent_fixture()
                } else {
                    grok::visual_fixture(false, false)
                });
            }
            View::Claude => {
                if populated {
                    for (index, title) in [
                        "Review the Windows release checklist",
                        "Design review",
                        "Meeting notes",
                        "Design review follow-up",
                        "Hardware setup",
                        "Launch plan",
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        *app.claude_agent.saved.binding_mut(Control::Button(index)) =
                            claude::Binding::PinnedChat {
                                id: Uuid::from_u128(index as u128 + 1).to_string(),
                                title: title.into(),
                            };
                    }
                }
                app.mode.active = Active::Claude(claude::Mode::new(app.claude_agent.saved.clone()));
            }
            View::Workspace => {}
        }
        let mut harness = Self {
            app,
            ctx,
            view,
            scale,
            time: 0.0,
            frames: Vec::new(),
            opened: Vec::new(),
            mode_content_height: 0.0,
            screen: Rect::from_min_size(Pos2::ZERO, vec2(width as f32, height as f32)),
        };
        harness.settle();
        harness
    }

    fn frame(&mut self, events: Vec<Event>) {
        self.time += 0.075;
        let mut input = RawInput {
            screen_rect: Some(self.screen),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        let viewport = input.viewports.get_mut(&ViewportId::ROOT).unwrap();
        viewport.native_pixels_per_point = Some(self.scale);
        viewport.inner_rect = Some(self.screen);
        let frame = self.ctx.run(input, |ctx| {
            let app = &mut self.app;
            app.refresh_grok_previews(ctx);
            app.toolbar(ctx);
            app.footer(ctx);
            self.mode_content_height = ctx.available_rect().height();
            let open = |target: OpenTarget<'_>| {
                let id = match target {
                    OpenTarget::Chat(id) | OpenTarget::ClaudeChat(id) => id.to_string(),
                    OpenTarget::Bot(id) => String::from(id),
                };
                self.opened.push(id);
                Ok(())
            };
            match self.view {
                View::Codex => mode_page::show::<codex::Mode>(app, ctx, open),
                View::Grok => mode_page::show::<grok::Mode>(app, ctx, open),
                View::Claude => mode_page::show::<claude::Mode>(app, ctx, open),
                View::Workspace => {
                    app.palette(ctx);
                    app.inspector(ctx);
                    app.center(ctx);
                    app.prompt(ctx);
                }
            }
            if app.settings {
                app.settings(ctx);
            }
            if app.plugin_window {
                app.plugin_manager(ctx);
            }
        });
        self.frames.push(frame);
    }

    fn settle(&mut self) {
        for _ in 0..6 {
            self.frame(Vec::new());
        }
    }

    fn click(&mut self, point: Pos2, button: PointerButton) {
        self.frame(vec![Event::PointerMoved(point)]);
        for pressed in [true, false] {
            self.frame(vec![Event::PointerButton {
                pos: point,
                button,
                pressed,
                modifiers: Default::default(),
            }]);
        }
        self.settle();
    }

    fn click_control(&mut self, control: Control, button: PointerButton) {
        self.click(self.control_center(control), button);
    }

    fn control_center(&self, control: Control) -> Pos2 {
        let width = DeckFace::mode_width(&AKP03E, self.mode_content_height);
        let origin = pos2((self.screen.width() - width) / 2.0, 202.0);
        let face = Rect::from_min_size(origin, vec2(width, width / AKP03E.aspect()));
        DeckFace::at(&AKP03E, face)
            .controls()
            .find(|shape| shape.control() == control)
            .unwrap()
            .rect()
            .center()
    }

    fn label_position(&self, label: &str) -> Pos2 {
        let frame = self.frames.last().unwrap();
        frame
            .shapes
            .iter()
            .rev()
            .find_map(|clipped| {
                let Shape::Text(text) = &clipped.shape else {
                    return None;
                };
                let center = text.pos + text.galley.rect.center().to_vec2();
                (text.galley.text() == label && clipped.clip_rect.contains(center))
                    .then_some(center)
            })
            .unwrap_or_else(|| panic!("Missing {label:?} in {:?}", self.view))
    }

    fn click_label(&mut self, label: &str) {
        self.click(self.label_position(label), PointerButton::Primary);
    }

    fn setting_switch(&self, row: &str, value: &str) -> Pos2 {
        let label = self.label_position(row);
        self.frames
            .last()
            .unwrap()
            .shapes
            .iter()
            .find_map(|clipped| {
                let Shape::Text(text) = &clipped.shape else {
                    return None;
                };
                let center = text.pos + text.galley.rect.center().to_vec2();
                (text.galley.text() == value
                    && (center.y - label.y).abs() < 6.0
                    && clipped.clip_rect.contains(center))
                .then_some(center)
            })
            .unwrap_or_else(|| panic!("Missing {value:?} switch for {row:?}"))
    }

    fn standard_agent_keys(&self) -> &'static str {
        match self.view {
            View::Codex => "Most recent chats",
            View::Grok => "Main bot + recent bots",
            View::Claude => "6 pinned chats",
            View::Workspace => unreachable!(),
        }
    }

    fn open_agent_keys(&mut self, label: &str) {
        if self.screen.height() < 740.0 {
            self.scroll(self.screen.center(), -350.0);
        }
        self.click_label(label);
    }

    fn standard_main_dial(&self) -> &'static str {
        match self.view {
            View::Codex | View::Claude => "Chat selection",
            View::Grok => "Bot selection",
            View::Workspace => unreachable!(),
        }
    }

    fn open_main_dial(&mut self, label: &str) {
        self.scroll(self.screen.center(), -350.0);
        self.click_label(label);
    }

    fn show_main_dial(&mut self, state: &str) {
        if state == "dial-default" {
            self.scroll(self.screen.center(), -350.0);
            self.label_position(self.standard_main_dial());
            return;
        }
        self.open_main_dial(self.standard_main_dial());
        if state == "dial-menu" {
            return;
        }
        self.click_label("Switch modes");
        assert_eq!(self.app.saved.agent_main_dial, MainDial::SwitchModes);
        if state == "dial-hover" {
            self.scroll(self.screen.center(), 350.0);
            let point = self.control_center(Control::Dial(0, Direction::Clockwise));
            self.frame(vec![Event::PointerMoved(point)]);
            self.settle();
            self.settle();
            let next = self.app.view().cycle_agent(Direction::Clockwise).unwrap();
            self.label_position(&format!("Switch to {}", next.label()));
        } else {
            self.label_position("Turn to cycle Codex, Grok Bot, and Claude");
        }
    }

    fn show_usage(&mut self, state: &str) {
        assert!(self.app.mode.active.usage_key().is_none());
        self.open_agent_keys(self.standard_agent_keys());
        if state == "usage-menu" {
            return;
        }
        self.click_label("5 keys + usage");
        let (index, provider) = self.app.mode.active.usage_key().unwrap();
        let observed_at = crate::usage::now_ms();
        let (session_used, weekly_used) = match state {
            "usage-zero" => (0.0, 100.0),
            "usage-low" => (0.0, 93.0),
            "usage-quarter" => (0.0, 75.0),
            "usage-full" => (0.0, 0.0),
            _ => (32.0, 19.0),
        };
        let real = crate::usage::Report::Available(crate::usage::Reading {
            windows: [
                (provider == crate::usage::Provider::Claude).then(|| {
                    crate::usage::Window::from_used(session_used, Some(300), None).unwrap()
                }),
                crate::usage::Window::from_used(
                    weekly_used,
                    Some(10080),
                    (provider != crate::usage::Provider::Claude)
                        .then_some(observed_at + 135 * 60_000),
                ),
            ],
            observed_at: if provider == crate::usage::Provider::Claude {
                observed_at - 65 * 60_000
            } else {
                observed_at
            },
            expires_at: observed_at + 600_000,
        });
        let report = match state {
            "usage-unavailable" => crate::usage::Report::Unavailable,
            "usage-waiting" => crate::usage::Report::Waiting,
            _ => real,
        };
        self.app.usage.readings.fixture(provider, report);
        self.scroll(self.screen.center(), 350.0);
        if matches!(state, "usage-details" | "usage-unavailable") {
            self.click_control(Control::Button(index), PointerButton::Primary);
            assert!(self.app.usage_details);
        } else if state == "usage-hover" {
            self.frame(vec![Event::PointerMoved(
                self.control_center(Control::Button(index)),
            )]);
        }
        self.settle();
    }

    fn key(&mut self, key: Key) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: Default::default(),
            }]);
        }
        self.settle();
    }

    fn scroll(&mut self, point: Pos2, delta: f32) {
        self.frame(vec![
            Event::PointerMoved(point),
            Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: vec2(0.0, delta),
                modifiers: Default::default(),
            },
        ]);
        self.settle();
    }

    fn plugins(&mut self, populated: bool) {
        let root = self.app.path.as_ref().unwrap().parent().unwrap();
        let mut host = PluginHost::at(root.join("host")).unwrap();
        if populated {
            let plugin = root.join("com.deckx.fixture.sdPlugin");
            std::fs::create_dir_all(&plugin).unwrap();
            std::fs::write(
                plugin.join("manifest.json"),
                r#"{
                    "Name": "Example Plugin", "UUID": "com.deckx.fixture", "Author": "Example Publisher",
                    "Version": "1.2.0", "CodePath": "index.html",
                    "Actions": [{"Name": "Example Action", "UUID": "com.deckx.fixture.action"}]
                }"#,
            )
            .unwrap();
            std::fs::write(plugin.join("index.html"), "<title>Fixture</title>").unwrap();
            host.import(&plugin).unwrap();
        }
        self.app.plugins = Some(host);
        self.app.plugin_window = true;
    }

    fn workspace_state(&mut self, state: &str) {
        match state {
            "layout" => {}
            "filter" => self.app.query = "open".into(),
            "palette-menu" => self.click(
                pos2(self.screen.right() - 27.0, 76.0),
                PointerButton::Primary,
            ),
            "appearance-menu" => self.click_label("⌄"),
            "binding" | "binding-menu" => {
                self.app.assign(Control::Button(0), ActionKind::OpenWebsite);
                if state == "binding-menu" {
                    self.settle();
                    self.click_label("Open a website");
                }
            }
            "profile-menu" | "profile-dialog" => {
                self.click_label("Default");
                if state == "profile-dialog" {
                    self.click_label("New profile…");
                    self.frame(vec![Event::Text("Example profile".into())]);
                }
            }
            "settings-general"
            | "settings-startup-on"
            | "settings-startup-error"
            | "settings-rotation"
            | "settings-dirty"
            | "settings-mcp"
            | "settings-mcp-scrolled" => {
                self.app.settings = true;
                self.app.startup.enabled = state == "settings-startup-on";
                if state == "settings-startup-error" {
                    self.app.startup.error =
                        Some("Could not change Windows startup: Access is denied.".into());
                }
                if state.starts_with("settings-mcp") {
                    self.app.settings_tab = mcp_settings::Tab::Mcp;
                    self.app.mcp_settings = mcp_settings::State::setup_fixture();
                }
                self.settle();
                if matches!(state, "settings-rotation" | "settings-dirty") {
                    self.click_label("90°");
                    if state == "settings-dirty" {
                        self.click_label("180°");
                    }
                } else if state == "settings-mcp-scrolled" {
                    self.scroll(self.screen.center(), -240.0);
                    self.click_label("Manual setup");
                    self.scroll(self.screen.center(), -240.0);
                }
            }
            "settings-easter-egg-menu" | "settings-easter-egg-on" => {
                self.open_easter_egg("Off");
                if state == "settings-easter-egg-on" {
                    self.click_label("On");
                    assert!(self.app.saved.easter_egg);
                }
            }
            "settings-tray-off" | "settings-tray-on" => {
                self.app.settings = true;
                self.settle();
                if state == "settings-tray-on" {
                    let on = self.setting_switch("Minimize to tray", "On");
                    self.click(on, PointerButton::Primary);
                }
                assert_eq!(self.app.saved.minimize_to_tray, state == "settings-tray-on");
            }
            "plugins-empty"
            | "plugins-installed"
            | "plugins-enable"
            | "plugins-uninstall"
            | "plugins-unavailable" => {
                if state == "plugins-unavailable" {
                    self.app.plugin_window = true;
                } else {
                    self.plugins(state != "plugins-empty");
                    if state == "plugins-enable" {
                        self.settle();
                        self.click_label("Enable…");
                    } else if state == "plugins-uninstall" {
                        self.ctx.data_mut(|data| {
                            data.insert_temp(
                                eframe::egui::Id::new("uninstall-plugin"),
                                "com.deckx.fixture".to_owned(),
                            );
                        });
                    }
                }
            }
            state if state.starts_with("marketplace-") => {
                self.plugins(false);
                self.app.marketplace_tab = true;
                self.app.marketplace =
                    Marketplace::visual_fixture(if state == "marketplace-scrolled" {
                        "marketplace-results"
                    } else {
                        state
                    });
                if state == "marketplace-scrolled" {
                    self.settle();
                    self.scroll(self.screen.center(), -300.0);
                }
            }
            _ => unreachable!(),
        }
        self.settle();
    }

    fn open_easter_egg(&mut self, value: &str) {
        self.app.settings = true;
        self.settle();
        self.label_position("Easter egg");
        self.click_label(value);
    }

    fn editor_target(&self) -> Option<Option<Control>> {
        match self.view {
            View::Codex => self
                .app
                .codex_agent
                .editor
                .as_ref()
                .map(|editor| editor.target),
            View::Grok => self
                .app
                .grok_agent
                .editor
                .as_ref()
                .map(|editor| editor.target),
            View::Claude => self
                .app
                .claude_agent
                .editor
                .as_ref()
                .map(|editor| editor.target),
            View::Workspace => None,
        }
    }

    fn saved_label(&self, control: Control) -> String {
        let profile = self.app.path.as_ref().unwrap();
        match self.view {
            View::Codex => codex::Layout::load(&codex::Layout::path(profile))
                .unwrap()
                .binding(control)
                .label()
                .into(),
            View::Grok => grok::Bindings::load(&grok::Bindings::path(profile))
                .unwrap()
                .binding(control)
                .label()
                .into(),
            View::Claude => claude::Layout::load(&claude::Layout::path(profile))
                .unwrap()
                .binding(control)
                .label()
                .into(),
            View::Workspace => unreachable!(),
        }
    }

    fn pin_draft(&mut self) {
        match self.view {
            View::Codex => {
                let mut draft = self.app.codex_agent.saved.clone();
                let mut action = Action::from_kind(ActionKind::CodexOpenChat);
                action.value = self.app.codex_catalog[1].id.to_string();
                *draft.binding_mut(LCD) = codex::Binding::Action(action);
                self.app.codex_agent.editor = Some(Editor::new(Some(LCD), draft));
            }
            View::Grok => {
                let mut draft = self.app.grok_agent.saved.clone();
                let Active::Grok(mode) = &self.app.mode.active else {
                    unreachable!()
                };
                let bot = mode.layout.bot(1).unwrap();
                *draft.binding_mut(LCD) = grok::Binding::PinnedBot {
                    id: bot.id.clone(),
                    label: bot.label().into(),
                };
                self.app.grok_agent.editor = Some(Editor::new(Some(LCD), draft));
            }
            View::Claude => {
                let mut draft = self.app.claude_agent.saved.clone();
                *draft.binding_mut(LCD) = claude::Binding::PinnedChat {
                    id: format!("https://claude.ai/chat/{}", Uuid::from_u128(2)),
                    title: "Design review".into(),
                };
                self.app.claude_agent.editor = Some(Editor::new(Some(LCD), draft));
            }
            View::Workspace => unreachable!(),
        }
        self.settle();
    }
}

#[test]
fn usage_presets_save_per_mode_and_restore_the_previous_sixth_assignment() {
    let root = tempfile::tempdir().unwrap();
    for view in [View::Codex, View::Grok, View::Claude] {
        let profile = root.path().join(view.name()).join("profile.json");
        let mut harness = Harness::new(
            view,
            profile.clone(),
            840,
            740,
            1.0,
            true,
            &grok::visual_fixture(true, false),
        );
        let initial = match view {
            View::Codex => serde_json::to_value(&harness.app.codex_agent.saved),
            View::Grok => serde_json::to_value(&harness.app.grok_agent.saved),
            View::Claude => serde_json::to_value(&harness.app.claude_agent.saved),
            View::Workspace => unreachable!(),
        }
        .unwrap();
        harness.show_usage("usage-details");
        assert!(harness.editor_target().is_none());
        assert_eq!(harness.app.mode.active.usage_key().unwrap().0, 5);
        assert!(match &harness.app.mode.active {
            Active::Codex(mode) => mode.layout.agents[5].is_none(),
            Active::Grok(mode) => mode.layout.bot(5).is_none(),
            Active::Claude(mode) => mode.layout.pinned(5).is_none(),
            Active::Workspace => unreachable!(),
        });
        harness.label_position("81% left");
        let layout = profile.with_file_name(format!("{}-layout.json", view.name()));
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&layout).unwrap()).unwrap();
        assert_eq!(saved["preset"], "usage_limits");
        assert_eq!(saved["buttons"], initial["buttons"]);
        assert_eq!(saved["dials"], initial["dials"]);

        harness.click_label("Close");
        harness.open_agent_keys("5 keys + usage");
        harness.click_label(harness.standard_agent_keys());
        assert!(harness.app.mode.active.usage_key().is_none());
        let restored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&layout).unwrap()).unwrap();
        assert_eq!(restored, initial);
    }
}

#[test]
fn main_dial_switches_agent_modes_and_restores_saved_assignments() {
    use Direction::{Clockwise, Counterclockwise, Press};

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("profile.json");
    let fixture = grok::visual_fixture(true, false);
    let mut harness = Harness::new(View::Grok, path.clone(), 700, 580, 1.25, true, &fixture);
    let baseline = grok::Bindings::default();
    let mut bindings = baseline.clone();
    *bindings.binding_mut(Control::Dial(0, Clockwise)) = grok::Binding::ExitMode;
    let stored = grok::Bindings::path(&path);
    bindings.save(&stored, &baseline).unwrap();

    assert_eq!(harness.app.saved.agent_main_dial, MainDial::Assignments);
    harness.app.draft.brightness = 37;
    harness.show_main_dial("dial-switch");
    let loaded = Workspace::load(&path).unwrap();
    assert_eq!(loaded.agent_main_dial, MainDial::SwitchModes);
    assert_eq!(
        loaded.prepare().unwrap().agent_main_dial,
        MainDial::SwitchModes
    );
    assert_eq!(loaded.brightness, Workspace::default().brightness);
    assert_eq!(harness.app.draft.brightness, 37);
    assert_eq!(grok::Bindings::load(&stored).unwrap(), bindings);

    for (control, expected) in [
        (Control::Dial(0, Clockwise), Some(Clockwise)),
        (Control::Dial(0, Counterclockwise), Some(Counterclockwise)),
        (Control::Dial(0, Press), None),
        (Control::Dial(1, Clockwise), None),
        (Control::Button(0), None),
    ] {
        assert_eq!(loaded.agent_main_dial.mode_turn(control), expected);
    }

    for (turn, expected) in [
        (Clockwise, ModeKind::Claude),
        (Clockwise, ModeKind::Codex),
        (Clockwise, ModeKind::Grok),
        (Counterclockwise, ModeKind::Codex),
        (Counterclockwise, ModeKind::Claude),
        (Counterclockwise, ModeKind::Grok),
    ] {
        let generation = harness.app.mode.generation;
        harness
            .app
            .apply_mode_update(generation, ModeUpdate::CycleAgentMode(turn));
        assert_eq!(harness.app.view(), expected);
        assert_ne!(harness.app.mode.generation, generation);
        harness
            .app
            .apply_mode_update(generation, ModeUpdate::CycleAgentMode(turn));
        assert_eq!(harness.app.view(), expected);
    }
    harness.app.set_view(ModeKind::Workspace);
    harness.app.apply_mode_update(
        harness.app.mode.generation,
        ModeUpdate::CycleAgentMode(Clockwise),
    );
    assert_eq!(harness.app.view(), ModeKind::Workspace);

    let mut restored = Harness::new(View::Codex, path.clone(), 840, 740, 1.0, true, &fixture);
    restored.open_main_dial("Switch modes");
    restored.click_label("Chat selection");
    let saved = Workspace::load(&path).unwrap();
    assert_eq!(
        saved.agent_main_dial.mode_turn(Control::Dial(0, Clockwise)),
        None
    );
    assert_eq!(grok::Bindings::load(&stored).unwrap(), bindings);
}

#[test]
fn enabling_sydney_saves_the_option_without_committing_the_workspace_draft() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("profile.json");
    let fixture = grok::visual_fixture(false, false);
    let mut harness = Harness::new(
        View::Workspace,
        path.clone(),
        700,
        580,
        1.5,
        false,
        &fixture,
    );
    assert!(!harness.app.saved.easter_egg);
    harness.app.draft.rotation = 180;
    harness.workspace_state("settings-easter-egg-on");
    let saved = Workspace::load(&path).unwrap();
    assert!(saved.prepare().unwrap().easter_egg);
    assert_eq!(saved.rotation, Workspace::default().rotation);
    assert_eq!(harness.app.draft.rotation, 180);

    let mut reloaded = Harness::new(View::Grok, path.clone(), 840, 740, 1.0, false, &fixture);
    assert!(reloaded.app.saved.easter_egg);
    reloaded.open_easter_egg("On");
    reloaded.click_label("Off");
    assert!(!Workspace::load(&path).unwrap().easter_egg);
}

#[test]
fn tray_preference_survives_reload_without_saving_other_edits() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("profile.json");
    let fixture = grok::visual_fixture(false, false);
    let mut harness = Harness::new(
        View::Workspace,
        path.clone(),
        700,
        580,
        1.25,
        false,
        &fixture,
    );
    assert!(!harness.app.saved.minimize_to_tray);
    harness.app.draft.rotation = 180;
    harness.workspace_state("settings-tray-on");
    let saved = Workspace::load(&path).unwrap();
    assert!(saved.minimize_to_tray);
    assert_eq!(saved.rotation, Workspace::default().rotation);
    assert_eq!(harness.app.draft.rotation, 180);

    let mut reopened = Harness::new(View::Codex, path.clone(), 840, 740, 1.0, false, &fixture);
    assert!(reopened.app.saved.minimize_to_tray);
    reopened.app.settings = true;
    reopened.settle();
    let off = reopened.setting_switch("Minimize to tray", "Off");
    reopened.click(off, PointerButton::Primary);
    assert!(!Workspace::load(&path).unwrap().minimize_to_tray);
}

#[test]
fn mode_pages_share_click_edit_open_save_and_reset() {
    let root = tempfile::tempdir().unwrap();
    for (width, height) in [(840, 740), (700, 580)] {
        for view in [View::Codex, View::Grok, View::Claude] {
            let path = root
                .path()
                .join(format!("{}-{width}", view.name()))
                .join("profile.json");
            let mut harness = Harness::new(
                view,
                path,
                width,
                height,
                1.0,
                true,
                &grok::visual_fixture(true, false),
            );

            harness.click_control(LCD, PointerButton::Primary);
            assert_eq!(harness.editor_target(), Some(Some(LCD)));
            assert!(
                harness.opened.is_empty(),
                "Editing must not open the assigned item"
            );
            harness.click_label("Cancel");
            assert_eq!(harness.editor_target(), None);

            harness.click_control(LCD, PointerButton::Secondary);
            harness.click_label(view.open_label());
            let expected = match view {
                View::Codex | View::Claude => Uuid::from_u128(2).to_string(),
                View::Grok => "visual-fixture-1".into(),
                View::Workspace => unreachable!(),
            };
            assert_eq!(harness.opened, [expected]);
            assert_eq!(harness.editor_target(), None);
            if let Active::Grok(mode) = &harness.app.mode.active {
                assert!(!mode.highlight_selection);
            }

            harness.click_control(SIDE, PointerButton::Primary);
            assert_eq!(
                harness.editor_target(),
                Some(Some(SIDE)),
                "{view:?} at {width} × {height}"
            );
            harness.click_label(view.side_default());
            harness.click_label("Unassigned");
            harness.click_label("Cancel");
            assert_eq!(harness.saved_label(SIDE), view.side_default());

            harness.click_control(SIDE, PointerButton::Primary);
            harness.click_label(view.side_default());
            harness.click_label("Unassigned");
            harness.click_label("Save");
            assert_eq!(harness.editor_target(), None);
            assert_eq!(harness.saved_label(SIDE), "Unassigned");

            harness.click_control(SIDE, PointerButton::Primary);
            harness.click_label("Use default");
            harness.click_label("Save");
            assert_eq!(harness.saved_label(SIDE), view.side_default());

            harness.click_control(DIAL, PointerButton::Primary);
            assert_eq!(harness.editor_target(), Some(Some(DIAL)));
            harness.click_label("Turn right");
            assert_eq!(
                harness.editor_target(),
                Some(Some(Control::Dial(1, Direction::Clockwise)))
            );
            harness.click_label("Unassigned");
            harness.click_label(match view {
                View::Codex | View::Claude => "Select next chat",
                View::Grok => "Select next bot",
                View::Workspace => unreachable!(),
            });
            harness.click_label("Save");
            assert_ne!(
                harness.saved_label(Control::Dial(1, Direction::Clockwise)),
                "Unassigned"
            );

            harness.click_label("Reset layout…");
            assert_eq!(harness.editor_target(), Some(None));
            harness.click_label("Reset layout");
            assert_eq!(
                harness.saved_label(Control::Dial(1, Direction::Clockwise)),
                "Unassigned"
            );
            assert_eq!(harness.editor_target(), None);
        }
    }
}

#[test]
fn claude_pin_editor_validates_and_keeps_the_saved_assignment() {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("profile.json");
    let mut harness = Harness::new(
        View::Claude,
        profile.clone(),
        840,
        740,
        1.0,
        false,
        &grok::visual_fixture(false, false),
    );
    harness.click_control(LCD, PointerButton::Primary);
    harness.click_label("Unassigned");
    harness.click_label("Pin a Claude chat");
    harness.click_label("https://claude.ai/chat/…");
    harness.frame(vec![Event::Text(format!(
        "https://claude.ai/chat/{}?ref=fixture",
        Uuid::from_u128(2)
    ))]);
    harness.click_label("Title shown on the key");
    harness.frame(vec![Event::Text("Design review".into())]);
    harness.click_label("Save");
    assert_eq!(harness.editor_target(), None);
    let path = claude::Layout::path(&profile);
    let saved = claude::Layout::load(&path).unwrap();
    assert_eq!(saved.pinned(1), Some((Uuid::from_u128(2), "Design review")));
    harness.click_label("Open chat");
    assert_eq!(harness.opened, [Uuid::from_u128(2).to_string()]);

    harness.click_control(LCD, PointerButton::Primary);
    let editor = harness.app.claude_agent.editor.as_mut().unwrap();
    let claude::Binding::PinnedChat { id, .. } = editor.draft.binding_mut(LCD) else {
        unreachable!()
    };
    *id = "https://example.com/chat/invalid".into();
    harness.click_label("Save");
    assert!(
        harness
            .app
            .claude_agent
            .editor
            .as_ref()
            .unwrap()
            .error
            .is_some()
    );
    assert_eq!(claude::Layout::load(&path).unwrap(), saved);
}

#[test]
fn a_delayed_deck_exit_cannot_close_a_new_visit_to_claude_mode() {
    let root = tempfile::tempdir().unwrap();
    let mut harness = Harness::new(
        View::Claude,
        root.path().join("profile.json"),
        840,
        740,
        1.0,
        false,
        &grok::visual_fixture(false, false),
    );
    let earlier = harness.app.mode.generation;
    harness.app.set_view(super::View::Workspace);
    harness.app.set_view(super::View::Claude);
    harness.app.apply_mode_update(earlier, ModeUpdate::Exited);
    assert_eq!(harness.app.view(), super::View::Claude);
    harness
        .app
        .apply_mode_update(harness.app.mode.generation, ModeUpdate::Exited);
    assert_eq!(harness.app.view(), super::View::Workspace);
}

#[test]
fn workspace_dialogs_and_settings_preserve_their_save_boundaries() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("profile.json");
    let mut harness = Harness::new(
        View::Workspace,
        path.clone(),
        700,
        580,
        1.25,
        true,
        &grok::visual_fixture(false, false),
    );
    harness.click_label("Default");
    harness.click_label("New profile…");
    harness.frame(vec![Event::Text("Discard this".into())]);
    assert_eq!(harness.app.dialog.as_ref().unwrap().name, "Discard this");
    harness.key(Key::Escape);
    assert!(harness.app.dialog.is_none());
    assert_eq!(harness.app.draft.profiles.len(), 1);

    harness.click_label("Default");
    harness.click_label("New profile…");
    harness.click_label("Done");
    assert!(
        harness.app.dialog.is_some(),
        "An empty name cannot be saved"
    );
    harness.click_label("Name");
    harness.frame(vec![Event::Text("Evening".into())]);
    harness.key(Key::Enter);
    assert!(harness.app.dialog.is_none());
    assert_eq!(harness.app.draft.profiles[1].name, "Evening");
    assert_eq!(harness.app.saved.profiles.len(), 1);

    // Drag an action from the palette onto LCD key 2 at the minimum size.
    let start = harness.label_position("Open a website");
    let end = pos2(156.0, 152.0);
    for events in [
        vec![Event::PointerMoved(start)],
        vec![Event::PointerButton {
            pos: start,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        }],
        vec![Event::PointerMoved(end)],
        vec![Event::PointerButton {
            pos: end,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        }],
    ] {
        harness.frame(events);
    }
    harness.settle();
    assert_eq!(harness.app.selected, LCD);
    assert_eq!(
        harness.app.page().layout.action(LCD).kind,
        ActionKind::OpenWebsite
    );
    harness.click_label("https://example.com");
    harness.frame(vec![Event::Text("https://example.com".into())]);

    harness.workspace_state("settings-dirty");
    assert_eq!(harness.app.draft.rotation, 180);
    assert_eq!(harness.app.saved.rotation, 90);
    harness.click_label("Save workspace");
    let saved = Workspace::load(&path).unwrap();
    assert_eq!(saved.rotation, 180);
    assert_eq!(saved.profiles[1].name, "Evening");
    assert_eq!(
        saved
            .page(harness.app.scene)
            .unwrap()
            .layout
            .action(LCD)
            .value,
        "https://example.com"
    );
    harness.click_label("Done");
    assert!(!harness.app.settings);

    harness.workspace_state("plugins-enable");
    assert!(harness.app.plugin_enable.is_some());
    harness.click_label("Cancel");
    assert!(harness.app.plugin_enable.is_none());
    let plugins = harness.app.plugins.as_ref().unwrap().installed();
    assert_eq!(plugins.len(), 1);
    assert!(!plugins[0].enabled, "Cancel must not enable the plugin");
}

#[test]
#[ignore = "needs an EGL implementation, the installed Grok bundle, and DECKX_VISUAL_DIR"]
fn app_pages_render_offscreen() {
    let output = PathBuf::from(std::env::var_os("DECKX_VISUAL_DIR").unwrap());
    std::fs::create_dir_all(&output).unwrap();
    for (index, image) in crate::easter_egg::sydney(Workspace::default().rotation)
        .unwrap()
        .into_iter()
        .enumerate()
    {
        std::fs::write(output.join(format!("sydney-key-{}.jpg", index + 1)), image).unwrap();
    }
    let sandbox = tempfile::tempdir().unwrap();
    // Use the shipping egui renderer on an EGL pbuffer, without a desktop window.
    let display =
        unsafe { Display::new(RawDisplayHandle::Windows(WindowsDisplayHandle::new())) }.unwrap();
    let config = unsafe {
        display.find_configs(
            ConfigTemplateBuilder::new()
                .with_surface_type(ConfigSurfaceTypes::PBUFFER)
                .with_alpha_size(8)
                .build(),
        )
    }
    .unwrap()
    .next()
    .unwrap();
    let options = ContextAttributesBuilder::new()
        .with_context_api(ContextApi::Gles(Some(Version::new(3, 0))))
        .build(None);

    let grok_fixture = grok::visual_fixture(true, true);
    for (width, height) in [(840, 740), (700, 580)] {
        for scale in [1.0_f32, 1.25, 1.5] {
            let physical = [
                (width as f32 * scale) as u32,
                (height as f32 * scale) as u32,
            ];
            let surface = unsafe {
                display.create_pbuffer_surface(
                    &config,
                    &SurfaceAttributesBuilder::<PbufferSurface>::new().build(
                        NonZeroU32::new(physical[0]).unwrap(),
                        NonZeroU32::new(physical[1]).unwrap(),
                    ),
                )
            }
            .unwrap();
            let gl_context = unsafe { display.create_context(&config, &options) }
                .unwrap()
                .make_current(&surface)
                .unwrap();
            let gl = Arc::new(unsafe {
                glow::Context::from_loader_function(|symbol| {
                    let symbol = std::ffi::CString::new(symbol).unwrap();
                    display.get_proc_address(symbol.as_c_str())
                })
            });
            for view in [View::Codex, View::Grok, View::Claude, View::Workspace] {
                let mut states = match view {
                    View::Codex | View::Grok | View::Claude => vec![
                        "populated",
                        "empty",
                        "edit-lcd",
                        "edit-side",
                        "edit-dial",
                        "action-menu",
                        "picker",
                        "context-menu",
                        "opened",
                        "reset",
                        "usage-menu",
                        "usage-tile",
                        "usage-zero",
                        "usage-low",
                        "usage-quarter",
                        "usage-full",
                        "usage-hover",
                        "usage-details",
                        "usage-unavailable",
                        "usage-waiting",
                        "dial-default",
                        "dial-menu",
                        "dial-switch",
                        "dial-hover",
                    ],
                    View::Workspace => vec![
                        "layout",
                        "filter",
                        "palette-menu",
                        "appearance-menu",
                        "binding",
                        "binding-menu",
                        "profile-menu",
                        "profile-dialog",
                        "settings-general",
                        "settings-startup-on",
                        "settings-startup-error",
                        "settings-rotation",
                        "settings-dirty",
                        "settings-mcp",
                        "settings-mcp-scrolled",
                        "plugins-empty",
                        "plugins-installed",
                        "plugins-enable",
                        "plugins-uninstall",
                        "plugins-unavailable",
                        "marketplace-results",
                        "marketplace-scrolled",
                        "marketplace-detail",
                        "marketplace-empty",
                        "marketplace-error",
                        "marketplace-loading",
                    ],
                };
                states.extend([
                    "settings-easter-egg-menu",
                    "settings-easter-egg-on",
                    "easter-egg-visible",
                    "settings-tray-off",
                    "settings-tray-on",
                ]);
                if !matches!(view, View::Workspace) && height == 580 {
                    states.push("scrolled");
                }
                if matches!(view, View::Codex) {
                    states.extend([
                        "classic",
                        "settings-general",
                        "settings-startup-on",
                        "status-incompatible",
                    ]);
                } else if matches!(view, View::Grok) {
                    states.extend(["plugins-empty", "settings-general", "settings-startup-on"]);
                } else if matches!(view, View::Claude) {
                    states.extend(["invalid-link", "app-missing"]);
                }
                for state in states {
                    let name = format!(
                        "{}-{state}-{width}x{height}-{}",
                        view.name(),
                        (scale * 100.0) as u32
                    );
                    let profile = sandbox.path().join(&name).join("profile.json");
                    let mut harness = Harness::new(
                        view,
                        profile,
                        width,
                        height,
                        scale,
                        state != "empty",
                        &grok_fixture,
                    );
                    match state {
                        "easter-egg-visible" => harness.app.easter_egg_visible = true,
                        _ if matches!(view, View::Workspace)
                            || matches!(
                                state,
                                "settings-general"
                                    | "settings-startup-on"
                                    | "plugins-empty"
                                    | "settings-easter-egg-menu"
                                    | "settings-easter-egg-on"
                                    | "settings-tray-off"
                                    | "settings-tray-on"
                            ) =>
                        {
                            harness.workspace_state(state);
                        }
                        "edit-lcd" => harness.click_control(LCD, PointerButton::Primary),
                        "edit-side" => harness.click_control(SIDE, PointerButton::Primary),
                        "edit-dial" => {
                            harness.click_control(DIAL, PointerButton::Primary);
                            harness.click_label("Turn right");
                            assert_eq!(
                                harness.editor_target(),
                                Some(Some(Control::Dial(1, Direction::Clockwise)))
                            );
                        }
                        "action-menu" => {
                            harness.click_control(SIDE, PointerButton::Primary);
                            harness.click_label(view.side_default());
                        }
                        "picker" => harness.pin_draft(),
                        state if state.starts_with("dial-") => harness.show_main_dial(state),
                        state if state.starts_with("usage-") => {
                            harness.show_usage(state);
                            if (width, height, scale) == (840, 740, 1.0)
                                && let Some((_, provider)) = harness.app.mode.active.usage_key()
                            {
                                let report = harness.app.usage.readings.get(provider);
                                let tile = crate::usage::KeyFace::from_report(report)
                                    .render(crate::theme::LINE);
                                let jpeg = crate::profile::encode_tile(
                                    tile,
                                    Workspace::default().rotation,
                                )
                                .unwrap();
                                std::fs::write(output.join(format!("{name}-lcd.jpg")), jpeg)
                                    .unwrap();
                            }
                        }
                        "invalid-link" => {
                            harness.pin_draft();
                            let editor = harness.app.claude_agent.editor.as_mut().unwrap();
                            let claude::Binding::PinnedChat { id, .. } =
                                editor.draft.binding_mut(LCD)
                            else {
                                unreachable!()
                            };
                            *id = "https://example.com/chat/design-review".into();
                            harness.click_label("Save");
                            assert!(
                                harness
                                    .app
                                    .claude_agent
                                    .editor
                                    .as_ref()
                                    .unwrap()
                                    .error
                                    .is_some()
                            );
                        }
                        "app-missing" => harness.app.report(
                            "Could not open Claude Desktop. Check that Claude Desktop is installed",
                            true,
                        ),
                        "context-menu" | "opened" => {
                            harness.click_control(LCD, PointerButton::Secondary);
                            if state == "opened" {
                                harness.click_label(view.open_label());
                                assert_eq!(harness.opened.len(), 1);
                            }
                        }
                        "reset" => harness.click_label("Reset layout…"),
                        "classic" => harness.click_label("Classic"),
                        "status-incompatible" => {
                            harness.app.codex_error = Some(codex::STATUS_FORMAT_ERROR.to_owned());
                        }
                        "scrolled" => harness.scroll(pos2(200.0, 400.0), -350.0),
                        _ => {}
                    }
                    if !matches!(
                        state,
                        "context-menu"
                            | "action-menu"
                            | "dial-menu"
                            | "dial-hover"
                            | "usage-menu"
                            | "usage-hover"
                            | "profile-menu"
                            | "binding-menu"
                            | "settings-rotation"
                            | "settings-easter-egg-menu"
                            | "palette-menu"
                            | "appearance-menu"
                    ) {
                        harness.frame(vec![Event::PointerMoved(pos2(8.0, 70.0))]);
                        harness.settle();
                    }
                    render(
                        harness,
                        gl.clone(),
                        physical,
                        &output.join(format!("{name}.png")),
                    );
                }
            }
            gl_context.make_not_current().unwrap();
        }
    }
}

fn render(harness: Harness, gl: Arc<glow::Context>, physical: [u32; 2], path: &Path) {
    let mut painter = Painter::new(gl.clone(), "", None, false).unwrap();
    let mut frames = harness.frames.into_iter().peekable();
    while let Some(frame) = frames.next() {
        let shapes = if frames.peek().is_none() {
            harness.ctx.tessellate(frame.shapes, frame.pixels_per_point)
        } else {
            Vec::new()
        };
        painter.clear(physical, [0.0, 0.0, 0.0, 1.0]);
        painter.paint_and_update_textures(
            physical,
            frame.pixels_per_point,
            &shapes,
            &frame.textures_delta,
        );
    }
    let [w, h] = physical;
    let mut pixels = vec![0; (w * h * 4) as usize];
    unsafe {
        gl.read_pixels(
            0,
            0,
            w as i32,
            h as i32,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut pixels)),
        );
    }
    let mut image = image::RgbaImage::from_raw(w, h, pixels).unwrap();
    image::imageops::flip_vertical_in_place(&mut image);
    image.save(path).unwrap();
    painter.destroy();
}
