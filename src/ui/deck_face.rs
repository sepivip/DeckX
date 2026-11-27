use std::f32::consts::FRAC_PI_2;

use crate::{
    deck_model::{DeckModel, Placement},
    profile::{Control, Direction},
    theme,
};
use eframe::egui::{
    self, Align2, Color32, FontId, Painter, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui,
    Vec2, vec2,
};

const DEVICE: Color32 = Color32::from_rgb(24, 25, 30);
const DIAL: Color32 = Color32::from_rgb(38, 40, 49);
const LCD_CORNER_RATIO: f32 = 0.18;

pub(super) struct DeckFace<'a> {
    model: &'a DeckModel,
    face: Rect,
    unit: f32,
}

#[derive(Clone, Copy)]
pub(super) enum FaceControl {
    Lcd {
        index: usize,
        rect: Rect,
    },
    Round {
        control: Control,
        center: Pos2,
        radius: f32,
        dial: bool,
    },
    Turn {
        index: usize,
        direction: Direction,
        rect: Rect,
    },
}

impl FaceControl {
    pub(super) fn control(self) -> Control {
        match self {
            Self::Lcd { index, .. } => Control::Button(index),
            Self::Round { control, .. } => control,
            Self::Turn {
                index, direction, ..
            } => Control::Dial(index, direction),
        }
    }

    pub(super) fn rect(self) -> Rect {
        match self {
            Self::Lcd { rect, .. } | Self::Turn { rect, .. } => rect,
            Self::Round { center, radius, .. } => {
                Rect::from_center_size(center, Vec2::splat(2.0 * radius))
            }
        }
    }

    pub(super) fn paint(self, painter: &Painter, border: Color32) {
        match self {
            Self::Lcd { rect, .. } => key_outline(painter, rect, border),
            Self::Round {
                center,
                radius,
                dial,
                ..
            } => {
                round_control(painter, center, radius, border, dial);
            }
            Self::Turn {
                rect, direction, ..
            } => turn_control(painter, rect, direction, border),
        }
    }
}

impl<'a> DeckFace<'a> {
    pub(super) fn mode_width(model: &DeckModel, page_height: f32) -> f32 {
        // Leave room for the layout editor and options above the shared footer.
        ((page_height - 428.0) * model.aspect()).clamp(292.0, 404.0)
    }

    pub(super) fn allocate(ui: &mut Ui, model: &'a DeckModel, width: f32) -> Self {
        let size = vec2(width, width / model.aspect());
        let (body, _) = ui.allocate_exact_size(size + vec2(0.0, 7.0), Sense::hover());
        Self::at(model, Rect::from_min_size(body.min, size))
    }

    pub(super) fn at(model: &'a DeckModel, face: Rect) -> Self {
        Self {
            model,
            face,
            unit: face.width() / model.face[0],
        }
    }

    fn point(&self, [x, y]: [f32; 2]) -> Pos2 {
        self.face.min + vec2(x, y) * self.unit
    }

    pub(super) fn controls(&self) -> impl Iterator<Item = FaceControl> + '_ {
        let lcds = self
            .model
            .lcd
            .iter()
            .enumerate()
            .map(|(index, key)| FaceControl::Lcd {
                index,
                rect: Rect::from_center_size(
                    self.point(key.center),
                    Vec2::splat(key.size * self.unit),
                ),
            });
        let buttons = self.model.side.iter().enumerate().map(|(index, button)| {
            self.round(Control::Button(self.model.lcd.len() + index), button)
        });
        let dials = self
            .model
            .dials
            .iter()
            .enumerate()
            .flat_map(|(index, dial)| {
                [
                    Direction::Press,
                    Direction::Counterclockwise,
                    Direction::Clockwise,
                ]
                .map(|direction| {
                    if direction == Direction::Press {
                        self.round(Control::Dial(index, direction), dial)
                    } else {
                        FaceControl::Turn {
                            index,
                            direction,
                            rect: self.dial_turn(dial, direction),
                        }
                    }
                })
            });
        lcds.chain(buttons).chain(dials)
    }

    fn round(&self, control: Control, placement: &Placement) -> FaceControl {
        FaceControl::Round {
            control,
            center: self.point(placement.center),
            radius: placement.size * self.unit / 2.0,
            dial: matches!(control, Control::Dial(..)),
        }
    }

    fn dial_turn(&self, dial: &Placement, direction: Direction) -> Rect {
        let center = self.point(dial.center);
        let radius = dial.size * self.unit / 2.0;
        let size = vec2(
            (7.0 * self.unit).clamp(16.0, 24.0),
            (5.4 * self.unit).clamp(13.0, 18.0),
        );
        let sign = if direction == Direction::Clockwise {
            1.0
        } else {
            -1.0
        };
        let offset = (radius * 0.63).max(size.x / 2.0 + 2.0);
        Rect::from_center_size(
            center + vec2(sign * offset, radius + size.y / 2.0 + 3.0),
            size,
        )
    }
    pub(super) fn paint(&self, painter: &Painter) {
        let face = self.face;
        painter.rect_filled(face.translate(vec2(0.0, 5.0)), 12, Color32::from_gray(13));
        shade(painter, face, 12.0, Color32::from_rgb(36, 38, 46), DEVICE);
        painter.rect_stroke(
            face,
            12,
            Stroke::new(1.0, Color32::from_rgb(58, 61, 72)),
            StrokeKind::Inside,
        );
        painter.rect_stroke(
            face.shrink(2.0),
            10,
            Stroke::new(1.0, Color32::from_gray(38)),
            StrokeKind::Inside,
        );
        painter.text(
            self.point(self.model.mark_position),
            Align2::LEFT_CENTER,
            self.model.mark,
            FontId::monospace(9.0),
            Color32::from_gray(151),
        );
    }
}

