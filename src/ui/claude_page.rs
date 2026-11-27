use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Result;
use eframe::egui::{RichText, Ui};

use super::{
    DeckX,
    mode_editor::{AgentState, Assignments, Stored},
    mode_page::{ControlDisplay, ControlIcon, KeyFace, ModePage, OpenTarget, Summary},
};
use crate::{
    actions::Action,
    claude::{self, Binding, Layout, Mode},
    profile::{Control, DISPLAY_KEYS, Direction},
    runtime::{Active, Request},
    theme::{self, BODY, GAP, MUTED},
};

impl Assignments for Layout {
    type Binding = Binding;
    type Catalog = ();
    const NAME: &'static str = "Claude mode";

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

    fn choices(control: Control) -> impl Iterator<Item = Binding> {
        std::iter::once(Binding::None)
            .chain(control.is_lcd().then(|| Binding::PinnedChat {
                id: String::new(),
                title: String::new(),
            }))
            .chain(Binding::MODE_ACTIONS)
    }

    fn label(binding: &Binding, _control: Control) -> &str {
        binding.label()
    }

    fn same_choice(current: &Binding, choice: &Binding) -> bool {
        matches!(
            (current, choice),
            (Binding::PinnedChat { .. }, Binding::PinnedChat { .. })
        ) || current == choice
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
        _control: Control,
        binding: &mut Binding,
        _: &(),
        _search: &mut String,
    ) {
        if let Binding::PinnedChat { id, title } = binding {
            ui.add_space(GAP);
            ui.label(RichText::new("Chat link or ID").size(BODY).color(MUTED));
            theme::input(ui, id, "https://claude.ai/chat/…", 2048);
            ui.label(RichText::new("LCD title").size(BODY).color(MUTED));
            theme::input(ui, title, "Title shown on the key", claude::TITLE_LIMIT);
        }
    }
}

impl Stored for Layout {
    fn file_path(profile: &Path) -> PathBuf {
        Self::path(profile)
    }

    fn read(path: &Path) -> Result<Self> {
        Self::load(path)
    }

    fn write(&self, path: &Path, previous: &Self) -> Result<()> {
        self.save(path, previous)
    }
}

impl ModePage for Mode {
    type Layout = Layout;
    const OPEN_LABEL: &'static str = "Open chat";

    fn active(app: &DeckX) -> Option<&Self> {
        if let Active::Claude(mode) = &app.mode.active {
            Some(mode)
        } else {
            None
        }
    }

    fn state(app: &DeckX) -> &AgentState<Layout> {
        &app.claude_agent
    }

    fn state_mut(app: &mut DeckX) -> &mut AgentState<Layout> {
        &mut app.claude_agent
    }

    fn catalog<'a>(&'a self, _app: &'a DeckX) -> &'a () {
        &()
    }

    fn save(app: &mut DeckX, draft: Layout) -> Result<()> {
        if app.claude_agent.save(&app.path, draft.normalized()?)?
            && let Active::Claude(mode) = &mut app.mode.active
        {
            let layout = Arc::new(app.claude_agent.saved.clone());
            mode.set_layout(Arc::clone(&layout));
            if let Some(session) = &app.session {
                let _ = session.requests.send(Request::ClaudeLayout(layout));
            }
        }
        Ok(())
    }

    fn errors<'a>(&'a self, app: &'a DeckX) -> impl Iterator<Item = &'a str> {
        app.claude_agent.load_error.as_deref().into_iter()
    }

    fn describe<'a>(&'a self, _app: &'a DeckX, control: Control) -> ControlDisplay<'a> {
        if let Control::Button(index) = control
            && let Some((_, title)) = self.layout.pinned(index)
        {
            return ControlDisplay {
                label: title,
                detail: Some("Pinned chat"),
                icon: ControlIcon::None,
                highlight: self.selected == Some(index),
                can_open: true,
            };
        }
        let binding = self.layout.binding(control);
        let icon = match binding {
            Binding::None | Binding::PinnedChat { .. } => ControlIcon::None,
            Binding::NewChat => ControlIcon::Add,
            Binding::OpenSelected => ControlIcon::Open,
            Binding::PreviousChat => ControlIcon::Previous,
            Binding::NextChat => ControlIcon::Next,
            Binding::ExitMode => ControlIcon::Exit,
            Binding::Action(action) => ControlIcon::action(action.kind),
        };
        ControlDisplay::binding(binding.label(), icon)
    }

    fn paint_lcd(
        &self,
        _app: &DeckX,
        _ui: &mut Ui,
        key: KeyFace<'_>,
        display: &ControlDisplay<'_>,
    ) {
        if let Some((_, title)) = self.layout.pinned(key.index) {
            key.title(title, claude::title_style(false));
        } else {
            key.command(display.icon);
        }
    }

    fn selected(&self) -> Option<usize> {
        self.selection().map(|(index, _)| index)
    }

    fn summary(&self) -> Summary {
        let count = (0..DISPLAY_KEYS)
            .filter(|&index| {
                matches!(
                    self.layout.binding(Control::Button(index)),
                    Binding::PinnedChat { .. }
                )
            })
            .count();
        let agents = if count == 1 {
            "1 pinned chat".into()
        } else {
            format!("{count} pinned chats").into()
        };
        let defaults = Layout::default();
        let default_dial = Direction::ALL.into_iter().all(|gesture| {
            let control = Control::Dial(0, gesture);
            self.layout.binding(control) == defaults.binding(control)
        });
        Summary {
            agents: ("Pin Claude chats or assign actions to LCD keys", agents),
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
        if let Some((id, _)) = Self::active(app).and_then(|mode| mode.layout.pinned(index)) {
            open(OpenTarget::ClaudeChat(id))?;
        }
        Ok(())
    }
}
