use anyhow::Result;
use eframe::egui::{Color32, RichText, Ui};

use super::{
    DeckX,
    mode_editor::{AgentState, Assignments, Stored, assignment_picker},
    mode_page::{ControlDisplay, ControlIcon, KeyFace, ModePage, OpenTarget, Summary},
};
use crate::{
    actions::{Action, ActionKind, codex_chat_id},
    codex::{self, Binding, KeyStyle, Mode, RecentChat},
    profile::{Control, DISPLAY_KEYS, Direction},
    theme::{self, DETAIL, MUTED},
};

impl Assignments for codex::Layout {
    type Binding = Binding;
    type Catalog = [RecentChat];
    const NAME: &'static str = "Codex Mode";

    fn binding(&self, control: Control) -> &Binding {
        self.binding(control)
    }

    fn binding_mut(&mut self, control: Control) -> &mut Binding {
        self.binding_mut(control)
    }

    fn preset(&self) -> crate::agent_layout::Preset {
        self.preset()
    }

    fn set_preset(&mut self, preset: crate::agent_layout::Preset) {
        self.set_preset(preset);
    }

    fn defaults(&self) -> Self {
        let mut defaults = Self::default();
        defaults.key_style = self.key_style;
        defaults.set_preset(self.preset());
        defaults
    }

    fn choices(control: Control) -> impl Iterator<Item = Binding> {
        std::iter::once(Binding::None)
            .chain(control.is_lcd().then_some(Binding::RecentChat))
            .chain(Binding::MODE_ACTIONS)
    }

    fn label(binding: &Binding, control: Control) -> &str {
        match binding {
            Binding::Action(action) => Self::action_name(action.kind, control),
            _ => binding.label(),
        }
    }

    fn action_name(kind: ActionKind, control: Control) -> &'static str {
        if control.is_lcd() && kind == ActionKind::CodexOpenChat {
            "Pin a Codex chat"
        } else {
            kind.label()
        }
    }

    fn from_action(action: Action) -> Binding {
        Binding::Action(action)
    }

    fn action(binding: &mut Binding) -> Option<&mut Action> {
        match binding {
            Binding::Action(action) => Some(action),
            _ => None,
        }
    }

    fn details(
        ui: &mut Ui,
        control: Control,
        binding: &mut Binding,
        chats: &[RecentChat],
        search: &mut String,
    ) {
        let Binding::Action(action) = binding else {
            return;
        };
        if action.kind != ActionKind::CodexOpenChat {
            return;
        }
        if control.is_lcd() {
            ui.label(
                RichText::new(
                    "The key shows live status. Double-tap the deck key to open the chat.",
                )
                .size(DETAIL)
                .color(MUTED),
            );
        }
        if let Some(id) = assignment_picker(
            ui,
            search,
            "Search recent chats",
            "No matching local chats",
            codex_chat_id(action.value.trim()).ok(),
            chats
                .iter()
                .map(|chat| (chat.id, chat.thread_name.as_str())),
        ) {
            action.value = id.to_string();
        }
    }
}

impl Stored for codex::Layout {
    fn file_path(profile: &std::path::Path) -> std::path::PathBuf {
        Self::path(profile)
    }

    fn read(path: &std::path::Path) -> Result<Self> {
        Self::load(path)
    }

    fn write(&self, path: &std::path::Path, previous: &Self) -> Result<()> {
        self.save(path, previous)
    }
}

impl ModePage for Mode {
    type Layout = codex::Layout;
    const OPEN_LABEL: &'static str = "Open chat";

    fn active(app: &DeckX) -> Option<&Self> {
        if let crate::runtime::Active::Codex(mode) = &app.mode.active {
            Some(mode)
        } else {
            None
        }
    }

    fn state(app: &DeckX) -> &AgentState<Self::Layout> {
        &app.codex_agent
    }

    fn state_mut(app: &mut DeckX) -> &mut AgentState<Self::Layout> {
        &mut app.codex_agent
    }

    fn catalog<'a>(&'a self, app: &'a DeckX) -> &'a [RecentChat] {
        &app.codex_catalog
    }

    fn save(app: &mut DeckX, draft: Self::Layout) -> Result<()> {
        app.save_codex_layout(draft)
    }

