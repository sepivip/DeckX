//! The workspace editor: action palette, page strip, deck face, and drag and drop.

use super::*;

impl DeckX {
    pub(super) fn palette(&mut self, ctx: &Context) {
        egui::SidePanel::right("actions")
            .exact_width(271.0)
            .resizable(false)
            .frame(theme::frame(WELL, 0))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let mut all_open = None;
                theme::frame(PANEL, 0)
                    .inner_margin(PALETTE_MARGIN)
                    .show(ui, |ui| {
                        theme::split_row(
                            ui,
                            FIELD_HEIGHT,
                            FIELD_HEIGHT,
                            |ui| {
                                theme::search(ui, &mut self.query, "Search", usize::MAX);
                            },
                            |ui| {
                                let list =
                                    theme::icon_button(ui, Symbol::List, "Action list options");
                                theme::menu(&list, |ui| {
                                    for (label, open) in
                                        [("Expand all", true), ("Collapse all", false)]
                                    {
                                        if theme::menu_item(ui, label, true) {
                                            all_open = Some(open);
                                        }
                                    }
                                    if theme::menu_item(ui, "Manage plugins…", true) {
                                        self.settings = false;
                                        self.marketplace_tab = false;
                                        self.plugin_window = true;
                                    }
                                    if theme::menu_item(ui, "Elgato Marketplace…", true) {
                                        self.settings = false;
                                        self.marketplace_tab = true;
                                        self.plugin_window = true;
                                    }
                                });
                            },
                        );
                    });
                if all_open.is_some() {
                    self.query.clear();
                }
                let query = self.query.trim().to_lowercase();
                let searching = !query.is_empty();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let mut found = false;
                        for (category, kinds, symbol) in GROUPS {
                            let actions: Vec<_> = kinds
                                .iter()
                                .copied()
                                .filter(|kind| {
                                    kind.label().to_lowercase().contains(&query)
                                        || kind.tile_label().to_lowercase().contains(&query)
                                        || category.to_lowercase().contains(&query)
                                })
                                .collect();
                            if actions.is_empty() {
                                continue;
                            }
                            found = true;
                            if theme::palette_group(
                                ui,
                                ("builtin", category),
                                category,
                                symbol,
                                searching,
                                all_open,
                                category == "System",
                            ) {
                                for kind in actions {
                                    let response = theme::palette_action(
                                        ui,
                                        kind.label(),
                                        action_symbol(kind),
                                    );
                                    response.dnd_set_drag_payload(Drag::Action(kind));
                                    if response.clicked() {
                                        self.assign(self.selected, kind);
                                    }
                                }
                            }
                        }
                        if let Some(host) = &self.plugins {
                            let items: Vec<_> = host
                                .catalog()
                                .into_iter()
                                .filter(|item| {
                                    item.name.to_lowercase().contains(&query)
                                        || item.category.to_lowercase().contains(&query)
                                })
                                .collect();
                            for group in items.chunk_by(|a, b| a.category == b.category) {
                                let category = &group[0].category;
                                found = true;
                                if theme::palette_group(
                                    ui,
                                    ("plugin", category),
                                    category,
                                    Symbol::Plugins,
                                    searching,
                                    all_open,
                                    true,
                                ) {
                                    for item in group {
                                        let response =
                                            theme::palette_action(ui, &item.name, Symbol::Plugins)
                                                .on_hover_text(&item.tooltip);
                                        response
                                            .dnd_set_drag_payload(Drag::Plugin(item.uuid.clone()));
                                        if response.clicked() {
                                            self.assign_plugin(self.selected, item.clone(), None);
                                        }
                                    }
                                }
                            }
                        }
                        if !found {
                            ui.add_space(14.0);
                            ui.vertical_centered(|ui| {
                                ui.label(RichText::new("No matching actions").color(MUTED));
                            });
                        }
                    });
            });
    }

    pub(super) fn assign(&mut self, control: Control, kind: ActionKind) {
        self.selected = control;
        let layout = &mut self.page_mut().layout;
        if layout.action(control).kind != kind {
            layout.set_action(control, Action::from_kind(kind));
        }
    }

    pub(super) fn assign_plugin(
        &mut self,
        control: Control,
        item: CatalogItem,
        copy: Option<&str>,
    ) {
        if !plugin_fits(&item, control) {
            self.report(
                if item.dial {
                    "Drop this plugin action on a dial"
                } else {
                    "Drop this plugin action on a key"
                },
                true,
            );
            return;
        }
        self.selected = control;
        let action = Action::plugin(item.uuid);
        if let (Some(host), Some(from), Some(to)) = (&self.plugins, copy, &action.plugin_context) {
            lock(&host.hub()).duplicate_settings(&action.value, from, to);
        }
        self.page_mut().layout.set_action(control, action);
        self.report("Plugin action assigned. Save workspace to apply it.", false);
    }

    pub(super) fn plugin_item(&self, uuid: &str) -> Option<CatalogItem> {
        self.plugins.as_ref()?.item(uuid)
    }

    pub(super) fn center(&mut self, ctx: &Context) {
        egui::CentralPanel::default()
            .frame(theme::frame(CANVAS, 0))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.add_space(SECTION_GAP);
                theme::centered_column(ui, editor_width(ui), |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Profile").size(DETAIL).color(MUTED));
                        self.profile_picker(ui);
                    });
                });
                let stage_height = ui.available_height();
                let deck_width = (ui.available_width() - 40.0)
                    .min(456.0)
                    .min((stage_height - 64.0).max(42.0) * AKP03E.aspect());
                let group_height = deck_width / AKP03E.aspect() + 7.0 + SECTION_GAP + FIELD_HEIGHT;
                ui.add_space(((stage_height - group_height) / 2.0).max(0.0));
                ui.vertical_centered(|ui| self.deck(ui, deck_width));
                ui.add_space(SECTION_GAP);
                self.pages(ui);
            });
    }

    pub(super) fn pages(&mut self, ui: &mut Ui) {
        let profile = self.draft.profile(self.scene.profile).unwrap();
        let (folders, pages): (Vec<_>, Vec<_>) = profile
            .pages
            .iter()
            .map(|page| {
                (
                    Scene {
                        profile: profile.id,
                        page: page.id,
                    },
                    page.name.clone(),
                    page.folder,
                )
            })
            .partition(|(_, _, folder)| *folder);
        let caption = if self.page().folder {
            "Folder ▾"
        } else {
            "…"
        };
        let label_width = ui
            .painter()
            .layout_no_wrap("Pages:".into(), FontId::proportional(DETAIL), DIM)
            .size()
            .x
            .ceil();
        let menu_width = if self.page().folder {
            64.0
        } else {
            FIELD_HEIGHT
        };
        let track_width = (pages.len() as f32 * PAGE_CHIP_WIDTH)
            .min((ui.available_width() - label_width - menu_width - 52.0).max(PAGE_CHIP_WIDTH));
        let width = label_width + track_width + menu_width + 52.0;
        theme::centered_column(ui, width, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                ui.add_sized(
                    vec2(label_width, FIELD_HEIGHT),
                    egui::Label::new(RichText::new("Pages:").size(DETAIL).color(DIM)),
                );
                theme::page_track().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.spacing_mut().interact_size.y = CONTROL_COMPACT_HEIGHT;
                        ui.spacing_mut().button_padding.y = 0.0;
                        egui::ScrollArea::horizontal()
                            .max_width(track_width)
                            .min_scrolled_width(track_width)
                            .show(ui, |ui| {
                                for (index, (scene, name, _)) in pages.iter().enumerate() {
                                    let active = self.scene == *scene;
                                    let number = (index + 1).to_string();
                                    if theme::page_chip(ui, &number, active)
                                        .on_hover_text(name)
                                        .clicked()
                                    {
                                        self.select_scene(*scene);
                                    }
                                }
                            });
                        if theme::bare_button(ui, "+", 32.0, CONTROL_COMPACT_HEIGHT, INSET)
                            .on_hover_text("New page")
                            .clicked()
                        {
                            self.begin_dialog(Dialog::NewPage);
                        }
                    });
                });
                let menu = theme::bare_button(ui, caption, menu_width, FIELD_HEIGHT, BODY)
                    .on_hover_text("Page and folder options");
                theme::menu(&menu, |ui| {
                    for (scene, name, _) in &folders {
                        if theme::option(ui, name, self.scene == *scene) {
                            self.select_scene(*scene);
                        }
                    }
                    self.menu_dialog(ui, Dialog::NewFolder, "New folder…");
                    ui.separator();
                    self.menu_dialog(ui, Dialog::RenamePage, "Rename…");
                    if self.page().folder || pages.len() > 1 {
                        self.menu_dialog(ui, Dialog::DeletePage, "Remove…");
                    }
                });
            });
        });
    }

    pub(super) fn deck(&mut self, ui: &mut Ui, width: f32) {
        let geometry = DeckFace::allocate(ui, &AKP03E, width);
        let painter = ui.painter().clone();
        geometry.paint(&painter);
        for shape in geometry.controls() {
            let control = shape.control();
            if let FaceControl::Lcd { index, rect } = shape {
                let mut cell = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt(("lcd", index))
                        .max_rect(rect)
                        .layout(Layout::top_down(Align::Min)),
                );
                self.display_key(&mut cell, index, rect.size());
                continue;
            }
            let response =
                ui.interact(shape.rect(), ui.id().with(control), Sense::click_and_drag());
            let border = self.control_border(ui, control, &response);
            shape.paint(&painter, border);
            if let FaceControl::Round { center, dial, .. } = shape {
                let kind = self.page().layout.action(control).kind;
                let index = match control {
                    Control::Button(index) => index - DISPLAY_KEYS,
                    Control::Dial(index, _) => index,
                };
                let label = if kind == ActionKind::None {
                    (index + 1).to_string()
                } else {
                    glyph(kind).into()
                };
                theme::centered_text(
                    &painter,
                    center,
                    label,
                    FontId::proportional(if dial { 13.0 } else { 12.0 }),
                    if !dial && kind == ActionKind::None {
                        MUTED
                    } else {
                        TEXT
                    },
                );
            }
            self.control_interaction(response, control);
        }
    }

    pub(super) fn display_key(&mut self, ui: &mut Ui, index: usize, size: Vec2) {
        let control = Control::Button(index);
        let button = self.page().layout.buttons[index].clone();
        let color = button.color.unwrap_or(TILE_COLORS[index]);
        let texture = if let Some(path) = &button.image {
            self.photo(ui.ctx(), index, path)
        } else {
            self.plugin_preview(ui.ctx(), index, &button)
        };
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let border = self.control_border(ui, control, &response);
        let radius = deck_face::key_radius(rect);
        painter.rect_filled(
            rect,
            radius,
            Color32::from_rgb(color[0], color[1], color[2]),
        );
        let label = &button.label;
        if let Some(texture) = texture {
            let inner = rect.shrink((size.x * 0.13).max(4.0));
            painter.image(
                texture,
                inner,
                Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            let has_symbol = button.action.kind != ActionKind::None;
            let caption = if label.is_empty() {
                None
            } else {
                let width = rect.width() - 10.0;
                let mut font_size = (size.x * 0.20).clamp(8.0, 12.0);
                while font_size > 7.0
                    && painter
                        .layout_no_wrap(label.clone(), FontId::proportional(font_size), TEXT)
                        .size()
                        .x
                        > width
                {
                    font_size -= 0.5;
                }
                let caption = fit_caption(&painter, label, width, font_size);
                Some(painter.layout_no_wrap(caption, FontId::proportional(font_size), TEXT))
            };
            let icon_size = size.x * if caption.is_some() { 0.40 } else { 0.49 };
            let gap = if has_symbol && caption.is_some() {
                2.0
            } else {
                0.0
            };
            if has_symbol {
                let caption_height = caption
                    .as_ref()
                    .map_or(0.0, |text| text.mesh_bounds.height());
                theme::symbol(
                    &painter,
                    rect.center() - vec2(0.0, (gap + caption_height) / 2.0),
                    icon_size,
                    action_symbol(button.action.kind),
                    TEXT,
                );
            }
            if let Some(caption) = caption {
                let icon_height = if has_symbol { icon_size } else { 0.0 };
                let center = rect.center() + vec2(0.0, (gap + icon_height) / 2.0);
                theme::centered_galley(&painter, center, caption, TEXT);
            }
        }
        deck_face::key_outline(&painter, rect, border);
        self.control_interaction(response, control);
    }

    pub(super) fn control_border(&self, ui: &Ui, control: Control, response: &Response) -> Color32 {
        let active = self.recent.is_some_and(|(scene, last, at)| {
            scene == self.scene && last == control && at.elapsed() < Duration::from_millis(400)
        });
        if active {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
            MINT
        } else {
            deck_face::control_border(
                response,
                self.selected == control || response.dnd_hover_payload::<Drag>().is_some(),
            )
        }
    }

    pub(super) fn control_interaction(&mut self, response: Response, control: Control) {
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, control.label())
        });
        response.dnd_set_drag_payload(Drag::Control(control));
        if response.clicked() {
            self.selected = control;
        }
        if let Some(payload) = response.dnd_release_payload::<Drag>() {
            match (*payload).clone() {
                Drag::Action(kind) => self.assign(control, kind),
                Drag::Plugin(uuid) => {
                    if let Some(item) = self.plugin_item(&uuid) {
                        self.assign_plugin(control, item, None);
                    }
                }
                Drag::Control(source) if source != control => self.swap_controls(source, control),
                Drag::Control(_) => {}
            }
        }
        theme::context_menu(&response, |ui| {
            self.selected = control;
            if theme::menu_item(ui, "Copy binding", true) {
                let layout = &self.page().layout;
                let display = match control {
                    Control::Button(i) if i < DISPLAY_KEYS => Some(layout.buttons[i].clone()),
                    _ => None,
                };
                self.clipboard = Some(CopiedControl {
                    action: layout.action(control).clone(),
                    display,
                });
            }
            if theme::menu_item(ui, "Paste binding", self.clipboard.is_some())
                && let Some(copied) = self.clipboard.clone()
            {
                if copied.action.kind == ActionKind::Plugin {
                    if let Some(item) = self.plugin_item(&copied.action.value) {
                        self.assign_plugin(control, item, copied.action.plugin_context.as_deref());
                    } else {
                        self.report("Import the plugin before pasting this binding", true);
                    }
                } else if let (Control::Button(i), Some(display)) = (control, copied.display)
                    && i < DISPLAY_KEYS
                {
                    self.page_mut().layout.buttons[i] = display;
                } else {
                    self.selected = control;
                    self.page_mut().layout.set_action(control, copied.action);
                }
            }
            if theme::menu_item(ui, "Clear binding", true) {
                self.assign(control, ActionKind::None);
            }
        });
        response.on_hover_text(format!(
            "{} · {}",
            control.label(),
            self.page().layout.action(control).kind.label()
        ));
    }

    pub(super) fn swap_controls(&mut self, a: Control, b: Control) {
        match self.page_mut().layout.swap(a, b) {
            Ok(()) => self.selected = b,
            Err(error) => self.report(format!("{error:#}"), true),
        }
    }

    pub(super) fn photo(
        &mut self,
        ctx: &Context,
        index: usize,
        path: &Path,
    ) -> Option<egui::TextureId> {
        let color = self.page().layout.buttons[index]
            .color
            .unwrap_or(TILE_COLORS[index]);
        let version = std::fs::metadata(path)
            .ok()
            .map(|info| (info.modified().ok(), info.len()));
        let current = self
            .photos
            .get(path)
            .is_some_and(|cached| cached.color == color && cached.version == version);
        if !current {
            // One entry per path. A new color or a changed file replaces it, and a file that
            // failed to load is retried once it changes.
            if self.photos.len() >= 64 {
                self.photos.clear();
            }
            let texture = self.page().layout.tile(index).ok().map(|image| {
                let image = deck_face::lcd_image(&image);
                ctx.load_texture(path.to_string_lossy(), image, egui::TextureOptions::LINEAR)
            });
            self.photos.insert(
                path.to_owned(),
                PhotoTexture {
                    color,
                    version,
                    texture,
                },
            );
        }
        self.photos
            .get(path)
            .and_then(|cached| cached.texture.as_ref())
            .map(TextureHandle::id)
    }

    pub(super) fn plugin_preview(
        &mut self,
        ctx: &Context,
        index: usize,
        button: &profile::Button,
    ) -> Option<egui::TextureId> {
        let context = button.action.plugin_context.as_ref()?;
        let host = self.plugins.as_ref()?;
        let instance = Instance {
            context: context.clone(),
            action: button.action.value.clone(),
            control: Control::Button(index),
        };
        let color = button.color.unwrap_or(TILE_COLORS[index]);
        let hub = host.hub();
        let (version, spec) = {
            let hub = lock(&hub);
            if !hub.available(&instance.action) {
                return None;
            }
            let version = hub.version(context, false);
            if let Some(preview) = &self.plugin_previews[index]
                && preview.context == *context
                && preview.version == version
                && preview.label == button.label
                && preview.color == color
            {
                return Some(preview.texture.id());
            }
            (version, hub.render_spec(&instance, false)?)
        };
        let image = crate::plugins::render(&spec, color, &button.label);
        let texture = ctx.load_texture(
            "Plugin LCD",
            deck_face::lcd_image(&image),
            egui::TextureOptions::LINEAR,
        );
        let id = texture.id();
        self.plugin_previews[index] = Some(PluginPreview {
            context: context.clone(),
            version,
            label: button.label.clone(),
            color,
            texture,
        });
        Some(id)
    }
}

