use std::path::{Path, PathBuf};

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    actions::{Action, grok_agent_link},
    agent_layout::{self, Assignments, Slot},
    profile::Control,
};

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub enum Binding {
    #[default]
    None,
    MainBot,
    RecentBot,
    PinnedBot {
        id: String,
        label: String,
    },
    NewBot,
    Search,
    PreviousBot,
    NextBot,
    OpenSelected,
    ExitMode,
    Action(Action),
}

impl Binding {
    pub const COMMANDS: [Self; 6] = [
        Self::NewBot,
        Self::Search,
        Self::OpenSelected,
        Self::PreviousBot,
        Self::NextBot,
        Self::ExitMode,
    ];

    pub fn label(&self) -> &str {
        match self {
            Self::None => "Unassigned",
            Self::MainBot => "Follow the main bot",
            Self::RecentBot => "Follow a recent bot",
            Self::PinnedBot { label, .. } if !label.is_empty() => label,
            Self::PinnedBot { .. } => "Open a specific bot",
            Self::NewBot => "New bot",
            Self::Search => "Search Grok Bot",
            Self::PreviousBot => "Select previous bot",
            Self::NextBot => "Select next bot",
            Self::OpenSelected => "Open selected bot",
            Self::ExitMode => "Exit Grok Bot Mode",
            Self::Action(action) => action.kind.label(),
        }
    }

    pub fn tile_label(&self) -> &str {
        match self {
            Self::None | Self::MainBot | Self::RecentBot | Self::PinnedBot { .. } => "",
            Self::NewBot => "NEW BOT",
            Self::Search => "SEARCH",
            Self::PreviousBot => "< BOT",
            Self::NextBot => "BOT >",
            Self::OpenSelected => "OPEN",
            Self::ExitMode => "EXIT",
            Self::Action(action) => action.kind.tile_label(),
        }
    }
}

impl Slot for Binding {
    fn defaults() -> Bindings {
        use Binding::*;
        Assignments::new(
            [
                MainBot,
                RecentBot,
                RecentBot,
                RecentBot,
                RecentBot,
                RecentBot,
                NewBot,
                OpenSelected,
                ExitMode,
            ],
            [
                [PreviousBot, OpenSelected, NextBot],
                [None, Search, None],
                [None, ExitMode, None],
            ],
        )
    }

    fn validate(&self, control: Control) -> Result<()> {
        match self {
            Self::MainBot | Self::RecentBot => ensure!(
                control.is_lcd(),
                "Bot tracking is only available on LCD keys"
            ),
            Self::PinnedBot { id, label } => {
                grok_agent_link(id)?;
                ensure!(!label.trim().is_empty(), "Choose a bot to pin");
            }
            Self::Action(action) => ensure!(
                action.kind.allowed_in_agent_mode(),
                "This action is unavailable in Grok Bot Mode"
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

/// The Grok Bot Mode layout, saved as `grok-layout.json`.
pub type Bindings = Assignments<Binding>;

const NAME: &str = "Grok Bot";

impl Assignments<Binding> {
    pub fn path(profile_path: &Path) -> PathBuf {
        profile_path.with_file_name("grok-layout.json")
    }

    pub fn load(path: &Path) -> Result<Self> {
        let layout: Self = agent_layout::load(path, NAME)?;
        layout.validate()?;
        Ok(layout)
    }

    pub fn save(&self, path: &Path, previous: &Self) -> Result<()> {
        self.validate_changes(previous)?;
        agent_layout::save(path, NAME, self)
    }
}
