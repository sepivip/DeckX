use std::hash::Hash;

use eframe::egui::{
    self, Align, Button, Color32, ComboBox, Context, Frame, Id, InnerResponse, Layout, Margin,
    Modal, ModalResponse, Painter, Popup, Rect, Response, RichText, Sense, Stroke, TextEdit,
    TextWrapMode, Ui, Vec2, WidgetInfo, WidgetType, vec2,
};

use super::{
    Symbol, bold, centered_galley, centered_text, dialog_frame, focus_outline, right_aligned_row,
    symbol, tokens::*,
};

pub fn modal<T>(
    ctx: &Context,
    id: &'static str,
    content: impl FnOnce(&mut Ui) -> T,
) -> ModalResponse<T> {
    Modal::new(Id::new(id))
        .frame(dialog_frame())
        .show(ctx, |ui| {
            ui.set_width(
                DIALOG_WIDTH.min((ctx.content_rect().width() - 2.0 * PAGE_INSET).max(0.0)),
            );
            ui.spacing_mut().item_spacing = DIALOG_GAP;
            content(ui)
        })
}

pub fn dialog_title(ui: &mut Ui, title: impl Into<String>) {
    ui.label(RichText::new(title).font(bold(DIALOG_TITLE)));
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DialogAction {
    Confirm,
    Cancel,
    None,
}

pub fn dialog_actions(ui: &mut Ui, confirm: &str, enabled: bool) -> DialogAction {
    right_aligned_row(ui, FIELD_HEIGHT, |ui| {
        let confirm = super::action_button(ui, confirm, true, enabled).clicked();
        let cancel = super::action_button(ui, "Cancel", false, true).clicked();
        if confirm {
            DialogAction::Confirm
        } else if cancel {
            DialogAction::Cancel
        } else {
            DialogAction::None
        }
    })
}

/// Settings and Plugins are tool windows. Their contents scroll inside the
/// shared frame so the title and close button stay visible at small sizes.
pub fn panel_window<'a>(title: &str, open: &'a mut bool, width: f32) -> egui::Window<'a> {
    egui::Window::new(title)
        .open(open)
        .collapsible(false)
        .resizable(false)
        .default_width(width)
        .frame(dialog_frame())
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
}

pub fn select<T>(
    ui: &mut Ui,
    id: impl Hash,
    value: &str,
    width: f32,
    options: impl FnOnce(&mut Ui) -> T,
) -> InnerResponse<Option<T>> {
    let width = width.min(ui.available_width()).max(0.0);
    let menu_height = (ui.ctx().content_rect().height() * 0.42).clamp(180.0, 288.0);
    ui.scope(|ui| {
        ui.set_max_width(width);
        ui.spacing_mut().interact_size.y = FIELD_HEIGHT;
        let response = ComboBox::from_id_salt(id)
            .width(width)
            .height(menu_height)
            .wrap_mode(TextWrapMode::Truncate)
            .selected_text(RichText::new(value).color(Color32::TRANSPARENT))
            .icon(|_, _, _, _| {})
            .popup_style(popup_style.into())
            .show_ui(ui, |ui| {
                ui.set_max_width(ui.available_width());
                options(ui)
            });
        let rect = response.response.rect;
        let padding = ui.spacing().button_padding.x;
        let text_rect = rect.shrink2(vec2(padding, 0.0));
        let icon_width = ui.spacing().icon_width;
        let available = (text_rect.width() - icon_width - ui.spacing().icon_spacing).max(0.0);
        let color = if ui.is_enabled() { TEXT } else { DIM };
        // Align both marks to the final button face, which can be taller than
        // egui's internal label and icon boxes.
        let arrow = snap_center(
            ui,
            egui::pos2(text_rect.right() - icon_width / 2.0, rect.center().y),
        );
        ui.painter().add(egui::Shape::convex_polygon(
            vec![
                arrow + vec2(-4.0, -2.5),
                arrow + vec2(4.0, -2.5),
                arrow + vec2(0.0, 2.5),
            ],
            color,
            Stroke::NONE,
        ));
        let text_clip = Rect::from_min_size(text_rect.min, vec2(available, text_rect.height()));
        leading_text(
            &ui.painter_at(text_clip),
            text_clip,
            value,
            egui::FontId::proportional(BODY),
            color,
        );
        if response.response.has_focus() {
            focus_outline(ui, rect);
        }
        response
    })
    .inner
}

