//! Chat assignments for Claude Desktop. Claude owns authentication and navigation.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

use crate::{
    actions::{Action, PreparedAction},
    agent_layout::{self, Assignments, Preset, Slot},
    glyphs::TitleStyle,
    profile::Control,
    theme,
};

const NAME: &str = "Claude mode";
const HOST: &str = "claude.ai";
pub(crate) const TITLE_LIMIT: usize = 80;

/// The destination passed to the Windows URI handler.
#[derive(Clone, Copy)]
pub(crate) enum Target {
    NewChat,
    Chat(Uuid),
}

impl Target {
    pub(crate) fn link(self) -> String {
        match self {
            Self::NewChat => format!("claude://{HOST}/new"),
            Self::Chat(id) => format!("claude://{HOST}/chat/{id}"),
        }
    }
}

fn chat_id(value: &str) -> Result<Uuid> {
    const HELP: &str = "Enter a Claude chat link or chat ID";
    let value = value.trim();
    if let Ok(id) = Uuid::parse_str(value) {
        return Ok(id);
    }
    let url = Url::parse(value).context(HELP)?;
    ensure!(
        matches!(url.scheme(), "https" | "claude")
            && url
                .host_str()
                .is_some_and(|host| host.eq_ignore_ascii_case(HOST))
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none(),
        HELP
    );
    let path = url.path().strip_prefix("/chat/").context(HELP)?;
    Uuid::parse_str(path).context(HELP)
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) enum Binding {
    #[default]
    None,
    /// A saved UUID, or the URL or UUID being edited in a draft.
    PinnedChat {
        id: String,
        title: String,
    },
    NewChat,
    OpenSelected,
    PreviousChat,
    NextChat,
    ExitMode,
    Action(Action),
}

impl Binding {
    pub(crate) const MODE_ACTIONS: [Self; 5] = [
        Self::NewChat,
        Self::OpenSelected,
        Self::PreviousChat,
        Self::NextChat,
        Self::ExitMode,
    ];

    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::None => "Unassigned",
            Self::PinnedChat { .. } => "Pin a Claude chat",
            Self::NewChat => "New chat",
            Self::OpenSelected => "Open selected chat",
            Self::PreviousChat => "Select previous chat",
            Self::NextChat => "Select next chat",
            Self::ExitMode => "Exit Claude mode",
            Self::Action(action) => action.kind.label(),
        }
    }

    pub(crate) fn tile_label(&self) -> &'static str {
        match self {
            Self::None | Self::PinnedChat { .. } => "",
            Self::NewChat => "NEW",
            Self::OpenSelected => "OPEN",
            Self::PreviousChat => "< CHAT",
            Self::NextChat => "CHAT >",
            Self::ExitMode => "EXIT",
            Self::Action(action) => action.kind.tile_label(),
        }
    }
}

impl Slot for Binding {
    fn defaults() -> Assignments<Self> {
        let mut buttons = std::array::from_fn(|_| Self::None);
        let first_side = crate::profile::DISPLAY_KEYS;
        buttons[first_side] = Self::NewChat;
        buttons[first_side + 1] = Self::OpenSelected;
        buttons[first_side + 2] = Self::ExitMode;
        Assignments::new(
            buttons,
            [
                [Self::PreviousChat, Self::OpenSelected, Self::NextChat],
                std::array::from_fn(|_| Self::None),
                std::array::from_fn(|_| Self::None),
            ],
        )
    }

    fn validate(&self, control: Control) -> Result<()> {
        match self {
            Self::PinnedChat { id, title } => {
                ensure!(control.is_lcd(), "Pin chats to LCD keys");
                Uuid::parse_str(id).context("Enter a Claude chat link or chat ID")?;
                ensure!(!title.trim().is_empty(), "Enter an LCD title");
                ensure!(
                    title.chars().count() <= TITLE_LIMIT,
                    "LCD titles can have up to {TITLE_LIMIT} characters"
                );
            }
            Self::Action(action) => ensure!(
                action.kind.allowed_in_agent_mode(),
                "This action is unavailable in Claude mode"
            ),
            _ => {}
        }
        Ok(())
    }

