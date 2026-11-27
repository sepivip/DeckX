//! Saved key and dial assignments for the agent modes.

use std::{
    fs::File,
    io::{BufReader, ErrorKind},
    path::Path,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    actions::Action,
    profile::{self, BUTTONS, Control, DIALS, DISPLAY_KEYS, Direction},
};

/// A mode's binding for one key or dial gesture.
pub(crate) trait Slot: Clone + PartialEq + Sized {
    fn defaults() -> Assignments<Self>;
    /// Checks the binding on `control` when a layout loads or changes.
    fn validate(&self, control: Control) -> Result<()>;
    /// The action this binding runs, which must prepare before it is saved.
    fn action(&self) -> Option<&Action>;
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Preset {
    #[default]
    Standard,
    UsageLimits,
}

impl Preset {
    pub(crate) const ALL: [Self; 2] = [Self::Standard, Self::UsageLimits];

    fn is_standard(&self) -> bool {
        *self == Self::Standard
    }

    pub(crate) fn usage_key(self, lcd_keys: usize) -> Option<usize> {
        match self {
            Self::Standard => None,
            Self::UsageLimits => lcd_keys.checked_sub(1),
        }
    }

    pub(crate) fn reserves(self, control: Control) -> bool {
        matches!(control, Control::Button(index) if Some(index) == self.usage_key(DISPLAY_KEYS))
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MainDial {
    #[default]
    Assignments,
    SwitchModes,
}

impl MainDial {
    pub(crate) fn uses_assignments(&self) -> bool {
        *self == Self::Assignments
    }

    pub(crate) fn mode_turn(self, control: Control) -> Option<Direction> {
        match (self, control) {
            (
                Self::SwitchModes,
                Control::Dial(0, turn @ (Direction::Clockwise | Direction::Counterclockwise)),
            ) => Some(turn),
            _ => None,
        }
    }
}

/// Bindings for every LCD key, side button, and dial gesture. The JSON
/// format adds an optional preset to the existing buttons and dials.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Assignments<B> {
    buttons: [B; BUTTONS],
    dials: [[B; 3]; DIALS],
    #[serde(default, skip_serializing_if = "Preset::is_standard")]
    pub(crate) preset: Preset,
}

impl<B: Slot> Default for Assignments<B> {
    fn default() -> Self {
        B::defaults()
    }
}

impl<B> Assignments<B> {
    pub(crate) fn new(buttons: [B; BUTTONS], dials: [[B; 3]; DIALS]) -> Self {
        Self {
            buttons,
            dials,
            preset: Preset::default(),
        }
    }

    pub fn binding(&self, control: Control) -> &B {
        match control {
            Control::Button(index) => &self.buttons[index],
            Control::Dial(index, direction) => &self.dials[index][direction.index()],
        }
    }

    pub(crate) fn active_binding(&self, control: Control) -> Option<&B> {
        (!self.preset.reserves(control)).then(|| self.binding(control))
    }

    pub fn binding_mut(&mut self, control: Control) -> &mut B {
        match control {
            Control::Button(index) => &mut self.buttons[index],
            Control::Dial(index, direction) => &mut self.dials[index][direction.index()],
        }
    }
}

impl<B: Slot> Assignments<B> {
    /// Checks every binding and names the control that fails.
    pub(crate) fn validate(&self) -> Result<()> {
        for control in controls() {
            self.binding(control)
                .validate(control)
                .with_context(|| control.label())?;
        }
        Ok(())
    }