pub(super) fn round_control(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    border: Color32,
    ridged: bool,
) {
    painter.circle_filled(
        center + vec2(0.0, 2.0),
        radius + 1.0,
        Color32::from_gray(13),
    );
    painter.circle(center, radius, DIAL, Stroke::new(1.0, border));
    if ridged {
        for tick in 0..32 {
            let angle = tick as f32 * std::f32::consts::TAU / 32.0;
            let spoke = vec2(angle.cos(), angle.sin());
            painter.line_segment(
                [
                    center + spoke * (radius * 0.81),
                    center + spoke * (radius * 0.94),
                ],
                Stroke::new(0.85, Color32::from_rgb(72, 76, 87)),
            );
        }
    }
    painter.circle(
        center,
        radius * if ridged { 0.79 } else { 0.86 },
        Color32::from_rgb(33, 34, 41),
        Stroke::new(1.0, Color32::from_rgb(52, 55, 65)),
    );
    if ridged {
        painter.line_segment(
            [
                center - vec2(0.0, radius * 0.64),
                center - vec2(0.0, radius * 0.46),
            ],
            Stroke::new(1.5, Color32::from_rgb(155, 160, 179)),
        );
    }
}

pub(super) fn turn_control(painter: &Painter, rect: Rect, direction: Direction, border: Color32) {
    painter.rect(
        rect,
        5,
        theme::WELL,
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );
    theme::chevron(
        painter,
        rect.center(),
        direction == Direction::Clockwise,
        theme::MUTED,
    );
}

pub(super) fn key_radius(rect: Rect) -> u8 {
    (rect.width() * LCD_CORNER_RATIO).round() as u8
}

pub(super) fn lcd_image(tile: &image::RgbImage) -> egui::ColorImage {
    egui::ColorImage::from_rgb(
        [tile.width() as usize, tile.height() as usize],
        tile.as_raw(),
    )
}

pub(super) fn key_outline(painter: &Painter, rect: Rect, color: Color32) {
    painter.rect_stroke(
        rect,
        key_radius(rect),
        Stroke::new(1.5, color),
        StrokeKind::Inside,
    );
}

pub(super) fn control_border(response: &Response, selected: bool) -> Color32 {
    if selected || response.has_focus() {
        theme::ACCENT
    } else if response.hovered() {
        theme::DIM
    } else {
        theme::LINE
    }
}

fn shade(painter: &Painter, rect: Rect, radius: f32, highlight: Color32, base: Color32) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.center(), mix(highlight, base, 0.5));
    for (quarter, center) in [
        rect.right_top() + vec2(-radius, radius),
        rect.right_bottom() - vec2(radius, radius),
        rect.left_bottom() + vec2(radius, -radius),
        rect.left_top() + vec2(radius, radius),
    ]
    .into_iter()
    .enumerate()
    {
        for step in 0..=5 {
            let angle = (quarter as f32 - 1.0 + step as f32 / 5.0) * FRAC_PI_2;
            let point = center + vec2(angle.cos(), angle.sin()) * radius;
            let along = ((point.x - rect.left()) / rect.width()
                + (point.y - rect.top()) / rect.height())
                * 0.5;
            mesh.colored_vertex(point, mix(highlight, base, along));
        }
    }
    let vertices = mesh.vertices.len() as u32;
    for index in 1..vertices {
        mesh.add_triangle(0, index, if index + 1 < vertices { index + 1 } else { 1 });
    }
    painter.add(egui::Shape::mesh(mesh));
}

fn mix(first: Color32, second: Color32, amount: f32) -> Color32 {
    let mix_channel =
        |a: u8, b: u8| (f32::from(a) * (1.0 - amount) + f32::from(b) * amount).round() as u8;
    Color32::from_rgb(
        mix_channel(first.r(), second.r()),
        mix_channel(first.g(), second.g()),
        mix_channel(first.b(), second.b()),
    )
}
