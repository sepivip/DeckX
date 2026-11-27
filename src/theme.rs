use std::sync::Arc;

use eframe::egui::{
    self, Color32, Context, FontData, FontDefinitions, FontFamily, FontId, Frame, Margin, Painter,
    Pos2, Rect, RichText, Stroke, StrokeKind, TextStyle, Ui, Vec2, vec2,
};

mod forms;
mod tokens;
pub use forms::*;
pub use tokens::*;

pub(crate) fn font_definitions() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "Inter".into(),
        Arc::new(FontData::from_static(include_bytes!("../assets/Inter.ttf"))),
    );
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "Inter".into());
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        let root = std::path::PathBuf::from(system_root).join("Fonts");
        for (file, name, family) in [
            ("segoeui.ttf", "Segoe UI", FontFamily::Proportional),
            (
                "segoeuib.ttf",
                "Segoe UI Bold",
                FontFamily::Name("strong".into()),
            ),
        ] {
            if let Ok(bytes) = std::fs::read(root.join(file)) {
                fonts
                    .font_data
                    .insert(name.into(), Arc::new(FontData::from_owned(bytes)));
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .insert(0, name.into());
            }
        }
    }
    fonts
        .families
        .entry(FontFamily::Name("strong".into()))
        .or_default()
        .push("Inter".into());
    fonts
}

pub fn install(ctx: &Context) {
    ctx.set_fonts(font_definitions());

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = CANVAS;
    visuals.window_fill = PANEL;
    visuals.window_stroke = Stroke::new(BORDER_WIDTH, LINE);
    visuals.window_corner_radius = DIALOG_RADIUS.into();
    visuals.menu_corner_radius = CARD_RADIUS.into();
    visuals.popup_shadow = DIALOG_SHADOW;
    visuals.extreme_bg_color = SURFACE;
    visuals.override_text_color = Some(TEXT);
    visuals.selection.bg_fill = ACCENT_WELL;
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    for (visual, fill, border) in [
        (&mut visuals.widgets.noninteractive, PANEL, LINE),
        (&mut visuals.widgets.inactive, SURFACE, Color32::TRANSPARENT),
        (&mut visuals.widgets.hovered, HOVER, LINE),
        (&mut visuals.widgets.active, PRESSED, LINE),
        (&mut visuals.widgets.open, HOVER, LINE),
    ] {
        visual.bg_fill = fill;
        visual.weak_bg_fill = fill;
        visual.bg_stroke = Stroke::new(BORDER_WIDTH, border);
        visual.corner_radius = FIELD_RADIUS.into();
        visual.fg_stroke.color = TEXT;
        visual.expansion = 0.0;
    }
    ctx.set_visuals(visuals);
    ctx.style_mut(|style| {
        style.spacing.item_spacing = vec2(GAP, 6.0);
        style.spacing.button_padding = vec2(9.0, TIGHT);
        style.spacing.interact_size.y = 25.0;
        style.spacing.scroll.dormant_handle_opacity = 0.55;
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(BODY));
        style
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(BODY));
        style
            .text_styles
            .insert(TextStyle::Heading, bold(PAGE_TITLE));
        style
            .text_styles
            .insert(TextStyle::Small, FontId::proportional(CAPTION));
    });
}

pub fn label(text: impl Into<String>) -> RichText {
    RichText::new(text).size(CAPTION).color(MUTED).strong()
}

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("strong".into()))
}

pub fn frame(fill: Color32, margin: i8) -> Frame {
    Frame::new().fill(fill).inner_margin(Margin::same(margin))
}

pub fn card(fill: Color32, margin: i8) -> Frame {
    frame(fill, margin)
        .corner_radius(CARD_RADIUS)
        .stroke(Stroke::new(BORDER_WIDTH, LINE))
}

pub fn centered_column<T>(ui: &mut Ui, width: f32, content: impl FnOnce(&mut Ui) -> T) -> T {
    ui.vertical_centered(|ui| {
        ui.allocate_ui_with_layout(
            vec2(width, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_width(width);
                content(ui)
            },
        )
        .inner
    })
    .inner
}

pub fn split_row(
    ui: &mut Ui,
    trailing: f32,
    height: f32,
    left: impl FnOnce(&mut Ui),
    right: impl FnOnce(&mut Ui),
) {
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), height),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            let leading = (ui.available_width() - trailing - ui.spacing().item_spacing.x).max(0.0);
            ui.allocate_ui_with_layout(
                vec2(leading, height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(leading);
                    left(ui);
                },
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), right);
        },
    );
}