pub fn select_value<T: PartialEq>(ui: &mut Ui, current: &mut T, value: T, label: &str) -> bool {
    if option(ui, label, *current == value) {
        *current = value;
        true
    } else {
        false
    }
}

pub fn option(ui: &mut Ui, label: &str, selected: bool) -> bool {
    if choice_row(ui, label, selected).clicked() {
        ui.close();
        true
    } else {
        false
    }
}

pub fn choice_row(ui: &mut Ui, label: &str, selected: bool) -> Response {
    ui.add(
        Button::selectable(selected, label)
            .truncate()
            .min_size(vec2(ui.available_width(), CONTROL_COMPACT_HEIGHT)),
    )
    .on_hover_text(label)
}

pub fn menu_group(ui: &mut Ui, title: &str) {
    ui.separator();
    ui.label(RichText::new(title).size(DETAIL).color(MUTED));
}

pub fn menu_item(ui: &mut Ui, label: &str, enabled: bool) -> bool {
    let row = ui.add_enabled(
        enabled,
        Button::new(label)
            .truncate()
            .min_size(vec2(ui.available_width(), CONTROL_COMPACT_HEIGHT)),
    );
    if row.clicked() {
        ui.close();
        true
    } else {
        false
    }
}

pub fn menu(response: &Response, contents: impl FnOnce(&mut Ui)) {
    Popup::menu(response)
        .width(COMMAND_MENU_WIDTH)
        .style(popup_style)
        .show(contents);
}

pub fn context_menu(response: &Response, contents: impl FnOnce(&mut Ui)) {
    Popup::context_menu(response)
        .width(COMMAND_MENU_WIDTH)
        .style(popup_style)
        .show(contents);
}

fn popup_style(style: &mut egui::Style) {
    egui::containers::menu::menu_style(style);
    style.spacing.button_padding = vec2(GAP, 0.0);
    style.spacing.item_spacing.y = STACK_GAP;
}

pub fn input(ui: &mut Ui, value: &mut String, hint: &str, limit: usize) -> Response {
    ui.add(singleline(value, hint, limit).min_size(vec2(0.0, FIELD_HEIGHT)))
}

fn singleline<'a>(value: &'a mut String, hint: &str, limit: usize) -> TextEdit<'a> {
    TextEdit::singleline(value)
        .hint_text(RichText::new(hint).color(DIM))
        .desired_width(f32::INFINITY)
        .char_limit(limit)
        .vertical_align(Align::Center)
}

pub fn multiline(ui: &mut Ui, value: &mut String, hint: &str) -> Response {
    ui.add(
        TextEdit::multiline(value)
            .hint_text(RichText::new(hint).color(DIM))
            .desired_width(f32::INFINITY)
            .desired_rows(3),
    )
}

pub fn search(ui: &mut Ui, value: &mut String, hint: &str, limit: usize) -> Response {
    let frame = ui.painter().add(egui::Shape::Noop);
    let response = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let (icon, _) =
                ui.allocate_exact_size(vec2(CONTROL_COMPACT_HEIGHT, FIELD_HEIGHT), Sense::hover());
            symbol(
                ui.painter(),
                icon.center(),
                ROW_TITLE,
                Symbol::Search,
                MUTED,
            );
            ui.add(
                singleline(value, hint, limit)
                    .frame(false)
                    .margin(Margin::symmetric(2, 4))
                    .min_size(vec2(0.0, FIELD_HEIGHT)),
            )
        })
        .inner;
    let bounds = Rect::from_min_max(
        response.rect.min - vec2(CONTROL_COMPACT_HEIGHT, 0.0),
        response.rect.max,
    );
    ui.painter().set(
        frame,
        egui::Shape::rect_filled(bounds, FIELD_RADIUS, SURFACE),
    );
    if response.has_focus() {
        focus_outline(ui, bounds);
    }
    response
}

pub fn form_row(ui: &mut Ui, label: &str, contents: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.add_sized(
            vec2(45.0, FIELD_HEIGHT),
            egui::Label::new(RichText::new(label).size(DETAIL).color(MUTED)).halign(Align::Max),
        );
        contents(ui);
    });
}

pub fn heading(ui: &mut Ui, title: impl Into<String>) {
    ui.label(RichText::new(title).font(bold(ROW_TITLE)));
}

