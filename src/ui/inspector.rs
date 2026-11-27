//! The inspector for the selected control's binding and LCD appearance.

use super::*;

impl DeckX {
    pub(super) fn targets(&self) -> Targets {
        let profile = self
            .draft
            .profile(self.scene.profile)
            .expect("editor has a profile");
        let mut targets = Targets {
            profiles: self
                .draft
                .profiles
                .iter()
                .map(|p| (p.id, p.name.clone()))
                .collect(),
            ..Default::default()
        };
        for page in &profile.pages {
            let collection = if page.folder {
                &mut targets.folders
            } else {
                &mut targets.pages
            };
            collection.push((page.id, page.name.clone()));
        }
        targets
    }

    pub(super) fn inspector(&mut self, ctx: &Context) {
        let selected = self.selected;
        let kind = self.page().layout.action(selected).kind;
        let (category, action) = if kind == ActionKind::Plugin {
            let binding = self.page().layout.action(selected);
            self.plugin_item(&binding.value)
                .map(|item| (Some(item.category), item.name))
                .unwrap_or_else(|| (None, kind.label().into()))
        } else {
            let category = GROUPS
                .iter()
                .find(|(_, kinds, _)| kinds.contains(&kind))
                .map(|(category, _, _)| (*category).to_owned());
            (category, kind.label().into())
        };
        let height = ctx.content_rect().height();
        // Reserve space for the header, device controls and page strip when the window shrinks.
        let max_height = (height - 395.0).min(height * 0.48).max(150.0);
        let default_height = (height * 0.42 - 36.0).clamp(150.0, max_height);
        egui::TopBottomPanel::bottom("inspector")
            .default_height(default_height)
            .height_range(150.0..=max_height)
            .resizable(true)
            .frame(theme::frame(CANVAS, 0).stroke(Stroke::new(1.0, DIVIDER)))
            .show(ctx, |ui| {
                ui.add_space(GAP);
                let width = editor_width(ui);
                theme::centered_column(ui, width, |ui| {
                    let control_label = selected.label();
                    let text_width = ui
                        .painter()
                        .layout_no_wrap(control_label.clone(), FontId::proportional(DETAIL), MUTED)
                        .size()
                        .x
                        .ceil();
                    let trailing = text_width + if kind == ActionKind::None { 0.0 } else { 36.0 };
                    theme::split_row(
                        ui,
                        trailing,
                        28.0,
                        |ui| {
                            let mut heading = egui::text::LayoutJob::default();
                            if let Some(category) = category {
                                heading.append(
                                    &format!("{category}:"),
                                    0.0,
                                    egui::TextFormat::simple(theme::bold(DETAIL), TEXT),
                                );
                            }
                            heading.append(
                                &action,
                                if heading.text.is_empty() { 0.0 } else { 8.0 },
                                egui::TextFormat::simple(FontId::proportional(DETAIL), TEXT),
                            );
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.add(egui::Label::new(heading).truncate());
                            });
                        },
                        |ui| {
                            if kind != ActionKind::None
                                && theme::icon_button(ui, Symbol::Trash, "Clear binding").clicked()
                            {
                                self.assign(selected, ActionKind::None);
                            }
                            ui.label(RichText::new(control_label).size(DETAIL).color(MUTED));
                        },
                    );
                    ui.separator();
                });
                let targets = self.targets();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(GAP);
                        theme::centered_column(ui, width, |ui| {
                            ui.horizontal_top(|ui| {
                                ui.spacing_mut().item_spacing.x = PAGE_INSET;
                                let lcd = match selected {
                                    Control::Button(index) if index < DISPLAY_KEYS => {
                                        self.display_preview(ui, index);
                                        Some(index)
                                    }
                                    _ => None,
                                };
                                ui.vertical(|ui| {
                                    ui.set_width(ui.available_width());
                                    if let Some(index) = lcd {
                                        self.display_editor(ui, index);
                                    }
                                    self.binding_editor(ui, selected, &targets);
                                });
                            });
                        });
                        ui.add_space(SECTION_GAP);
                    });
            });
    }

    pub(super) fn binding_editor(&mut self, ui: &mut Ui, selected: Control, targets: &Targets) {
        let mut binding = self.page().layout.action(selected).clone();
        let previous = binding.kind;
        theme::form_row(ui, "Action:", |ui| {
            action_kind(ui, Id::new("binding-kind"), &mut binding, false);
        });
        if binding.kind != previous && previous == ActionKind::Plugin {
            self.assign(selected, binding.kind);
        }
        if binding.kind == ActionKind::Plugin {
            let item = self.plugin_item(&binding.value);
            ui.label(
                item.as_ref()
                    .map(|item| item.name.as_str())
                    .unwrap_or(&binding.value),
            );
            if item.as_ref().is_some_and(|item| item.inspector)
                && theme::action_button(ui, "Configure plugin…", false, true).clicked()
            {
                let result = (|| {
                    let host = self
                        .plugins
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Plugin host is unavailable"))?;
                    let context = binding
                        .plugin_context
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("Missing binding context"))?;
                    host.inspect(Instance {
                        action: binding.value.clone(),
                        context,
                        control: selected,
                    })
                })();
                if let Err(error) = result {
                    self.report(format!("{error:#}"), true);
                }
            }
        } else {
            if let Some(label) = field_label(binding.kind) {
                theme::form_row(ui, label, |ui| {
                    action_fields(ui, &mut binding, targets, Id::new("binding-value"));
                });
            } else {
                action_fields(ui, &mut binding, targets, Id::new("binding-value"));
            }
            if binding.kind == ActionKind::MultiAction {
                sequence_editor(ui, &mut binding.steps, targets);
            }
        }
        if binding != *self.page().layout.action(selected) {
            *self.page_mut().layout.action_mut(selected) = binding;
        }
    }

    pub(super) fn display_preview(&mut self, ui: &mut Ui, index: usize) {
        let button = self.page().layout.buttons[index].clone();
        let mut color = button.color.unwrap_or(TILE_COLORS[index]);
        let has_image = button.image.is_some();
        let live_preview = if !has_image {
            self.plugin_preview(ui.ctx(), index, &button)
        } else {
            None
        };
        if self.preview.as_ref().is_none_or(|cached| {
            cached.scene != self.scene || cached.index != index || cached.button != button
        }) {
            let texture = self.page().layout.tile(index).ok().map(|image| {
                ui.ctx().load_texture(
                    "LCD preview",
                    deck_face::lcd_image(&image),
                    egui::TextureOptions::NEAREST,
                )
            });
            self.preview = Some(KeyPreview {
                scene: self.scene,
                index,
                button,
                texture,
            });
        }
        let preview = live_preview.or_else(|| {
            self.preview
                .as_ref()
                .and_then(|cached| cached.texture.as_ref())
                .map(TextureHandle::id)
        });
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(PREVIEW_SIZE), Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, PREVIEW_RADIUS, WELL);
        let image_rect = rect.shrink(PREVIEW_INSET);
        if let Some(texture) = preview {
            painter.image(
                texture,
                image_rect,
                Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            painter.rect_filled(
                image_rect,
                4,
                Color32::from_rgb(color[0], color[1], color[2]),
            );
            theme::centered_text(
                &painter,
                rect.center(),
                "!",
                FontId::monospace(theme::EMPTY_TITLE),
                ERROR,
            );
        }
        painter.rect_stroke(
            rect,
            PREVIEW_RADIUS,
            Stroke::new(PREVIEW_BORDER, LINE),
            StrokeKind::Inside,
        );
        let choose = theme::preview_button(
            ui,
            rect.left_top() + Vec2::splat(INSET),
            "+",
            "Choose key image",
        );
        if choose.clicked() {
            self.choose_image(index);
        }
        let menu = theme::preview_button(
            ui,
            rect.right_top() + vec2(-INSET, INSET),
            "⌄",
            "Key appearance",
        );
        theme::menu(&menu, |ui| {
            ui.horizontal(|ui| {
                if ui.color_edit_button_srgb(&mut color).changed() {
                    self.page_mut().layout.buttons[index].color = Some(color);
                }
                ui.label("Key color");
            });
            if theme::menu_item(ui, "Choose image…", true) {
                self.choose_image(index);
            }
            if has_image && theme::menu_item(ui, "Remove image", true) {
                self.page_mut().layout.buttons[index].image = None;
            }
        });
    }

    pub(super) fn display_editor(&mut self, ui: &mut Ui, index: usize) {
        let mut label = self.page().layout.buttons[index].label.clone();
        theme::form_row(ui, "Title:", |ui| {
            if theme::input(ui, &mut label, "LCD label", profile::LABEL_CHARS).changed() {
                self.page_mut().layout.buttons[index].label = label;
            }
        });
    }

    pub(super) fn choose_image(&mut self, index: usize) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Key image", &["png", "jpg", "jpeg"])
            .pick_file()
        {
            self.photos.remove(&path);
            self.preview = None;
            self.page_mut().layout.buttons[index].image = Some(path);
        }
    }
}