pub fn right_aligned_row<T>(ui: &mut Ui, height: f32, content: impl FnOnce(&mut Ui) -> T) -> T {
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), height),
        egui::Layout::right_to_left(egui::Align::Center),
        content,
    )
    .inner
}

fn dialog_frame() -> Frame {
    frame(PANEL, DIALOG_PADDING)
        .corner_radius(DIALOG_RADIUS)
        .stroke(Stroke::new(BORDER_WIDTH, LINE))
        .shadow(DIALOG_SHADOW)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centered_text_aligns_the_glyph_mesh() {
        let ctx = Context::default();
        install(&ctx);
        for label in ["+", "↗", "Connect"] {
            let center = egui::pos2(140.0, 80.0);
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(320.0, 240.0))),
                    ..Default::default()
                },
                |ctx| {
                    centered_text(
                        &ctx.layer_painter(egui::LayerId::background()),
                        center,
                        label,
                        FontId::proportional(17.0),
                        TEXT,
                    );
                },
            );
            let text = output
                .shapes
                .iter()
                .find_map(|clipped| {
                    if let egui::Shape::Text(text) = &clipped.shape {
                        Some(text)
                    } else {
                        None
                    }
                })
                .expect("expected rendered text");
            let ink_center = text.pos + text.galley.mesh_bounds.center().to_vec2();
            assert!(
                (ink_center - center).length() < 0.01,
                "{label}: {ink_center:?}"
            );
        }
    }

    #[test]
    fn shared_columns_and_rows_keep_their_centers() {
        for viewport_width in [700.0, 840.0, 1360.0] {
            let ctx = Context::default();
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(viewport_width, 580.0))),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let available = ui.available_rect_before_wrap();
                        let width = (available.width() - 56.0).min(672.0);
                        let (row_rect, left_rect, right_rect) = centered_column(ui, width, |ui| {
                            let top = ui.next_widget_position().y;
                            let mut left_rect = Rect::NOTHING;
                            let mut right_rect = Rect::NOTHING;
                            split_row(
                                ui,
                                80.0,
                                34.0,
                                |ui| {
                                    left_rect = ui
                                        .allocate_exact_size(vec2(60.0, 34.0), egui::Sense::hover())
                                        .0
                                },
                                |ui| {
                                    right_rect = ui
                                        .allocate_exact_size(vec2(80.0, 26.0), egui::Sense::hover())
                                        .0
                                },
                            );
                            let row_rect = Rect::from_min_size(
                                egui::pos2(ui.min_rect().left(), top),
                                vec2(width, 34.0),
                            );
                            (row_rect, left_rect, right_rect)
                        });
                        assert!((row_rect.center().x - available.center().x).abs() < 0.5);
                        assert!((right_rect.right() - row_rect.right()).abs() < 0.5);
                        assert!((left_rect.center().y - right_rect.center().y).abs() < 0.5);
                        let top = ui.available_rect_before_wrap().top();
                        let button = right_aligned_row(ui, 28.0, |ui| {
                            ui.allocate_exact_size(vec2(60.0, 28.0), egui::Sense::hover())
                                .0
                        });
                        assert!((button.top() - top).abs() < 0.5);
                        assert!(
                            (ui.available_rect_before_wrap().top()
                                - top
                                - 28.0
                                - ui.spacing().item_spacing.y)
                                .abs()
                                < 0.5
                        );
                    });
                },
            );
        }
    }
}

pub fn chevron(painter: &Painter, center: Pos2, right: bool, color: Color32) {
    let sign = if right { 1.0 } else { -1.0 };
    painter.add(egui::Shape::line(
        vec![
            center + vec2(-2.0 * sign, -3.0),
            center + vec2(1.5 * sign, 0.0),
            center + vec2(-2.0 * sign, 3.0),
        ],
        Stroke::new(1.2, color),
    ));
}

pub fn text_button(
    ui: &mut Ui,
    label: &str,
    width: f32,
    selected: bool,
    enabled: bool,
) -> egui::Response {
    button(ui, label, width, selected, false, enabled)
}

pub fn action_button(ui: &mut Ui, label: &str, primary: bool, enabled: bool) -> egui::Response {
    button(ui, label, 0.0, false, primary, enabled)
}

pub fn tab(ui: &mut Ui, label: &str, width: f32, selected: bool) -> egui::Response {
    let response = text_button(ui, label, width, selected, true);
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, label)
    });
    response
}