pub fn clipped_text(ui: &mut Ui, value: &str, size: f32, color: Color32) {
    ui.add(egui::Label::new(RichText::new(value).size(size).color(color)).truncate())
        .on_hover_text(value);
}

pub fn row_title(ui: &mut Ui, title: &str) {
    ui.add(egui::Label::new(RichText::new(title).font(bold(ROW_TITLE))).truncate())
        .on_hover_text(title);
}

pub fn list_card(
    ui: &mut Ui,
    trailing: f32,
    title: &str,
    description: &str,
    status: Option<(&str, Color32)>,
    actions: impl FnOnce(&mut Ui),
) {
    super::card(WELL, CARD_PADDING).show(ui, |ui| {
        let height = if status.is_some() {
            LIST_ROW_HEIGHT + INSET
        } else {
            LIST_ROW_HEIGHT
        };
        super::split_row(
            ui,
            trailing,
            height,
            |ui| {
                ui.spacing_mut().item_spacing.y = STACK_GAP;
                row_title(ui, title);
                clipped_text(ui, description, DETAIL, MUTED);
                if let Some((label, color)) = status {
                    clipped_text(ui, label, DETAIL, color);
                }
            },
            actions,
        );
    });
}

pub fn status(ui: &mut Ui, label: &str, color: Color32) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(color));
        let (rect, _) = ui.allocate_exact_size(vec2(6.0, DETAIL), Sense::hover());
        ui.painter().circle_filled(rect.center(), 3.0, color);
    });
}

pub fn setting_row(ui: &mut Ui, label: &str, trailing: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), FIELD_HEIGHT + STACK_GAP),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.label(RichText::new(label).font(bold(BODY)));
            ui.with_layout(Layout::right_to_left(Align::Center), trailing);
        },
    );
}

pub fn field_setting(ui: &mut Ui, label: &str, width: f32, field: impl FnOnce(&mut Ui)) {
    super::split_row(
        ui,
        width,
        FIELD_HEIGHT,
        |ui| {
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.label(label);
            });
        },
        field,
    );
}

pub fn detail_row(
    ui: &mut Ui,
    title: &str,
    description: &str,
    trailing: f32,
    actions: impl FnOnce(&mut Ui),
) {
    Frame::new()
        .inner_margin(Margin::symmetric(INSET as i8, 9))
        .show(ui, |ui| {
            super::split_row(
                ui,
                trailing,
                FORM_ROW_HEIGHT,
                |ui| {
                    if description.is_empty() {
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            row_title(ui, title);
                        });
                    } else {
                        ui.spacing_mut().item_spacing.y = BORDER_WIDTH;
                        row_title(ui, title);
                        clipped_text(ui, description, DETAIL, MUTED);
                    }
                },
                actions,
            );
        });
}

pub fn page_track() -> Frame {
    super::frame(PAGE_TRACK, 2).corner_radius(TRACK_RADIUS)
}

pub fn section_header(ui: &mut Ui, heading: &str, actions: impl FnOnce(&mut Ui)) {
    ui.add_space(INSET);
    ui.horizontal(|ui| {
        ui.set_min_height(FIELD_HEIGHT);
        ui.label(RichText::new(heading).font(bold(SECTION_TITLE)));
        actions(ui);
    });
    ui.add_space(5.0);
}

pub fn page_chip(ui: &mut Ui, caption: &str, selected: bool) -> Response {
    super::caption_button(
        ui,
        caption,
        vec2(PAGE_CHIP_WIDTH, CONTROL_COMPACT_HEIGHT),
        bold(DETAIL),
        if selected { WELL } else { TEXT },
        Button::new(RichText::new(caption).color(Color32::TRANSPARENT))
            .fill(if selected { PAGE_ACTIVE } else { PAGE_TRACK })
            .stroke(Stroke::NONE)
            .corner_radius(CHIP_RADIUS),
    )
}

pub fn bare_button(ui: &mut Ui, caption: &str, width: f32, height: f32, size: f32) -> Response {
    super::caption_button(
        ui,
        caption,
        vec2(width, height),
        egui::FontId::proportional(size),
        TEXT,
        Button::new(RichText::new(caption).color(Color32::TRANSPARENT)).frame(false),
    )
}

