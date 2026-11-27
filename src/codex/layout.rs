use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    actions::{Action, ActionKind, PreparedAction, codex_chat_id},
    agent_layout::{self, Assignments, Preset, Slot},
    profile::{Control, DISPLAY_KEYS},
};

use super::RecentChat;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub enum Binding {
    #[default]
    None,
    RecentChat,
    PreviousChat,
    NextChat,
    OpenSelected,
    ExitMode,
    Action(Action),
}

impl Binding {
    pub const MODE_ACTIONS: [Self; 4] = [
        Self::OpenSelected,
        Self::PreviousChat,
        Self::NextChat,
        Self::ExitMode,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Self::None => "Unassigned",
            Self::RecentChat => "Follow a recent chat",
            Self::PreviousChat => "Select previous chat",
            Self::NextChat => "Select next chat",
            Self::OpenSelected => "Open selected chat",
            Self::ExitMode => "Exit Codex Mode",
            Self::Action(action) => action.kind.label(),
        }
    }

    pub fn tile_label(&self) -> &'static str {
        match self {
            Self::None | Self::RecentChat => "",
            Self::PreviousChat => "< CHAT",
            Self::NextChat => "CHAT >",
            Self::OpenSelected => "OPEN",
            Self::ExitMode => "EXIT",
            Self::Action(action) => action.kind.tile_label(),
        }
    }

    fn chat_id(&self) -> Option<Uuid> {
        match self {
            Self::Action(action) if action.kind == ActionKind::CodexOpenChat => {
                codex_chat_id(action.value.trim()).ok()
            }
            _ => None,
        }
    }
}

impl Slot for Binding {
    fn defaults() -> Assignments<Self> {
        let mut buttons = std::array::from_fn(|_| Binding::RecentChat);
        buttons[6] = Binding::Action(Action::from_kind(ActionKind::CodexNewChat));
        buttons[7] = Binding::OpenSelected;
        buttons[8] = Binding::ExitMode;
        Assignments::new(
            buttons,
            [
                [
                    Binding::PreviousChat,
                    Binding::OpenSelected,
                    Binding::NextChat,
                ],
                std::array::from_fn(|_| Binding::None),
                std::array::from_fn(|_| Binding::None),
            ],
        )
    }

    fn validate(&self, control: Control) -> Result<()> {
        let supported = match self {
            Self::RecentChat => control.is_lcd(),
            Self::Action(action) => action.kind.allowed_in_agent_mode(),
            _ => true,
        };
        ensure!(supported, "This action is unavailable in Codex Mode");
        if let Self::Action(action) = self
            && action.kind == ActionKind::CodexOpenChat
        {
            codex_chat_id(action.value.trim())?;
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

#[derive(
    Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum KeyStyle {
    Classic,
    #[default]
    ColorTitles,
}

impl KeyStyle {
    pub const ALL: [Self; 2] = [Self::ColorTitles, Self::Classic];

    pub fn label(self) -> &'static str {
        match self {
            Self::Classic => "Classic",
            Self::ColorTitles => "Color titles",
        }
    }
}

/// The Codex Mode layout, saved as `codex-layout.json` with the key style
/// ahead of the shared assignments.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Layout {
    #[serde(default)]
    pub key_style: KeyStyle,
    #[serde(flatten)]
    assignments: Assignments<Binding>,
}

const NAME: &str = "Codex Mode";

impl Layout {
    pub fn path(profile_path: &Path) -> PathBuf {
        profile_path.with_file_name("codex-layout.json")
    }

    pub fn load(path: &Path) -> Result<Self> {
        let layout: Self = agent_layout::load(path, NAME)?;
        layout.assignments.validate()?;
        Ok(layout)
    }

    pub fn save(&self, path: &Path, previous: &Self) -> Result<()> {
        self.assignments.validate_changes(&previous.assignments)?;
        agent_layout::save(path, NAME, self)
    }

    pub fn binding(&self, control: Control) -> &Binding {
        self.assignments.binding(control)
    }

    pub fn binding_mut(&mut self, control: Control) -> &mut Binding {
        self.assignments.binding_mut(control)
    }

    pub(crate) fn preset(&self) -> Preset {
        self.assignments.preset
    }

    pub(crate) fn set_preset(&mut self, preset: Preset) {
        self.assignments.preset = preset;
    }

    pub(crate) fn active_binding(&self, control: Control) -> Option<&Binding> {
        self.assignments.active_binding(control)
    }