fn button(
    ui: &mut Ui,
    label: &str,
    width: f32,
    selected: bool,
    primary: bool,
    enabled: bool,
) -> egui::Response {
    let font = if primary {
        bold(BODY)
    } else {
        FontId::proportional(BODY)
    };
    let mut widget = egui::Button::new(
        RichText::new(label)
            .font(font.clone())
            .color(Color32::TRANSPARENT),
    )
    .min_size(vec2(width, FIELD_HEIGHT));
    if primary && enabled {
        widget = widget.fill(PRIMARY).stroke(Stroke::new(1.0, ACCENT));
    } else if selected && enabled {
        widget = widget.fill(ACCENT_WELL).stroke(Stroke::new(1.0, ACCENT));
    }
    let response = ui.add_enabled(enabled, widget);
    let face = response.rect;
    if enabled
        && (primary || selected)
        && (response.hovered() || response.is_pointer_button_down_on())
    {
        let fill = if response.is_pointer_button_down_on() {
            PRIMARY_PRESSED
        } else if primary {
            PRIMARY_HOVER
        } else {
            SELECTED_HOVER
        };
        ui.painter()
            .rect_filled(face.shrink(BORDER_WIDTH), FIELD_RADIUS - 1, fill);
    }
    let color = if enabled { TEXT } else { DIM };
    centered_text(ui.painter(), face.center(), label, font, color);
    if enabled && response.has_focus() {
        focus_outline(ui, face);
    }
    response
}

fn focus_outline(ui: &Ui, rect: Rect) {
    ui.painter().rect_stroke(
        rect.shrink(BORDER_WIDTH),
        FIELD_RADIUS.saturating_sub(1),
        Stroke::new(BORDER_WIDTH, TEXT),
        StrokeKind::Inside,
    );
}

pub fn empty_state(ui: &mut Ui, title: &str, detail: &str) {
    ui.label(RichText::new(title).font(bold(EMPTY_TITLE)));
    ui.label(RichText::new(detail).color(MUTED));
}

fn caption_button(
    ui: &mut Ui,
    caption: &str,
    size: Vec2,
    font: FontId,
    color: Color32,
    widget: egui::Button<'_>,
) -> egui::Response {
    let response = ui.add_sized(size, widget);
    centered_text(ui.painter(), response.rect.center(), caption, font, color);
    response
}

const BRIGHTNESS_SLIDER_WIDTH: f32 = 144.0;
const BRIGHTNESS_VALUE_WIDTH: f32 = 50.0;

pub fn brightness_width(ui: &Ui) -> f32 {
    BRIGHTNESS_SLIDER_WIDTH + BRIGHTNESS_VALUE_WIDTH + ui.spacing().item_spacing.x
}

pub fn brightness_slider(ui: &mut Ui, brightness: &mut u8) -> egui::Response {
    let (value_rect, _) =
        ui.allocate_exact_size(vec2(BRIGHTNESS_VALUE_WIDTH, 25.0), egui::Sense::hover());
    let response = ui
        .scope(|ui| {
            // Keep the native slider's input handling and draw a centered thumb and value.
            ui.spacing_mut().slider_width = BRIGHTNESS_SLIDER_WIDTH;
            let widgets = &mut ui.visuals_mut().widgets;
            for style in [
                &mut widgets.inactive,
                &mut widgets.hovered,
                &mut widgets.active,
            ] {
                style.bg_fill = Color32::TRANSPARENT;
                style.fg_stroke = Stroke::NONE;
            }
            ui.add(
                egui::Slider::new(brightness, 0..=100)
                    .show_value(false)
                    .trailing_fill(false),
            )
        })
        .inner;
    response.widget_info(|| {
        egui::WidgetInfo::slider(ui.is_enabled(), f64::from(*brightness), "Brightness")
    });

    let rect = response.rect;
    let y = rect.center().y;
    let radius = rect.height() / 2.5;
    let range = rect.x_range().shrink(radius);
    let center = egui::pos2(egui::lerp(range, f32::from(*brightness) / 100.0), y);
    let painter = ui.painter();
    painter.hline(range, y, Stroke::new(2.0, LINE));
    if response.has_focus() {
        painter.circle_stroke(center, radius + 2.0, Stroke::new(1.0, ACCENT));
    }
    painter.circle_filled(center, radius, TEXT);
    let value = painter.layout_no_wrap(format!("{brightness}%"), FontId::proportional(BODY), MUTED);
    let value_center = egui::pos2(value_rect.right() - value.mesh_bounds.width() / 2.0, y);
    centered_galley(painter, value_center, value, MUTED);
    response
}

pub fn centered_text(
    painter: &Painter,
    center: Pos2,
    text: impl Into<String>,
    font: FontId,
    color: Color32,
) {
    let galley = painter.layout_no_wrap(text.into(), font, color);
    centered_galley(painter, center, galley, color);
}

