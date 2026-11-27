use std::path::{Path, PathBuf};

use anyhow::Result;
use eframe::egui::{self, Align, Context, Layout, RichText, Ui};

use crate::{
    actions::{Action, ActionKind},
    agent_layout::Preset,
    profile::{Control, Direction},
    theme::{self, BODY, DETAIL, DialogAction, ERROR, MUTED},
};

use super::{Targets, action_fields, mode_page};

/// A mode supplies its saved binding format and provider-specific choices.
/// Control navigation, action groups, and save/cancel behavior live here.
pub(super) trait Assignments: Clone + Default {
    type Binding: Clone + PartialEq;
    type Catalog: ?Sized;
    const NAME: &'static str;

    fn binding(&self, control: Control) -> &Self::Binding;
    fn binding_mut(&mut self, control: Control) -> &mut Self::Binding;

    fn preset(&self) -> Preset;
    fn set_preset(&mut self, preset: Preset);
    fn defaults(&self) -> Self {
        let mut defaults = Self::default();
        defaults.set_preset(self.preset());
        defaults
    }

    fn choices(control: Control) -> impl Iterator<Item = Self::Binding>;
    fn label(binding: &Self::Binding, control: Control) -> &str;

    fn same_choice(current: &Self::Binding, choice: &Self::Binding) -> bool {
        current == choice
    }

    fn action_name(kind: ActionKind, _control: Control) -> &'static str {
        kind.label()
    }

    fn from_action(action: Action) -> Self::Binding;
    fn action(binding: &mut Self::Binding) -> Option<&mut Action>;
    fn details(
        ui: &mut Ui,
        control: Control,
        binding: &mut Self::Binding,
        catalog: &Self::Catalog,
        search: &mut String,
    );
}

/// An agent layout that is saved in its own file next to the workspace.
pub(super) trait Stored: Assignments + PartialEq {
    fn file_path(profile: &Path) -> PathBuf;
    fn read(path: &Path) -> Result<Self>;
    fn write(&self, path: &Path, previous: &Self) -> Result<()>;
}

/// The editor side of one agent mode: its saved layout, why that layout could not be loaded,
/// and the assignment editor when it is open.
pub(super) struct AgentState<L> {
    pub(super) saved: L,
    pub(super) load_error: Option<String>,
    pub(super) editor: Option<Editor<L>>,
}

impl<L: Stored> AgentState<L> {
    /// Loads the layout saved next to `profile`, or starts from the defaults.
    pub(super) fn load(profile: &Result<PathBuf>) -> Self {
        let loaded = profile
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error:#}"))
            .and_then(|profile| L::read(&L::file_path(profile)));
        let (saved, load_error) = match loaded {
            Ok(saved) => (saved, None),
            Err(error) => (L::default(), Some(format!("{error:#}"))),
        };
        Self {
            saved,
            load_error,
            editor: None,
        }
    }

    /// Saves `next`, unless it matches a layout that loaded cleanly. Returns whether the saved
    /// layout changed.
    pub(super) fn save(&mut self, profile: &Result<PathBuf>, next: L) -> Result<bool> {
        if next == self.saved && self.load_error.is_none() {
            return Ok(false);
        }
        let profile = profile
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error:#}"))?;
        next.write(&L::file_path(profile), &self.saved)?;
        self.saved = next;
        self.load_error = None;
        Ok(true)
    }
}

pub(super) struct Editor<A> {
    pub(super) target: Option<Control>,
    pub(super) draft: A,
    pub(super) error: Option<String>,
    search: String,
}

impl<A: Assignments> Editor<A> {
    pub(super) fn new(target: Option<Control>, draft: A) -> Self {
        Self {
            target,
            draft,
            error: None,
            search: String::new(),
        }
    }