    fn errors<'a>(&'a self, app: &'a DeckX) -> impl Iterator<Item = &'a str> {
        app.codex_agent
            .load_error
            .as_deref()
            .into_iter()
            .chain(app.codex_error.as_deref())
    }

    fn style_controls(ui: &mut Ui, app: &mut DeckX) {
        for style in KeyStyle::ALL.into_iter().rev() {
            let selected = app.codex_agent.saved.key_style == style;
            let response = theme::tab(ui, style.label(), 80.0, selected);
            if response.clicked() && !selected {
                let mut layout = app.codex_agent.saved.clone();
                layout.key_style = style;
                match app.save_codex_layout(layout) {
                    Ok(()) => app.report("Key style saved", false),
                    Err(error) => app.report(format!("{error:#}"), true),
                }
                ui.ctx().request_repaint();
            }
        }
        ui.label(RichText::new("Key style").size(DETAIL).color(MUTED));
    }

    fn describe<'a>(&'a self, app: &'a DeckX, control: Control) -> ControlDisplay<'a> {
        if let Control::Button(index) = control
            && let Some(Some(chat)) = self.layout.agents.get(index)
        {
            return ControlDisplay {
                label: &chat.title,
                detail: Some(app.codex.status(chat.id).label()),
                icon: ControlIcon::None,
                highlight: self.selected == Some(index),
                can_open: true,
            };
        }
        let binding = self.layout.bindings.binding(control);
        ControlDisplay::binding(binding.label(), binding_icon(binding))
    }

    fn paint_lcd(&self, app: &DeckX, _ui: &mut Ui, key: KeyFace<'_>, display: &ControlDisplay<'_>) {
        let chat = self.layout.agents[key.index].as_ref();
        let status = chat.map(|chat| app.codex.status(chat.id));
        if app.codex_agent.saved.key_style == KeyStyle::Classic {
            let [r, g, b] = codex::classic_color(
                chat.is_some(),
                !self
                    .layout
                    .bindings
                    .binding(Control::Button(key.index))
                    .tile_label()
                    .is_empty(),
                status.unwrap_or_default(),
            );
            key.fill(Color32::from_rgb(r, g, b));
        } else if let (Some(chat), Some(status)) = (chat, status) {
            key.title(&chat.title, codex::title_style(status, display.highlight));
        } else {
            key.command(display.icon);
        }
    }

    fn selected(&self) -> Option<usize> {
        self.selected
            .filter(|&index| self.layout.agents.get(index).is_some_and(Option::is_some))
    }

    fn summary(&self) -> Summary {
        let recent = (0..DISPLAY_KEYS)
            .filter(|&index| {
                *self.layout.bindings.binding(Control::Button(index)) == Binding::RecentChat
            })
            .count();
        let agents = match recent {
            DISPLAY_KEYS => "Most recent chats".into(),
            0 => "Custom assignments".into(),
            count => format!("{count} follow recent chats").into(),
        };
        let defaults = codex::Layout::default();
        let default_dial = Direction::ALL.iter().all(|&gesture| {
            let control = Control::Dial(0, gesture);
            self.layout.bindings.binding(control) == defaults.binding(control)
        });
        Summary {
            agents: ("Choose which LCD keys follow chats or run actions", agents),
            dial: if default_dial {
                (
                    "Turn to select a chat and press to open it",
                    "Chat selection",
                )
            } else {
                ("Press and turn assignments", "Custom assignments")
            },
        }
    }

    fn open(
        app: &mut DeckX,
        index: usize,
        open: impl FnOnce(OpenTarget<'_>) -> Result<()>,
    ) -> Result<()> {
        if let Some(chat) = Self::active(app)
            .and_then(|mode| mode.layout.agents.get(index))
            .and_then(Option::as_ref)
        {
            open(OpenTarget::Chat(chat.id))?;
        }
        Ok(())
    }
}

fn binding_icon(binding: &Binding) -> ControlIcon {
    match binding {
        Binding::None | Binding::RecentChat => ControlIcon::None,
        Binding::OpenSelected => ControlIcon::Open,
        Binding::ExitMode => ControlIcon::Exit,
        Binding::PreviousChat => ControlIcon::Previous,
        Binding::NextChat => ControlIcon::Next,
        Binding::Action(action) => ControlIcon::action(action.kind),
    }
}