pub fn centered_galley(painter: &Painter, center: Pos2, galley: Arc<egui::Galley>, color: Color32) {
    if galley.mesh_bounds.is_positive() {
        // Center the painted glyph bounds within the control.
        painter.galley(
            center - galley.mesh_bounds.center().to_vec2(),
            galley,
            color,
        );
    }
}

#[derive(Clone, Copy)]
pub enum Symbol {
    Search,
    List,
    Settings,
    Plugins,
    Plug,
    Stack,
    Navigate,
    Media,
    System,
    Web,
    Hotkey,
    Open,
    Text,
    Play,
    Trash,
}

pub fn symbol(painter: &Painter, center: Pos2, size: f32, icon: Symbol, color: Color32) {
    let scale = size / 24.0;
    let point = |x: f32, y: f32| center + vec2(x - 12.0, y - 12.0) * scale;
    let stroke = Stroke::new((1.6 * scale).max(1.0), color);
    let path = |coords: &[(f32, f32)]| {
        painter.add(egui::Shape::line(
            coords.iter().map(|&(x, y)| point(x, y)).collect(),
            stroke,
        ));
    };
    let rect = |x1, y1, x2, y2, r| {
        painter.rect_stroke(
            Rect::from_min_max(point(x1, y1), point(x2, y2)),
            r,
            stroke,
            StrokeKind::Middle,
        );
    };
    let circle = |x, y, r| {
        painter.circle_stroke(point(x, y), r * scale, stroke);
    };
    use Symbol::*;
    match icon {
        Search => {
            circle(10.0, 10.0, 5.5);
            path(&[(14.0, 14.0), (19.0, 19.0)]);
        }
        List => {
            for y in [6.0, 12.0, 18.0] {
                painter.circle_filled(point(4.0, y), 1.2 * scale, color);
                path(&[(8.0, y), (21.0, y)]);
            }
        }
        Settings => {
            let outline = (0..32)
                .map(|i| {
                    let angle = i as f32 * std::f32::consts::TAU / 32.0;
                    let radius = if i % 4 == 0 || i % 4 == 3 { 8.0 } else { 10.0 };
                    point(12.0 + angle.cos() * radius, 12.0 + angle.sin() * radius)
                })
                .collect();
            painter.add(egui::Shape::closed_line(outline, stroke));
            circle(12.0, 12.0, 3.1);
        }
        Plugins => {
            rect(3.0, 5.0, 21.0, 19.0, 2);
            for y in [9.0, 15.0] {
                for x in [7.0, 12.0, 17.0] {
                    painter.circle_filled(point(x, y), 0.9 * scale, color);
                }
            }
        }
        Plug => {
            path(&[(8.0, 3.0), (8.0, 8.0), (16.0, 8.0), (16.0, 3.0)]);
            path(&[
                (6.0, 8.0),
                (6.0, 12.0),
                (9.0, 16.0),
                (15.0, 16.0),
                (18.0, 12.0),
                (18.0, 8.0),
            ]);
            // This segment is thick enough to stay visible at 100% when
            // centered on the boundary between two physical pixels.
            painter.line_segment(
                [point(12.0, 16.0), point(12.0, 21.0)],
                Stroke::new(stroke.width.max(1.5), color),
            );
        }
        Stack => {
            path(&[
                (2.0, 12.0),
                (8.0, 6.0),
                (14.0, 12.0),
                (8.0, 18.0),
                (2.0, 12.0),
            ]);
            for x in [12.0, 17.0] {
                path(&[(x, 6.0), (x + 6.0, 12.0), (x, 18.0)]);
            }
        }
        Navigate => {
            rect(8.0, 2.0, 16.0, 10.0, 1);
            rect(2.0, 13.0, 10.0, 21.0, 1);
            rect(14.0, 13.0, 22.0, 21.0, 1);
            path(&[(10.0, 7.0), (12.0, 5.0), (14.0, 7.0)]);
            path(&[(7.0, 15.0), (5.0, 17.0), (7.0, 19.0)]);
            path(&[(17.0, 15.0), (19.0, 17.0), (17.0, 19.0)]);
        }
        Media => {
            path(&[
                (2.0, 9.0),
                (6.0, 9.0),
                (11.0, 5.0),
                (11.0, 19.0),
                (6.0, 15.0),
                (2.0, 15.0),
                (2.0, 9.0),
            ]);
            path(&[(14.0, 8.0), (16.0, 10.0), (16.0, 14.0), (14.0, 16.0)]);
            path(&[(18.0, 5.0), (21.0, 9.0), (21.0, 15.0), (18.0, 19.0)]);
        }
        System => {
            rect(7.0, 2.0, 17.0, 22.0, 2);
            path(&[(7.0, 6.0), (17.0, 6.0)]);
            path(&[(10.0, 10.0), (14.0, 10.0)]);
            circle(12.0, 16.0, 1.3);
        }
        Web => {
            circle(12.0, 12.0, 9.0);
            path(&[(3.0, 12.0), (21.0, 12.0)]);
            path(&[
                (12.0, 3.0),
                (8.0, 7.0),
                (7.0, 12.0),
                (8.0, 17.0),
                (12.0, 21.0),
                (16.0, 17.0),
                (17.0, 12.0),
                (16.0, 7.0),
                (12.0, 3.0),
            ]);
        }
        Hotkey => {
            rect(2.0, 5.0, 22.0, 19.0, 2);
            path(&[(6.0, 9.0), (8.0, 9.0)]);
            path(&[(11.0, 9.0), (13.0, 9.0)]);
            path(&[(16.0, 9.0), (18.0, 9.0)]);
            path(&[(7.0, 14.0), (17.0, 14.0)]);
        }
        Open => {
            path(&[
                (20.0, 12.0),
                (20.0, 21.0),
                (3.0, 21.0),
                (3.0, 4.0),
                (12.0, 4.0),
            ]);
            path(&[(10.0, 14.0), (21.0, 3.0), (15.0, 3.0)]);
            path(&[(21.0, 3.0), (21.0, 9.0)]);
        }
        Text => {
            path(&[(5.0, 7.0), (5.0, 4.0), (19.0, 4.0), (19.0, 7.0)]);
            path(&[(12.0, 4.0), (12.0, 20.0)]);
            path(&[(8.0, 20.0), (16.0, 20.0)]);
        }
        Play => {
            path(&[(3.0, 4.0), (3.0, 20.0), (13.0, 12.0), (3.0, 4.0)]);
            for x in [17.0, 21.0] {
                path(&[(x, 4.0), (x, 20.0)]);
            }
        }
        Trash => {
            path(&[(4.0, 6.0), (20.0, 6.0)]);
            path(&[(9.0, 6.0), (9.0, 3.0), (15.0, 3.0), (15.0, 6.0)]);
            path(&[(6.0, 6.0), (7.0, 21.0), (17.0, 21.0), (18.0, 6.0)]);
            for x in [10.0, 14.0] {
                path(&[(x, 9.0), (x, 17.0)]);
            }
        }
    }
}