    pub fn resolve(&self, chats: &[RecentChat]) -> ResolvedLayout {
        let key = |index| self.binding(Control::Button(index));
        let pinned: HashSet<_> = (0..DISPLAY_KEYS)
            .filter_map(|index| key(index).chat_id())
            .collect();
        let mut recent = chats.iter().filter(|chat| !pinned.contains(&chat.id));
        let agents =
            std::array::from_fn(|index| match self.active_binding(Control::Button(index))? {
                Binding::RecentChat => recent.next().map(Agent::from),
                binding => binding.chat_id().map(|id| {
                    chats.iter().find(|chat| chat.id == id).map_or_else(
                        || Agent {
                            id,
                            title: "Pinned chat".into(),
                        },
                        Agent::from,
                    )
                }),
            });
        ResolvedLayout {
            bindings: self.clone(),
            agents,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub id: Uuid,
    pub title: String,
}

impl From<&RecentChat> for Agent {
    fn from(chat: &RecentChat) -> Self {
        Self {
            id: chat.id,
            title: chat.thread_name.clone(),
        }
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct ResolvedLayout {
    pub bindings: Layout,
    pub agents: [Option<Agent>; DISPLAY_KEYS],
}

impl ResolvedLayout {
    pub fn chat_ids(&self) -> std::collections::BTreeSet<Uuid> {
        self.agents.iter().flatten().map(|chat| chat.id).collect()
    }
}

#[derive(Clone, Default)]
pub struct Mode {
    pub layout: ResolvedLayout,
    pub selected: Option<usize>,
    last_tap: Option<(usize, Uuid, Instant)>,
}

impl Mode {
    pub fn set_layout(&mut self, layout: ResolvedLayout) {
        if let Some(index) = self.selected
            && let Some(id) = self.layout.agents[index].as_ref().map(|chat| chat.id)
            && layout.agents[index].as_ref().map(|chat| chat.id) != Some(id)
        {
            self.selected = layout
                .agents
                .iter()
                .position(|chat| chat.as_ref().is_some_and(|chat| chat.id == id));
        }
        self.layout = layout;
        self.last_tap = None;
    }

    #[cfg(test)]
    pub fn input(&mut self, control: Control) -> Result<()> {
        self.input_with(control, |action| action.run())
    }

    /// Handles a deck control. `run` receives each action to run, so the caller can run it
    /// away from the HID thread.
    pub fn input_with(
        &mut self,
        control: Control,
        mut run: impl FnMut(PreparedAction) -> Result<()>,
    ) -> Result<()> {
        if let Control::Button(index) = control
            && let Some(Some(chat)) = self.layout.agents.get(index)
        {
            let id = chat.id;
            self.selected = Some(index);
            if self.last_tap.take().is_some_and(|(slot, last, at)| {
                slot == index && last == id && at.elapsed() <= Duration::from_millis(350)
            }) {
                run(PreparedAction::CodexAgent(id))?;
            } else {
                self.last_tap = Some((index, id, Instant::now()));
            }
            return Ok(());
        }
        self.last_tap = None;
        let Some(binding) = self.layout.bindings.active_binding(control) else {
            return Ok(());
        };
        match binding {
            Binding::OpenSelected => {
                if let Some(chat) = self
                    .selected
                    .and_then(|slot| self.layout.agents[slot].as_ref())
                {
                    run(PreparedAction::CodexAgent(chat.id))?;
                }
            }
            Binding::PreviousChat => self.select_chat(false),
            Binding::NextChat => self.select_chat(true),
            Binding::Action(action) => run(action.prepare()?)?,
            Binding::None | Binding::RecentChat | Binding::ExitMode => {}
        }
        Ok(())
    }

    fn select_chat(&mut self, forward: bool) {
        self.selected = agent_layout::next_key(self.selected, forward, |index| {
            self.layout.agents[index].is_some()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Direction;

    #[test]
    fn edited_gestures_survive_reload_and_invalid_edits_keep_the_saved_layout() -> Result<()> {
        let folder = tempfile::tempdir()?;
        let path = folder.path().join("codex-layout.json");
        let defaults = Layout::load(&path)?;
        let mut edited = defaults.clone();
        let left = Control::Dial(1, Direction::Counterclockwise);
        let press = Control::Dial(1, Direction::Press);
        *edited.binding_mut(left) = Binding::PreviousChat;
        *edited.binding_mut(press) = Binding::ExitMode;
        edited.save(&path, &defaults)?;
        assert_eq!(Layout::load(&path)?, edited);

        let mut invalid = edited.clone();
        *invalid.binding_mut(left) = Binding::Action(Action::from_kind(ActionKind::Shortcut));
        assert!(invalid.save(&path, &edited).is_err());
        assert_eq!(Layout::load(&path)?, edited);
        Ok(())
    }

    #[test]
    fn pinned_chats_stay_in_place_and_dial_navigation_skips_command_keys() -> Result<()> {
        let chats: Vec<RecentChat> = (1..=7)
            .map(|index| {
                serde_json::from_value(serde_json::json!({
                    "id": Uuid::from_u128(index).to_string(),
                    "thread_name": format!("Chat {index}"),
                    "updated_at": "2026-11-11T00:00:00Z",
                }))
            })
            .collect::<serde_json::Result<_>>()?;
        let mut layout = Layout::default();
        let mut pinned = Action::from_kind(ActionKind::CodexOpenChat);
        pinned.value = chats[0].id.to_string();
        *layout.binding_mut(Control::Button(2)) = Binding::Action(pinned);
        *layout.binding_mut(Control::Button(1)) =
            Binding::Action(Action::from_kind(ActionKind::CodexNewChat));
        let mut mode = Mode::default();
        mode.set_layout(layout.resolve(&chats));
        assert_eq!(
            mode.layout.agents[2].as_ref().map(|chat| chat.id),
            Some(chats[0].id)
        );
        assert_eq!(
            mode.layout.agents[0].as_ref().map(|chat| chat.id),
            Some(chats[1].id)
        );
        assert!(mode.layout.agents[1].is_none());
        mode.input(Control::Button(0))?;
        mode.input(Control::Dial(0, Direction::Clockwise))?;
        assert_eq!(mode.selected, Some(2));
        Ok(())
    }
}