    /// Checks the bindings that differ from `previous` and prepares their actions.
    pub(crate) fn validate_changes(&self, previous: &Self) -> Result<()> {
        for control in controls() {
            let binding = self.binding(control);
            if binding != previous.binding(control) {
                binding.validate(control).with_context(|| control.label())?;
                if let Some(action) = binding.action() {
                    action.prepare().with_context(|| control.label())?;
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn controls() -> impl Iterator<Item = Control> {
    (0..BUTTONS).map(Control::Button).chain(
        (0..DIALS)
            .flat_map(|index| Direction::ALL.map(|direction| Control::Dial(index, direction))),
    )
}

/// Moves between assigned LCD keys in physical key order, wrapping at either end.
pub(crate) fn next_key(
    selected: Option<usize>,
    forward: bool,
    assigned: impl Fn(usize) -> bool,
) -> Option<usize> {
    let Some(selected) = selected else {
        return (0..DISPLAY_KEYS).find(|&index| assigned(index));
    };
    let step = if forward { 1 } else { DISPLAY_KEYS - 1 };
    (1..=DISPLAY_KEYS)
        .map(|offset| (selected + step * offset) % DISPLAY_KEYS)
        .find(|&index| assigned(index))
}

/// Reads a saved layout, or the defaults when none was saved. `name` is the
/// mode's name in error messages.
pub(crate) fn load<T: DeserializeOwned + Default>(path: &Path, name: &str) -> Result<T> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(T::default()),
        Err(error) => {
            return Err(error).with_context(|| format!("Could not open the {name} layout"));
        }
    };
    serde_json::from_reader(BufReader::new(file)).with_context(|| format!("Invalid {name} layout"))
}

pub(crate) fn save(path: &Path, name: &str, layout: &impl Serialize) -> Result<()> {
    profile::save_json(path, layout).with_context(|| format!("Could not save the {name} layout"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        actions::ActionKind,
        codex::{self, KeyStyle},
        grok,
    };

    /// The layout files that DeckX wrote before the shared store.
    #[derive(Serialize)]
    struct LegacyCodex<'a> {
        key_style: KeyStyle,
        buttons: [&'a codex::Binding; BUTTONS],
        dials: [[&'a codex::Binding; 3]; DIALS],
    }

    #[derive(Serialize)]
    struct LegacyGrok<'a> {
        buttons: [&'a grok::Binding; BUTTONS],
        dials: [[&'a grok::Binding; 3]; DIALS],
    }

    fn legacy<B>(binding: impl Fn(Control) -> B) -> ([B; BUTTONS], [[B; 3]; DIALS]) {
        (
            std::array::from_fn(|index| binding(Control::Button(index))),
            std::array::from_fn(|index| {
                Direction::ALL.map(|direction| binding(Control::Dial(index, direction)))
            }),
        )
    }

    fn written(value: &impl Serialize) -> Vec<u8> {
        let mut bytes = serde_json::to_vec_pretty(value).unwrap();
        bytes.push(b'\n');
        bytes
    }

    #[test]
    fn codex_layout_files_keep_their_format() -> Result<()> {
        let folder = tempfile::tempdir()?;
        let path = folder.path().join("codex-layout.json");
        let defaults = codex::Layout::default();
        let mut layout = defaults.clone();
        layout.key_style = KeyStyle::Classic;
        let mut chat = Action::from_kind(ActionKind::CodexOpenChat);
        chat.value = "019a3c1e-2f4b-7c8d-9e0f-1a2b3c4d5e6f".into();
        *layout.binding_mut(Control::Button(2)) = codex::Binding::Action(chat);
        *layout.binding_mut(Control::Dial(1, Direction::Press)) = codex::Binding::ExitMode;
        layout.save(&path, &defaults)?;

        let (buttons, dials) = legacy(|control| layout.binding(control));
        let expected = written(&LegacyCodex {
            key_style: layout.key_style,
            buttons,
            dials,
        });
        assert_eq!(std::fs::read(&path)?, expected);
        assert_eq!(codex::Layout::load(&path)?, layout);

        // Layouts saved before key styles existed load with the default style.
        let (buttons, dials) = legacy(|control| layout.binding(control));
        let older = serde_json::json!({"buttons": buttons, "dials": dials});
        std::fs::write(&path, serde_json::to_vec(&older)?)?;
        let loaded = codex::Layout::load(&path)?;
        assert_eq!(loaded.key_style, KeyStyle::default());
        assert_eq!(
            loaded.binding(Control::Button(2)),
            layout.binding(Control::Button(2))
        );
        Ok(())
    }

    #[test]
    fn grok_layout_files_keep_their_format() -> Result<()> {
        let folder = tempfile::tempdir()?;
        let path = folder.path().join("grok-layout.json");
        let defaults = grok::Bindings::default();
        let mut bindings = defaults.clone();
        *bindings.binding_mut(Control::Button(1)) = grok::Binding::PinnedBot {
            id: "archive".into(),
            label: "Archive".into(),
        };
        *bindings.binding_mut(Control::Dial(2, Direction::Clockwise)) = grok::Binding::NextBot;
        bindings.save(&path, &defaults)?;

        let (buttons, dials) = legacy(|control| bindings.binding(control));
        let expected = written(&LegacyGrok { buttons, dials });
        assert_eq!(std::fs::read(&path)?, expected);
        assert_eq!(grok::Bindings::load(&path)?, bindings);
        Ok(())
    }
}
