//! Titles, images, states and alerts that plugins set on their keys.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use serde_json::Value;

use super::{Hub, Instance, MAX_MESSAGE};
use crate::plugins::visual::RenderSpec;

#[derive(Default)]
struct Face {
    title: Option<String>,
    image: Option<Arc<str>>,
}

#[derive(Default)]
struct Appearance {
    face: Face,
    states: HashMap<usize, Face>,
    state: usize,
    revision: u64,
    flash: Option<(bool, Instant)>,
}

/// The device and editor appearances of one binding.
#[derive(Default)]
pub(super) struct Looks {
    hardware: Appearance,
    editor: Appearance,
}

/// A plugin request that changes a binding's appearance.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum Change {
    Title,
    Image,
    State,
    Ok,
    Alert,
}

impl Hub {
    fn appearance(&self, context: &str, hardware: bool) -> Option<&Appearance> {
        let looks = self.appearances.get(context)?;
        Some(if hardware {
            &looks.hardware
        } else {
            &looks.editor
        })
    }

    pub fn version(&self, context: &str, hardware: bool) -> (u64, bool) {
        self.appearance(context, hardware)
            .map(|look| {
                (
                    look.revision,
                    look.flash
                        .is_some_and(|(_, at)| at.elapsed() < Duration::from_millis(850)),
                )
            })
            .unwrap_or_default()
    }

    /// The state the device shows for a binding.
    pub(super) fn state(&self, context: &str) -> usize {
        self.appearance(context, true).map_or(0, |look| look.state)
    }

    /// Switches a two-state action after a key press, as Stream Deck does.
    pub(super) fn toggle_state(&mut self, context: &str) {
        let looks = self.appearances.entry(context.to_owned()).or_default();
        let next = 1usize.saturating_sub(looks.hardware.state.min(1));
        for look in [&mut looks.hardware, &mut looks.editor] {
            look.state = next;
            look.revision += 1;
        }
    }

    pub fn render_spec(&self, instance: &Instance, hardware: bool) -> Option<RenderSpec> {
        let (owner, action) = self.lookup(&instance.action)?;
        let look = self.appearance(&instance.context, hardware);
        let state_index = look.map_or(0, |look| look.state);
        let state = action
            .states
            .get(state_index)
            .or_else(|| action.states.first());
        let face = look.and_then(|look| look.states.get(&state_index));
        // Plugin images can be megabytes of data URI, so they are shared rather than copied.
        let image = face
            .and_then(|face| face.image.clone())
            .or_else(|| look.and_then(|look| look.face.image.clone()))
            .or_else(|| {
                state
                    .filter(|state| !state.image.is_empty())
                    .map(|state| Arc::from(state.image.as_str()))
            })
            .or_else(|| (!action.icon.is_empty()).then(|| Arc::from(action.icon.as_str())));
        let title = face
            .and_then(|face| face.title.clone())
            .or_else(|| look.and_then(|look| look.face.title.clone()))
            .or_else(|| {
                state
                    .filter(|state| !state.title.is_empty())
                    .map(|state| state.title.clone())
            })
            .or_else(|| image.is_none().then(|| action.name.clone()));
        Some(RenderSpec {
            root: self.plugins[owner].root.clone(),
            image,
            title,
            flash: look
                .and_then(|look| look.flash)
                .filter(|(_, at)| at.elapsed() < Duration::from_millis(850))
                .map(|(ok, _)| ok),
        })
    }

    /// Applies setTitle, setImage, setState, showOk or showAlert to a binding whose action has
    /// `states` states.
    pub(super) fn change(&mut self, context: &str, change: Change, payload: &Value, states: usize) {
        let state_count = states.max(1);
        let state = payload["state"]
            .as_u64()
            .and_then(|state| usize::try_from(state).ok());
        if state.is_some_and(|state| state >= state_count)
            || change == Change::State && state.is_none()
        {
            return;
        }
        let target = payload["target"].as_u64().unwrap_or(0);
        let image = payload["image"]
            .as_str()
            .filter(|img| img.len() <= MAX_MESSAGE - 1024)
            .map(Arc::<str>::from);
        let title = payload["title"]
            .as_str()
            .map(|title| title.chars().take(180).collect::<String>());
        let looks = self.appearances.entry(context.to_owned()).or_default();
        for (side, appearance) in [(1, &mut looks.hardware), (2, &mut looks.editor)] {
            if target != 0 && target != side {
                continue;
            }
            match change {
                Change::Title | Change::Image => {
                    let face = if let Some(state) = state {
                        appearance.states.entry(state).or_default()
                    } else {
                        for face in appearance.states.values_mut() {
                            if change == Change::Title {
                                face.title = None;
                            } else {
                                face.image = None;
                            }
                        }
                        &mut appearance.face
                    };
                    if change == Change::Title {
                        face.title = title.clone();
                    } else {
                        face.image = image.clone();
                    }
                }
                Change::State => appearance.state = state.unwrap_or_default(),
                Change::Ok | Change::Alert => {
                    appearance.flash = Some((change == Change::Ok, Instant::now()))
                }
            }
            appearance.revision += 1;
        }
    }
}