pub(super) fn field_label(kind: ActionKind) -> Option<&'static str> {
    Some(match kind {
        ActionKind::Shortcut => "Hotkey:",
        ActionKind::TypeText => "Text:",
        ActionKind::OpenApp | ActionKind::OpenFile => "Path:",
        ActionKind::OpenWebsite => "URL:",
        ActionKind::CodexOpenChat => "Chat:",
        ActionKind::CodexNewChat => "Mode:",
        ActionKind::GoToPage => "Page:",
        ActionKind::OpenFolder => "Folder:",
        ActionKind::SwitchProfile => "Profile:",
        _ => return None,
    })
}

pub(super) fn action_kind(ui: &mut Ui, id: Id, action: &mut Action, step: bool) {
    let mut kind = action.kind;
    theme::select(ui, id, kind.label(), ui.available_width(), |ui| {
        theme::select_value(ui, &mut kind, ActionKind::None, ActionKind::None.label());
        for (group, kinds, _) in GROUPS {
            if step && matches!(group, "Navigation" | "DeckX" | "Multi Action") {
                continue;
            }
            theme::menu_group(ui, group);
            for item in kinds {
                theme::select_value(ui, &mut kind, *item, item.label());
            }
        }
        if step {
            theme::select_value(ui, &mut kind, ActionKind::Delay, ActionKind::Delay.label());
        }
    });
    if kind != action.kind {
        *action = Action::from_kind(kind);
    }
}