pub fn preview_button(ui: &mut Ui, center: egui::Pos2, glyph: &str, label: &str) -> Response {
    let rect = Rect::from_center_size(center, Vec2::splat(INSET));
    let response = ui.interact(rect, ui.id().with(label), Sense::click());
    ui.painter().circle_filled(
        center,
        GAP,
        if response.hovered() || response.has_focus() {
            SURFACE
        } else {
            PREVIEW_OVERLAY
        },
    );
    centered_text(
        ui.painter(),
        center,
        glyph,
        egui::FontId::proportional(ROW_TITLE),
        TEXT,
    );
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
    response.on_hover_text(label)
}

fn snap_center(ui: &Ui, pos: egui::Pos2) -> egui::Pos2 {
    let grid = ui.ctx().pixels_per_point() * 2.0;
    egui::pos2((pos.x * grid).round() / grid, (pos.y * grid).round() / grid)
}

pub fn palette_group(
    ui: &mut Ui,
    id: impl Hash,
    title: &str,
    icon: Symbol,
    searching: bool,
    all_open: Option<bool>,
    default_open: bool,
) -> bool {
    let id = ui.id().with(id);
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), PALETTE_GROUP_HEIGHT),
        Sense::click(),
    );
    let mut open = ui
        .data(|data| data.get_temp::<bool>(id))
        .unwrap_or(default_open);
    if let Some(next) = all_open.or_else(|| (response.clicked() && !searching).then_some(!open)) {
        open = next;
        ui.data_mut(|data| data.insert_temp(id, open));
    }
    let open = open || searching;
    let painter = ui.painter_at(rect);
    let fill = if response.hovered() || response.has_focus() {
        SURFACE
    } else {
        PANEL
    };
    painter.rect_filled(rect, 0, fill);
    row_divider(&painter, rect);
    let center = rect.left_center() + vec2(21.0, 0.0);
    let points = if open {
        [vec2(-3.5, -2.0), vec2(0.0, 1.5), vec2(3.5, -2.0)]
    } else {
        [vec2(-1.5, -3.5), vec2(2.0, 0.0), vec2(-1.5, 3.5)]
    };
    painter.add(egui::Shape::line(
        points.map(|point| center + point).to_vec(),
        Stroke::new(1.2, MUTED),
    ));
    symbol(
        &painter,
        rect.left_center() + vec2(PALETTE_ICON_X, 0.0),
        PAGE_INSET,
        icon,
        TEXT,
    );
    leading_text(&painter, palette_text_rect(rect), title, bold(DETAIL), TEXT);
    response.widget_info(|| WidgetInfo::selected(WidgetType::CollapsingHeader, true, open, title));
    open
}

pub fn palette_action(ui: &mut Ui, title: &str, icon: Symbol) -> Response {
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), PALETTE_ACTION_HEIGHT),
        Sense::click_and_drag(),
    );
    let painter = ui.painter_at(rect);
    if response.hovered() || response.dragged() || response.has_focus() {
        painter.rect_filled(rect, 0, SURFACE);
    }
    row_divider(&painter, rect);
    symbol(
        &painter,
        rect.left_center() + vec2(PALETTE_ICON_X, 0.0),
        EMPTY_TITLE,
        icon,
        MUTED,
    );
    leading_text(
        &painter,
        palette_text_rect(rect),
        title,
        egui::FontId::proportional(DETAIL),
        MUTED,
    );
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, title));
    response
}

fn row_divider(painter: &Painter, rect: Rect) {
    painter.hline(
        rect.x_range(),
        rect.bottom() - BORDER_WIDTH / 2.0,
        Stroke::new(BORDER_WIDTH, DIVIDER),
    );
}

fn palette_text_rect(rect: Rect) -> Rect {
    Rect::from_min_max(
        rect.min + vec2(PALETTE_TEXT_X, 0.0),
        rect.max - vec2(GAP, 0.0),
    )
}

fn leading_text(painter: &Painter, rect: Rect, text: &str, font: egui::FontId, color: Color32) {
    let mut job = egui::text::LayoutJob::simple_singleline(text.into(), font, color);
    job.wrap.max_width = rect.width().max(0.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let galley = painter.fonts_mut(|fonts| fonts.layout_job(job));
    let center = egui::pos2(
        rect.left() + galley.mesh_bounds.width() / 2.0,
        rect.center().y,
    );
    centered_galley(painter, center, galley, color);
}