    fn action(&self) -> Option<&Action> {
        match self {
            Self::Action(action) => Some(action),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub(crate) struct Layout(Assignments<Binding>);

impl Layout {
    pub(crate) fn path(profile: &Path) -> PathBuf {
        profile.with_file_name("claude-layout.json")
    }

    pub(crate) fn load(path: &Path) -> Result<Self> {
        let layout: Self = agent_layout::load(path, NAME)?;
        layout.0.validate()?;
        Ok(layout)
    }

    pub(crate) fn normalized(mut self) -> Result<Self> {
        for control in agent_layout::controls() {
            let Binding::PinnedChat { id, title } = self.binding_mut(control) else {
                continue;
            };
            *id = chat_id(id).with_context(|| control.label())?.to_string();
            *title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        }
        Ok(self)
    }

    pub(crate) fn save(&self, path: &Path, previous: &Self) -> Result<()> {
        self.0.validate_changes(&previous.0)?;
        agent_layout::save(path, NAME, self)
    }

    pub(crate) fn binding(&self, control: Control) -> &Binding {
        self.0.binding(control)
    }

    pub(crate) fn binding_mut(&mut self, control: Control) -> &mut Binding {
        self.0.binding_mut(control)
    }

    pub(crate) fn preset(&self) -> Preset {
        self.0.preset
    }

    pub(crate) fn set_preset(&mut self, preset: Preset) {
        self.0.preset = preset;
    }

    pub(crate) fn active_binding(&self, control: Control) -> Option<&Binding> {
        self.0.active_binding(control)
    }

    pub(crate) fn pinned(&self, index: usize) -> Option<(Uuid, &str)> {
        let control = Control::Button(index);
        if control.is_lcd()
            && let Some(Binding::PinnedChat { id, title }) = self.active_binding(control)
        {
            return Some((Uuid::parse_str(id).ok()?, title));
        }
        None
    }
}

#[derive(Clone)]
pub(crate) struct Mode {
    pub layout: Arc<Layout>,
    pub selected: Option<usize>,
}

impl Mode {
    pub(crate) fn new(layout: Layout) -> Self {
        let selected = agent_layout::next_key(None, true, |index| layout.pinned(index).is_some());
        Self {
            layout: Arc::new(layout),
            selected,
        }
    }

    pub(crate) fn set_layout(&mut self, layout: Arc<Layout>) {
        let selected = self.selection();
        self.layout = layout;
        self.set_selection(selected);
    }

    pub(crate) fn selection(&self) -> Option<(usize, Uuid)> {
        let selected = self.selected?;
        self.layout.pinned(selected).map(|(id, _)| (selected, id))
    }

    pub(crate) fn set_selection(&mut self, selection: Option<(usize, Uuid)>) {
        self.selected = selection
            .and_then(|(slot, id)| {
                let matches = |index| {
                    self.layout
                        .pinned(index)
                        .is_some_and(|(chat, _)| id == chat)
                };
                if matches(slot) {
                    Some(slot)
                } else {
                    (0..crate::profile::DISPLAY_KEYS).find(|&index| matches(index))
                }
            })
            .or_else(|| {
                agent_layout::next_key(None, true, |index| self.layout.pinned(index).is_some())
            });
    }

    pub(crate) fn input_with(
        &mut self,
        control: Control,
        run: impl FnOnce(PreparedAction) -> Result<()>,
    ) -> Result<()> {
        if let Control::Button(index) = control
            && let Some((id, _)) = self.layout.pinned(index)
        {
            self.selected = Some(index);
            return run(PreparedAction::Claude(Target::Chat(id)));
        }
        let Some(binding) = self.layout.active_binding(control) else {
            return Ok(());
        };
        let action = match binding {
            Binding::NewChat => PreparedAction::Claude(Target::NewChat),
            Binding::OpenSelected => {
                let Some((_, id)) = self.selection() else {
                    return Ok(());
                };
                PreparedAction::Claude(Target::Chat(id))
            }
            binding @ (Binding::PreviousChat | Binding::NextChat) => {
                self.selected =
                    agent_layout::next_key(self.selected, *binding == Binding::NextChat, |index| {
                        self.layout.pinned(index).is_some()
                    });
                return Ok(());
            }
            Binding::Action(action) => action.prepare()?,
            Binding::None | Binding::PinnedChat { .. } | Binding::ExitMode => return Ok(()),
        };
        run(action)
    }
}

pub(crate) fn title_style(selected: bool) -> TitleStyle {
    let [r, g, b, _] = theme::ACCENT.to_array();
    let [tr, tg, tb, _] = theme::TEXT.to_array();
    TitleStyle {
        font: "strong",
        background: theme::AGENT_TITLE,
        text: [tr, tg, tb],
        outline: selected.then_some([r, g, b]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{actions::ActionKind, profile::Direction};

    #[test]
    fn only_claude_chat_destinations_are_accepted() {
        let id = Uuid::from_u128(24);
        assert_eq!(chat_id(&id.to_string()).unwrap(), id);
        let web = format!("https://claude.ai/chat/{id}?q=ignored#section");
        assert_eq!(chat_id(&web).unwrap(), id);
        let link = Target::Chat(id).link();
        assert_eq!(link, format!("claude://claude.ai/chat/{id}"));
        assert_eq!(chat_id(&link).unwrap(), id);
        assert_eq!(Target::NewChat.link(), "claude://claude.ai/new");
        for invalid in [
            format!("https://claude.ai.example/chat/{id}"),
            format!("https://example.com@claude.ai/chat/{id}"),
            format!("https://claude.ai:9000/chat/{id}"),
            format!("https://claude.ai/share/{id}"),
            format!("https://claude.ai/chat/{id}/extra"),
            format!("http://claude.ai/chat/{id}"),
            "claude://claude.ai/new".into(),
            "".into(),
        ] {
            assert!(chat_id(&invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn a_saved_pin_survives_reload_and_an_invalid_edit() -> Result<()> {
        let folder = tempfile::tempdir()?;
        let path = Layout::path(&folder.path().join("profile.json"));
        let defaults = Layout::load(&path)?;
        let mut layout = defaults.clone();
        let id = Uuid::from_u128(42);
        *layout.binding_mut(Control::Button(1)) = Binding::PinnedChat {
            id: format!("https://claude.ai/chat/{id}?q=ignored"),
            title: " Design  review ".into(),
        };
        let layout = layout.normalized()?;
        layout.save(&path, &defaults)?;
        let reloaded = Layout::load(&path)?;
        assert_eq!(reloaded, layout);
        assert_eq!(reloaded.pinned(1), Some((id, "Design review")));
        assert!(!std::fs::read_to_string(&path)?.contains("://"));
        let mut invalid = layout.clone();
        *invalid.binding_mut(Control::Button(1)) = Binding::PinnedChat {
            id: id.to_string(),
            title: " ".into(),
        };
        assert!(invalid.normalized()?.save(&path, &layout).is_err());
        assert_eq!(Layout::load(&path)?, layout);
        Ok(())
    }

    #[test]
    fn one_press_opens_the_pinned_id_and_the_dial_skips_non_chat_keys() -> Result<()> {
        let mut layout = Layout::default();
        let ids = [Uuid::from_u128(1), Uuid::from_u128(2)];
        for (index, id) in [1, 5].into_iter().zip(ids) {
            *layout.binding_mut(Control::Button(index)) = Binding::PinnedChat {
                id: id.to_string(),
                title: "Same title".into(),
            };
        }
        *layout.binding_mut(Control::Button(3)) =
            Binding::Action(Action::from_kind(ActionKind::Mute));
        let mut mode = Mode::new(layout);
        assert_eq!(mode.selection(), Some((1, ids[0])));
        let mut opened = Vec::new();
        let mut run = |action| {
            let PreparedAction::Claude(target) = action else {
                panic!("Expected a Claude action")
            };
            opened.push(target.link());
            Ok(())
        };
        mode.input_with(Control::Button(1), &mut run)?;
        assert_eq!(mode.selection(), Some((1, ids[0])));
        mode.input_with(Control::Dial(0, Direction::Clockwise), &mut run)?;
        assert_eq!(mode.selection(), Some((5, ids[1])));
        mode.input_with(Control::Dial(0, Direction::Press), &mut run)?;
        assert_eq!(
            opened,
            [Target::Chat(ids[0]).link(), Target::Chat(ids[1]).link()]
        );

        let mut next = (*mode.layout).clone();
        let moved = std::mem::take(next.binding_mut(Control::Button(5)));
        *next.binding_mut(Control::Button(0)) = moved;
        mode.set_layout(Arc::new(next));
        assert_eq!(mode.selection(), Some((0, ids[1])));

        let mut next = (*mode.layout).clone();
        *next.binding_mut(Control::Button(0)) = Binding::None;
        mode.set_layout(Arc::new(next));
        assert_eq!(mode.selection(), Some((1, ids[0])));

        mode.set_layout(Arc::new(Layout::default()));
        assert_eq!(mode.selection(), None);
        Ok(())
    }
}