    pub(super) fn show(&mut self, ctx: &Context, catalog: &A::Catalog) -> DialogAction {
        let confirm = if self.target.is_some() {
            "Save"
        } else {
            "Reset layout"
        };
        let dialog = theme::modal(ctx, "agent-layout-editor", |ui| {
            if let Some(control) = self.target {
                self.form(ui, control, catalog);
            } else {
                theme::dialog_title(ui, format!("Reset {} layout?", A::NAME));
                ui.label(
                    RichText::new("Restore the default assignments for every key and dial.")
                        .color(MUTED),
                );
            }
            if let Some(error) = &self.error {
                ui.label(RichText::new(error).size(DETAIL).color(ERROR));
            }
            theme::dialog_actions(ui, confirm, true)
        });
        match dialog.inner {
            DialogAction::None if dialog.should_close() => DialogAction::Cancel,
            intent => intent,
        }
    }

    fn form(&mut self, ui: &mut Ui, mut control: Control, catalog: &A::Catalog) {
        let heading = match control {
            Control::Dial(index, _) => format!("Edit dial {}", index + 1),
            Control::Button(_) => format!("Edit {}", control.label()),
        };
        theme::dialog_title(ui, heading);
        if let Control::Dial(index, selected) = control {
            ui.horizontal(|ui| {
                for direction in Direction::ALL {
                    let gesture = Control::Dial(index, direction);
                    if theme::tab(ui, direction.label(), 78.0, direction == selected)
                        .on_hover_text(A::label(self.draft.binding(gesture), gesture))
                        .clicked()
                        && gesture != control
                    {
                        control = gesture;
                        self.target = Some(gesture);
                        self.search.clear();
                        self.error = None;
                    }
                }
            });
        }
        let use_default = ui
            .horizontal(|ui| {
                ui.label(RichText::new("Assigned action").size(BODY).color(MUTED));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    theme::action_button(ui, "Use default", false, true).clicked()
                })
                .inner
            })
            .inner;
        if use_default {
            let default = self.draft.defaults().binding(control).clone();
            *self.draft.binding_mut(control) = default;
            self.search.clear();
            self.error = None;
        }

        let binding = self.draft.binding_mut(control);
        let title = A::label(binding, control).to_owned();
        let mut changed = false;
        theme::select(
            ui,
            ("assignment", control),
            &title,
            ui.available_width(),
            |ui| {
                for choice in A::choices(control) {
                    let selected = A::same_choice(binding, &choice);
                    if theme::option(ui, A::label(&choice, control), selected) && !selected {
                        *binding = choice;
                        changed = true;
                    }
                }
                for (group, kinds) in ActionKind::AGENT_GROUPS {
                    theme::menu_group(ui, group);
                    for &kind in kinds {
                        let selected = A::action(binding).is_some_and(|action| action.kind == kind);
                        if theme::option(ui, A::action_name(kind, control), selected) && !selected {
                            *binding = A::from_action(Action::from_kind(kind));
                            changed = true;
                        }
                    }
                }
            },
        );
        if changed {
            self.search.clear();
            self.error = None;
        }
        if let Some(action) = A::action(binding) {
            let id = ui.id().with(("assignment-value", control));
            action_fields(ui, action, &Targets::default(), id);
        }
        A::details(ui, control, binding, catalog, &mut self.search);
    }
}

pub(super) fn assignment_picker<'a, Id: Copy + PartialEq>(
    ui: &mut Ui,
    search: &mut String,
    hint: &str,
    empty: &str,
    selected: Option<Id>,
    items: impl Iterator<Item = (Id, &'a str)>,
) -> Option<Id> {
    theme::search(ui, search, hint, usize::MAX);
    let query = search.trim().to_lowercase();
    mode_page::surface()
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("assignment-picker")
                .max_height(132.0)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let mut found = false;
                    let mut chosen = None;
                    for (id, name) in items.filter(|(_, name)| {
                        query.is_empty() || name.to_lowercase().contains(&query)
                    }) {
                        found = true;
                        if theme::choice_row(ui, name, selected == Some(id)).clicked() {
                            chosen = Some(id);
                        }
                    }
                    if !found {
                        ui.label(RichText::new(empty).size(DETAIL).color(MUTED));
                    }
                    chosen
                })
                .inner
        })
        .inner
}
