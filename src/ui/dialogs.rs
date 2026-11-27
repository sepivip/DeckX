//! Dialogs that add, rename, and remove profiles and pages.

use super::*;

impl DeckX {
    pub(super) fn menu_dialog(&mut self, ui: &mut Ui, dialog: Dialog, label: &str) {
        if theme::menu_item(ui, label, true) {
            self.begin_dialog(dialog);
        }
    }

    pub(super) fn begin_dialog(&mut self, dialog: Dialog) {
        let name = match dialog {
            Dialog::RenameProfile => self
                .draft
                .profile(self.scene.profile)
                .expect("editor has a profile")
                .name
                .clone(),
            Dialog::RenamePage => self.page().name.clone(),
            _ => String::new(),
        };
        self.dialog = Some(ScenePrompt {
            dialog,
            target: self.scene,
            name,
            error: None,
            focus_name: !dialog.removes(),
        });
    }

    pub(super) fn prompt(&mut self, ctx: &Context) {
        let Some(mut prompt) = self.dialog.take() else {
            return;
        };
        let dialog = prompt.dialog;
        let modal = theme::modal(ctx, "scene-dialog", |ui| {
            theme::dialog_title(ui, dialog.title());
            let mut enter = false;
            if dialog.removes() {
                ui.label(
                    RichText::new("This removes the bindings from the workspace when you save.")
                        .color(MUTED),
                );
            } else {
                let field =
                    theme::input(ui, &mut prompt.name, "Name", crate::workspace::NAME_CHARS);
                if prompt.focus_name && ui.is_enabled() && !ui.is_sizing_pass() {
                    field.request_focus();
                    prompt.focus_name = false;
                }
                if field.changed() {
                    prompt.error = None;
                }
                enter = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            }
            if let Some(message) = &prompt.error {
                ui.label(RichText::new(message).size(DETAIL).color(ERROR));
            }
            let valid = dialog.removes() || !prompt.name.trim().is_empty();
            let actions =
                theme::dialog_actions(ui, if dialog.removes() { "Remove" } else { "Done" }, valid);
            if enter && valid {
                DialogAction::Confirm
            } else {
                actions
            }
        });
        if modal.inner == DialogAction::Confirm {
            match self.apply_dialog(dialog, prompt.name.trim().to_owned(), prompt.target) {
                Ok(()) => return,
                Err(problem) => prompt.error = Some(format!("{problem:#}")),
            }
        }
        if modal.inner != DialogAction::Cancel && !modal.should_close() {
            self.dialog = Some(prompt);
        }
    }

    /// Applies a dialog to the page or profile it was opened for. The deck can change
    /// the active page while the dialog is open.
    pub(super) fn apply_dialog(
        &mut self,
        dialog: Dialog,
        name: String,
        target: Scene,
    ) -> Result<()> {
        if !dialog.removes() {
            ensure!(!name.is_empty(), "Enter a name");
        }
        let scene = match dialog {
            Dialog::NewProfile => self.draft.add_profile(name)?,
            Dialog::NewPage | Dialog::NewFolder => {
                self.draft
                    .add_page(target.profile, name, matches!(dialog, Dialog::NewFolder))?
            }
            Dialog::RenameProfile => {
                self.draft
                    .profile_mut(target.profile)
                    .context("The profile was removed")?
                    .name = name;
                return Ok(());
            }
            Dialog::RenamePage => {
                self.draft
                    .page_mut(target)
                    .context("The page was removed")?
                    .name = name;
                return Ok(());
            }
            Dialog::DeleteProfile => self.draft.remove_profile(target.profile)?,
            Dialog::DeletePage => self.draft.remove_page(target)?,
        };
        self.select_scene(scene);
        Ok(())
    }
}