pub(super) fn action_fields(ui: &mut Ui, action: &mut Action, targets: &Targets, id: Id) {
    use ActionKind::*;
    let hint = match action.kind {
        Shortcut => "Ctrl+Shift+M",
        OpenApp => "Path to an .exe",
        OpenFile => "Path to a file or folder",
        OpenWebsite => "https://example.com",
        CodexOpenChat => "codex://threads/…",
        Delay => "Milliseconds",
        TypeText | CodexDraft => {
            let hint = if action.kind == CodexDraft {
                "Prompt to put in the Codex composer"
            } else {
                "Text to type"
            };
            theme::multiline(ui, &mut action.value, hint);
            return;
        }
        CodexNewChat => {
            let modes = [("codex", "Codex"), ("work", "Work"), ("chat", "ChatGPT")];
            let label = modes
                .iter()
                .find(|(mode, _)| *mode == action.value)
                .map_or("Codex", |(_, name)| *name);
            theme::select(ui, id, label, ui.available_width(), |ui| {
                for (mode, name) in modes {
                    theme::select_value(ui, &mut action.value, mode.into(), name);
                }
            });
            return;
        }
        GoToPage | OpenFolder | SwitchProfile => {
            let choices = match action.kind {
                GoToPage => &targets.pages,
                OpenFolder => &targets.folders,
                _ => &targets.profiles,
            };
            let label = choices
                .iter()
                .find(|(id, _)| action.value.parse() == Ok(*id))
                .map(|(_, name)| name.as_str());
            theme::select(
                ui,
                id,
                label.unwrap_or("Choose destination…"),
                ui.available_width(),
                |ui| {
                    for (id, name) in choices {
                        theme::select_value(ui, &mut action.value, id.to_string(), name);
                    }
                },
            );
            if choices.is_empty() && action.kind == OpenFolder {
                ui.label(
                    RichText::new("Create a folder from the Folders menu.")
                        .color(MUTED)
                        .size(CAPTION),
                );
            }
            return;
        }
        _ => return,
    };
    theme::input(ui, &mut action.value, hint, usize::MAX);
    match action.kind {
        OpenApp | OpenFile => {
            ui.horizontal(|ui| {
                if theme::action_button(ui, "Browse files…", false, true).clicked() {
                    let mut dialog = rfd::FileDialog::new();
                    if action.kind == OpenApp {
                        dialog = dialog.add_filter("Windows application", &["exe"]);
                    }
                    if let Some(path) = dialog.pick_file() {
                        action.value = path.to_string_lossy().into_owned();
                    }
                }
                if action.kind == OpenFile
                    && theme::action_button(ui, "Browse folders…", false, true).clicked()
                    && let Some(path) = rfd::FileDialog::new().pick_folder()
                {
                    action.value = path.to_string_lossy().into_owned();
                }
            });
        }
        Delay => {
            ui.label(RichText::new("Milliseconds").size(CAPTION).color(MUTED));
        }
        _ => {}
    }
}

pub(super) fn sequence_editor(ui: &mut Ui, steps: &mut Vec<Action>, targets: &Targets) {
    ui.separator();
    ui.label(theme::label("Steps"));
    let length = steps.len();
    let mut remove = None;
    let mut swap = None;
    for (i, action) in steps.iter_mut().enumerate() {
        ui.push_id(i, |ui| {
            theme::card(PANEL, GAP as i8).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(theme::label(format!("Step {}", i + 1)));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if theme::text_button(ui, "×", FIELD_HEIGHT, false, true)
                            .on_hover_text("Remove step")
                            .clicked()
                        {
                            remove = Some(i);
                        }
                        if theme::text_button(ui, "↓", FIELD_HEIGHT, false, i + 1 < length)
                            .on_hover_text("Move down")
                            .clicked()
                        {
                            swap = Some((i, i + 1));
                        }
                        if theme::text_button(ui, "↑", FIELD_HEIGHT, false, i > 0)
                            .on_hover_text("Move up")
                            .clicked()
                        {
                            swap = Some((i, i - 1));
                        }
                    });
                });
                action_kind(ui, Id::new("step-kind"), action, true);
                action_fields(ui, action, targets, Id::new("step-fields"));
            });
        });
    }
    if let Some(i) = remove {
        steps.remove(i);
    } else if let Some((a, b)) = swap {
        steps.swap(a, b);
    }
    if theme::action_button(ui, "+ Add step", false, steps.len() < 32).clicked() {
        steps.push(Action::default());
    }
}
