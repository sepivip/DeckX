//! The Settings window.

use super::*;

impl DeckX {
    pub(super) fn settings(&mut self, ctx: &Context) {
        let mut open = self.settings;
        let profile_id = self.scene.profile;
        let max_content_height = (ctx.content_rect().height() - 244.0).max(160.0);
        theme::panel_window("Settings", &mut open, TOOL_WINDOW_WIDTH)
            .default_height(max_content_height + 44.0)
            .show(ctx, |ui| {
                ui.set_width(TOOL_WINDOW_WIDTH);
                ui.horizontal(|ui| {
                    for (tab, label) in [
                        (mcp_settings::Tab::General, "General"),
                        (mcp_settings::Tab::Mcp, "MCP"),
                    ] {
                        let selected = tab == self.settings_tab;
                        if theme::tab(ui, label, 72.0, selected).clicked() && !selected {
                            self.settings_tab = tab;
                            self.mcp_settings.refresh_registration();
                        }
                    }
                });
                ui.separator();
                // Reserve enough room beside the controls even when the scrollbar expands.
                let scroll = &mut ui.style_mut().spacing.scroll;
                scroll.floating_allocated_width = scroll.bar_width + scroll.bar_inner_margin;
                egui::ScrollArea::vertical()
                    .id_salt(match self.settings_tab {
                        mcp_settings::Tab::General => "settings-general",
                        mcp_settings::Tab::Mcp => "settings-mcp",
                    })
                    .max_height(max_content_height)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        if self.settings_tab == mcp_settings::Tab::Mcp {
                            self.show_mcp_settings(ui);
                            return;
                        }
                        theme::heading(ui, "Windows");
                        self.startup_setting(ui);
                        self.tray_setting(ui);
                        ui.separator();
                        theme::heading(ui, "Deck display");
                        theme::field_setting(ui, "Brightness", theme::brightness_width(ui), |ui| {
                            theme::brightness_slider(ui, &mut self.draft.brightness);
                        });
                        theme::field_setting(ui, "LCD image rotation", 174.0, |ui| {
                            let value = format!("{}°", self.draft.rotation);
                            theme::select(ui, "rotation", &value, 170.0, |ui| {
                                for degrees in [0, 90, 180, 270] {
                                    theme::select_value(ui, &mut self.draft.rotation, degrees, &format!("{degrees}°"));
                                }
                            });
                        });
                        self.easter_egg_setting(ui);
                        ui.separator();
                        if let Some(profile) = self.draft.profile_mut(profile_id) {
                            let name = format!("App association · {}", profile.name);
                            theme::row_title(ui, &name);
                            let app = profile.app.as_ref().map(|app| app.display().to_string());
                            let association = app.as_deref().unwrap_or("Switch to this profile when its linked app is in focus.");
                            theme::clipped_text(ui, association, BODY, MUTED);
                            theme::right_aligned_row(ui, FIELD_HEIGHT, |ui| {
                                if theme::action_button(ui, if app.is_some() { "Change application…" } else { "Link application…" }, false, true)
                                    .on_hover_text(
                                        "Activate this profile when the selected app has focus",
                                    )
                                    .clicked()
                                    && let Some(path) = rfd::FileDialog::new()
                                        .add_filter("Windows application", &["exe"])
                                        .pick_file()
                                {
                                    profile.app = Some(path);
                                }
                                if app.is_some() && theme::action_button(ui, "Unlink", false, true).clicked() {
                                    profile.app = None;
                                }
                            });
                        }
                        ui.separator();
                        theme::heading(ui, "Workspace");
                        ui.label(RichText::new("Back up or transfer your profiles and bindings.").color(MUTED));
                        theme::right_aligned_row(ui, FIELD_HEIGHT, |ui| {
                            if theme::action_button(ui, "Import workspace…", false, true)
                                .on_hover_text(
                                    "Loads a workspace into the editor. Save to apply it.",
                                )
                                .clicked()
                                && let Some(path) = rfd::FileDialog::new()
                                    .add_filter("DeckX workspace", &["json"])
                                    .pick_file()
                            {
                                self.settings_error = None;
                                match Workspace::load(&path) {
                                    Ok(workspace) => {
                                        self.scene = workspace.home;
                                        self.draft = workspace;
                                        self.selected = Control::Button(0);
                                        self.photos.clear();
                                        self.preview = None;
                                        self.report("Workspace loaded into the editor", false);
                                    }
                                    Err(error) => self.settings_error = Some(format!("{error:#}")),
                                }
                            }
                            if theme::action_button(ui, "Export workspace…", false, true).clicked()
                                && let Some(path) = rfd::FileDialog::new()
                                    .set_file_name("DeckX.json")
                                    .add_filter("DeckX workspace", &["json"])
                                    .save_file()
                            {
                                self.settings_error = None;
                                match self.draft.save(&path) {
                                    Ok(()) => self.report("Workspace exported", false),
                                    Err(error) => self.settings_error = Some(format!("{error:#}")),
                                }
                            }
                        });
                    });
                if self.settings_tab == mcp_settings::Tab::General {
                    ui.separator();
                    let dirty = self.draft != self.saved;
                    if let Some(error) = &self.settings_error {
                        theme::clipped_text(ui, error, BODY, ERROR);
                    }
                    let mut save = false;
                    theme::split_row(ui, 193.0, FIELD_HEIGHT, |ui| {
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            if dirty {
                                ui.label(RichText::new("Unsaved workspace changes").size(DETAIL).color(GOLD));
                            }
                        });
                    }, |ui| {
                        if dirty {
                            save = theme::action_button(ui, "Save workspace", true, self.can_save())
                                .on_hover_text("Saves these settings and any pending profile and binding edits")
                                .clicked();
                        }
                        if theme::action_button(ui, if dirty { "Close" } else { "Done" }, false, true).clicked() {
                            self.settings = false;
                        }
                    });
                    let shortcut = dirty && self.can_save() && ctx.input_mut(|input| {
                        input.consume_key(egui::Modifiers::CTRL, egui::Key::S)
                    });
                    if save || shortcut {
                        match self.save() {
                            Ok(()) => self.settings_error = None,
                            Err(error) => self.settings_error = Some(format!("{error:#}")),
                        }
                    }
                }
            });
        self.settings &= open;
    }

    fn startup_setting(&mut self, ui: &mut Ui) {
        if let Some(enabled) = switch_setting(
            ui,
            "Start DeckX when I sign in",
            self.startup.enabled,
            "Opens DeckX and tries to connect the deck at Windows sign-in",
        ) && (enabled != self.startup.enabled || self.startup.error.is_some())
        {
            self.startup.set(enabled);
        }
        if let Some(error) = &self.startup.error {
            ui.label(RichText::new(error).size(DETAIL).color(ERROR));
        }
    }

    fn tray_setting(&mut self, ui: &mut Ui) {
        let enabled = self.saved.minimize_to_tray;
        let next = ui
            .add_enabled_ui(self.path.is_ok() && !self.unreadable, |ui| {
                switch_setting(
                    ui,
                    "Minimize to tray",
                    enabled,
                    "Minimize from the title bar to keep the deck running in the background. Click the tray icon to reopen DeckX.",
                )
            })
            .inner;
        if let Some(next) = next
            && next != enabled
        {
            self.settings_error = self
                .save_tray_setting(next)
                .err()
                .map(|error| format!("{error:#}"));
            ui.ctx().request_repaint();
        }
    }

    fn save_tray_setting(&mut self, enabled: bool) -> Result<()> {
        self.save_workspace_setting(|workspace| workspace.minimize_to_tray = enabled)?;
        self.saved.minimize_to_tray = enabled;
        self.draft.minimize_to_tray = enabled;
        Ok(())
    }

    fn easter_egg_setting(&mut self, ui: &mut Ui) {
        let enabled = self.saved.easter_egg;
        let mut next = enabled;
        let label = |value| if value { "On" } else { "Off" };
        let description = format!(
            "Tap the left round button, then hold the right for {} seconds",
            crate::easter_egg::HOLD_TIME.as_secs()
        );
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = TIGHT;
            theme::field_setting(ui, "Easter egg", 110.0, |ui| {
                ui.add_enabled_ui(self.path.is_ok() && !self.unreadable, |ui| {
                    theme::select(ui, "easter-egg", label(enabled), 104.0, |ui| {
                        for value in [false, true] {
                            theme::select_value(ui, &mut next, value, label(value));
                        }
                    });
                });
            });
            theme::clipped_text(ui, &description, DETAIL, MUTED);
        });
        if next != enabled {
            self.settings_error = self
                .save_easter_egg(next)
                .err()
                .map(|error| format!("{error:#}"));
        }
    }

    fn save_easter_egg(&mut self, enabled: bool) -> Result<()> {
        self.save_workspace_setting(|workspace| workspace.easter_egg = enabled)?;
        if self.draft.easter_egg == self.saved.easter_egg {
            self.draft.easter_egg = enabled;
        }
        self.saved.easter_egg = enabled;
        if let Some(session) = &self.session
            && !matches!(session.connection, Connection::Closing)
        {
            session
                .requests
                .send(Request::EasterEgg(enabled))
                .map_err(|_| {
                    anyhow::anyhow!("Easter egg setting saved. Reconnect the deck to apply it")
                })?;
        }
        Ok(())
    }
}

fn switch_setting(ui: &mut Ui, label: &str, enabled: bool, help: &str) -> Option<bool> {
    let width = 52.0;
    let mut clicked = None;
    theme::field_setting(ui, label, 2.0 * width + ui.spacing().item_spacing.x, |ui| {
        for (value, caption) in [(true, "On"), (false, "Off")] {
            if theme::tab(ui, caption, width, value == enabled)
                .on_hover_text(help)
                .clicked()
            {
                clicked = Some(value);
            }
        }
    });
    clicked
}