pub fn icon_button(ui: &mut Ui, icon: Symbol, label: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(FIELD_HEIGHT), egui::Sense::click());
    if response.hovered() || response.has_focus() {
        ui.painter().rect_filled(rect, FIELD_RADIUS, SURFACE);
    }
    symbol(
        ui.painter(),
        rect.center(),
        ICON_SIZE,
        icon,
        if response.hovered() { TEXT } else { MUTED },
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    response.on_hover_text(label)
}

pub fn icon() -> egui::IconData {
    const SIZE: u32 = 64;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let corner_x = x.clamp(10, SIZE - 11);
            let corner_y = y.clamp(10, SIZE - 11);
            let dx = x.abs_diff(corner_x);
            let dy = y.abs_diff(corner_y);
            let pixel = if dx * dx + dy * dy > 100 {
                [0, 0, 0, 0]
            } else if ((14..29).contains(&x) || (35..50).contains(&x))
                && ((14..29).contains(&y) || (35..50).contains(&y))
            {
                let level = usize::from(x > 32) + usize::from(y > 32);
                [
                    [214, 233, 255, 255],
                    [112, 183, 255, 255],
                    [55, 144, 231, 255],
                ][level]
            } else {
                [35, 59, 80, 255]
            };
            rgba.extend_from_slice(&pixel);
        }
    }
    egui::IconData {
        rgba,
        width: SIZE,
        height: SIZE,
    }
}

pub fn window_caption(frame: &eframe::Frame) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
    let Ok(handle) = frame.window_handle() else {
        return false;
    };
    let RawWindowHandle::Win32(window) = handle.as_raw() else {
        return true;
    };
    let hwnd = window.hwnd.get() as windows_sys::Win32::Foundation::HWND;
    let dark = 1u32;
    let colorref = |color: Color32| u32::from_le_bytes([color.r(), color.g(), color.b(), 0]);
    // Only the DeckX window is modified. Older Windows builds can ignore the color attributes.
    for (attribute, value) in [
        (20, dark),
        (34, colorref(LINE)),
        (35, colorref(PANEL)),
        (36, colorref(TEXT)),
    ] {
        unsafe { DwmSetWindowAttribute(hwnd, attribute, (&value as *const u32).cast(), 4) };
    }
    true
}