pub(super) fn action_symbol(kind: ActionKind) -> Symbol {
    use ActionKind::*;
    match kind {
        Shortcut => Symbol::Hotkey,
        TypeText | CodexDraft => Symbol::Text,
        OpenApp | OpenFile | CodexLaunch | CodexNewChat | CodexOpenChat => Symbol::Open,
        OpenWebsite => Symbol::Web,
        VolumeUp | VolumeDown | Mute => Symbol::Media,
        PlayPause | PreviousTrack | NextTrack => Symbol::Play,
        NextPage | PreviousPage | GoToPage | OpenFolder | Back | SwitchProfile => Symbol::Navigate,
        MultiAction | Delay => Symbol::Stack,
        Plugin => Symbol::Plugins,
        None => Symbol::System,
    }
}

pub(super) fn fit_caption(painter: &egui::Painter, title: &str, width: f32, size: f32) -> String {
    let font = FontId::proportional(size);
    let mut text: String = title.chars().take(50).collect();
    if painter
        .layout_no_wrap(text.clone(), font.clone(), TEXT)
        .size()
        .x
        <= width
    {
        return text;
    }
    while text.pop().is_some() {
        let short = format!("{text}…");
        if painter
            .layout_no_wrap(short.clone(), font.clone(), TEXT)
            .size()
            .x
            <= width
        {
            return short;
        }
    }
    String::new()
}
