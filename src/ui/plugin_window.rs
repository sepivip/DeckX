//! The Plugins window and plugin package imports.

use super::*;

impl DeckX {
    pub(super) fn poll_plugin_import(&mut self, ctx: &Context) {
        let Some(import) = &self.plugin_import else {
            return;
        };
        let staged = match import.try_recv() {
            Ok(staged) => staged,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(Duration::from_millis(100));
                return;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(anyhow::anyhow!("The plugin import stopped unexpectedly"))
            }
        };
        self.plugin_import = None;
        let installed = staged.and_then(|staged| {
            self.plugins
                .as_mut()
                .context("Plugin host is unavailable")?
                .install(staged)
        });
        match installed {
            Ok(name) => {
                self.plugin_notice = Some(format!("{name} imported. Enable it to start it."));
            }
            Err(error) => {
                self.plugin_notice = None;
                self.plugin_error = Some(format!("{error:#}"));
            }
        }
    }

    pub(super) fn plugin_manager(&mut self, ctx: &Context) {
        let mut open = self.plugin_window;
        let height = (ctx.content_rect().height() - 198.0).clamp(320.0, 464.0);
        theme::panel_window("Plugins", &mut open, TOOL_WINDOW_WIDTH)
            .fixed_size(vec2(TOOL_WINDOW_WIDTH, height))
            .show(ctx, |ui| {
                ui.set_width(TOOL_WINDOW_WIDTH);
                ui.set_height(height);
                let Some(host) = &mut self.plugins else {
                    theme::empty_state(ui, "Plugins unavailable", self.plugin_error.as_deref().unwrap_or("DeckX could not start the plugin host."));
                    return;
                };
                ui.horizontal(|ui| {
                    if theme::tab(ui, "Installed", 84.0, !self.marketplace_tab).clicked() {
                        self.marketplace_tab = false;
                    }
                    if theme::tab(ui, "Marketplace", 102.0, self.marketplace_tab).clicked() {
                        self.marketplace_tab = true;
                        self.plugin_enable = None;
                    }
                });
                ui.separator();
                if self.marketplace_tab {
                    self.marketplace.show(ui, host);
                    return;
                }
                ui.horizontal(|ui| {
                    let can_import = self.plugin_import.is_none();
                    let package = theme::action_button(ui, "Import package…", false, can_import).on_hover_text("Open a downloaded .streamDeckPlugin file");
                    let folder = theme::action_button(ui, "Import folder…", false, can_import).on_hover_text("Choose an installed .sdPlugin folder");
                    let path = if package.clicked() {
                        rfd::FileDialog::new().add_filter("Stream Deck plugin", &["streamDeckPlugin"]).pick_file()
                    } else if folder.clicked() {
                        let mut dialog = rfd::FileDialog::new();
                        if let Some(root) = std::env::var_os("APPDATA").map(PathBuf::from).map(|root| root.join("Elgato/StreamDeck/Plugins"))
                            && root.is_dir()
                        {
                            dialog = dialog.set_directory(root);
                        }
                        dialog.pick_folder()
                    } else { None };
                    if let Some(path) = path {
                        self.plugin_error = None;
                        self.plugin_notice = Some("Importing the plugin…".into());
                        // Extraction can take a while for large packages, so stage it off the UI
                        // thread. `poll_plugin_import` registers the result.
                        let (done, result) = std::sync::mpsc::channel();
                        let packages = host.packages();
                        std::thread::spawn(move || {
                            let source = path.display().to_string();
                            let _ = done.send(crate::plugins::stage(&packages, &path, source));
                        });
                        self.plugin_import = Some(result);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let node = theme::action_button(ui, "Node.js…", false, true).on_hover_text(host.node().map(|path| path.display().to_string()).unwrap_or_else(|| "Using node.exe from PATH".into()));
                        if node.clicked() && let Some(path) = rfd::FileDialog::new().add_filter("Node runtime", &["exe"]).pick_file() {
                            self.plugin_notice = None;
                            self.plugin_error = None;
                            match host.set_node(path) {
                                Ok(()) => self.plugin_notice = Some("Node.js runtime updated.".into()),
                                Err(error) => self.plugin_error = Some(format!("{error:#}")),
                            }
                        }
                    });
                });
                ui.separator();
                let error = self.plugin_error.clone().or_else(|| host.error());
                if let Some((notice, color)) = error.as_ref().map(|message| (message, ERROR))
                    .or_else(|| self.plugin_notice.as_ref().map(|message| (message, MINT))) {
                    theme::clipped_text(ui, notice, BODY, color);
                }
                let list = host.installed();
                if list.is_empty() {
                    ui.vertical_centered(|ui| {
                        ui.add_space((ui.available_height() - 110.0).max(0.0) * 0.5);
                        theme::empty_state(ui, "No plugins installed", "Browse the Marketplace or import a plugin you already have.");
                        ui.add_space(GAP);
                        if theme::action_button(ui, "Browse Marketplace", true, true).clicked() {
                            self.marketplace_tab = true;
                        }
                    });
                    return;
                }
                let approval = self.plugin_enable.as_ref().and_then(|id| {
                    list.iter().find(|plugin| &plugin.id == id)
                        .map(|plugin| (plugin.id.clone(), plugin.name.clone(), plugin.source.clone()))
                });
                let removal_id = Id::new("uninstall-plugin");
                let removal = ctx.data(|data| data.get_temp::<String>(removal_id)).and_then(|id| {
                    list.iter().find(|plugin| plugin.id == id)
                        .map(|plugin| (plugin.id.clone(), plugin.name.clone()))
                });
                egui::ScrollArea::vertical().id_salt("installed-plugins").max_height(ui.available_height()).auto_shrink([false, true]).show(ui, |ui| {
                    for crate::plugins::Installed { id, name, detail, enabled, status, .. } in list {
                        ui.push_id(&id, |ui| {
                            let restart = enabled && !matches!(status.as_str(), "Running" | "Starting…");
                            let trailing = 36.0 + if restart { 150.0 } else { 80.0 };
                            let color = match status.as_str() {
                                "Running" => MINT,
                                "Disabled" => MUTED,
                                "Starting…" | "Waiting for plugin registration" => GOLD,
                                _ => ERROR,
                            };
                            theme::list_card(ui, trailing, &name, &detail, Some((&status, color)), |ui| {
                                if theme::icon_button(ui, Symbol::Trash, "Uninstall").clicked() {
                                    ui.data_mut(|data| data.insert_temp(removal_id, id.clone()));
                                }
                                if enabled && theme::text_button(ui, "Disable", 78.0, false, true).clicked() {
                                    self.plugin_notice = None;
                                    self.plugin_error = host.set_enabled(&id, false).err().map(|error| format!("{error:#}"));
                                }
                                if restart && theme::action_button(ui, "Restart", false, true).clicked() {
                                    self.plugin_notice = None;
                                    self.plugin_error = host.set_enabled(&id, true).err().map(|error| format!("{error:#}"));
                                }
                                if !enabled && theme::text_button(ui, "Enable…", 78.0, false, true).clicked() {
                                    self.plugin_enable = Some(id.clone());
                                }
                            });
                        });
                    }
                });
                if let Some((id, name, source)) = approval {
                    let dialog = theme::modal(ctx, "enable-plugin", |ui| {
                            theme::dialog_title(ui, format!("Enable {name}?"));
                            ui.label(RichText::new("Plugins run as desktop apps under your Windows account. Enable this plugin if you trust its publisher.").color(MUTED));
                            if let Some(source) = &source {
                                theme::clipped_text(ui, &format!("Source: {source}"), DETAIL, MUTED);
                            }
                            theme::dialog_actions(ui, "Enable plugin", true)
                        });
                    if dialog.inner == DialogAction::Confirm {
                        self.plugin_notice = None;
                        self.plugin_error = host.set_enabled(&id, true).err().map(|error| format!("{error:#}"));
                    }
                    if dialog.inner != DialogAction::None || dialog.should_close() {
                        self.plugin_enable = None;
                    }
                }
                if let Some((id, name)) = removal {
                    let dialog = theme::modal(ctx, "uninstall-plugin-dialog", |ui| {
                            theme::dialog_title(ui, format!("Uninstall {name}?"));
                            ui.label(RichText::new("DeckX removes the plugin and its settings. Keys and dials that use it stop working.").color(MUTED));
                            theme::dialog_actions(ui, "Uninstall", true)
                        });
                    if dialog.inner == DialogAction::Confirm {
                        self.plugin_notice = None;
                        self.plugin_error = None;
                        match host.uninstall(&id) {
                            Ok(name) => self.plugin_notice = Some(format!("{name} uninstalled.")),
                            Err(error) => self.plugin_error = Some(format!("{error:#}")),
                        }
                    }
                    if dialog.inner != DialogAction::None || dialog.should_close() {
                        ctx.data_mut(|data| data.remove::<String>(removal_id));
                    }
                }
            });
        self.plugin_window = open;
        if !open {
            self.plugin_enable = None;
            ctx.data_mut(|data| data.remove::<String>(Id::new("uninstall-plugin")));
        }
    }
}
