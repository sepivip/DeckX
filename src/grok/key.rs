use eframe::egui::{FontFamily, FontId, epaint::text::LayoutJob};
use image::{DynamicImage, Rgb, RgbImage, Rgba, RgbaImage, imageops};

use super::Bot;
use crate::{deck_model::LCD_PIXELS, glyphs, theme};

const SIDE: u32 = LCD_PIXELS;
const SELECTION: [u8; 3] = [0, 123, 255];
pub(crate) const AVATAR_SCALE: f32 = 0.88;
pub(crate) const AVATAR_CENTER_Y: f32 = 0.40;
pub(crate) const UNREAD_COLOR: [u8; 3] = [52, 150, 255];
pub(crate) const UNREAD_RADIUS: f32 = 0.065;
pub(crate) const UNREAD_OUTLINE: f32 = 0.02;
pub(crate) const STAR_CENTER: [f32; 2] = [0.75, 0.63];
pub(crate) const STAR_RADIUS: f32 = 0.09;
pub(crate) const STAR_BACKGROUND: [u8; 3] = [238, 153, 39];

pub(crate) fn caption_center(has_avatar: bool) -> f32 {
    if has_avatar { 0.86 } else { 0.50 }
}

pub(crate) fn unread_center(has_avatar: bool) -> [f32; 2] {
    if has_avatar {
        [0.75, 0.63]
    } else {
        [0.82, 0.18]
    }
}

pub(crate) fn star_points(radius: f32) -> [[f32; 2]; 10] {
    std::array::from_fn(|index| {
        let angle = index as f32 * std::f32::consts::TAU / 10.0 - std::f32::consts::FRAC_PI_2;
        let radius = radius * if index % 2 == 0 { 1.0 } else { 0.45 };
        [angle.cos() * radius, angle.sin() * radius]
    })
}

pub(crate) fn caption_job(label: &str, side: f32) -> LayoutJob {
    let mut job = LayoutJob::simple(
        label.split_whitespace().collect::<Vec<_>>().join(" "),
        FontId::new(side * 0.17, FontFamily::Name("strong".into())),
        theme::TEXT,
        side * 0.84,
    );
    job.wrap.max_rows = 1;
    job.wrap.overflow_character = Some('…');
    job
}

pub(crate) fn key_tile(
    bot: Option<&Bot>,
    avatar: Option<&RgbaImage>,
    selected: bool,
    main: bool,
) -> RgbImage {
    let side = SIDE as f32;
    let background = theme::WELL.to_array();
    let mut tile = if let Some(avatar) = avatar {
        let extent = (side * AVATAR_SCALE).round() as u32;
        let avatar = imageops::resize(avatar, extent, extent, imageops::FilterType::Triangle);
        let mut canvas = RgbaImage::from_pixel(SIDE, SIDE, Rgba(background));
        imageops::overlay(
            &mut canvas,
            &avatar,
            i64::from((SIDE - extent) / 2),
            (side * AVATAR_CENTER_Y - extent as f32 / 2.0).round() as i64,
        );
        DynamicImage::ImageRgba8(canvas).into_rgb8()
    } else {
        RgbImage::from_pixel(
            SIDE,
            SIDE,
            Rgb([background[0], background[1], background[2]]),
        )
    };
    if let Some(bot) = bot {
        paint_caption(&mut tile, bot.label(), avatar.is_some());
        if bot.unread() {
            let [cx, cy] = unread_center(avatar.is_some() && !main).map(|position| position * side);
            let radius_squared = (UNREAD_RADIUS * side).powi(2);
            let outline_squared = ((UNREAD_RADIUS + UNREAD_OUTLINE) * side).powi(2);
            for (x, y, pixel) in tile.enumerate_pixels_mut() {
                let distance = (x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2);
                if distance <= radius_squared {
                    *pixel = Rgb(UNREAD_COLOR);
                } else if distance <= outline_squared {
                    *pixel = Rgb([background[0], background[1], background[2]]);
                }
            }
        }
        if main {
            let [cx, cy] = STAR_CENTER.map(|position| position * side);
            let radius = STAR_RADIUS * side;
            let points = star_points(radius * 0.72);
            let cross = |a: [f32; 2], b: [f32; 2]| a[0] * b[1] - a[1] * b[0];
            for (x, y, pixel) in tile.enumerate_pixels_mut() {
                let p = [x as f32 + 0.5 - cx, y as f32 + 0.5 - cy];
                let distance = p[0].powi(2) + p[1].powi(2);
                if distance <= radius.powi(2) {
                    let star = points
                        .iter()
                        .zip(points.iter().cycle().skip(1))
                        .any(|(&a, &b)| {
                            cross(a, p) >= 0.0
                                && cross(p, b) >= 0.0
                                && cross([b[0] - a[0], b[1] - a[1]], [p[0] - a[0], p[1] - a[1]])
                                    >= 0.0
                        });
                    *pixel = Rgb(if star { [255; 3] } else { STAR_BACKGROUND });
                } else if distance <= (radius + UNREAD_OUTLINE * side).powi(2) {
                    *pixel = Rgb([background[0], background[1], background[2]]);
                }
            }
        }
    }
    if selected {
        for offset in 1..3 {
            for x in offset..SIDE - offset {
                for (x, y) in [
                    (x, offset),
                    (x, SIDE - 1 - offset),
                    (offset, x),
                    (SIDE - 1 - offset, x),
                ] {
                    tile.put_pixel(x, y, Rgb(SELECTION));
                }
            }
        }
    }
    tile
}

fn paint_caption(tile: &mut RgbImage, label: &str, has_avatar: bool) {
    let [r, g, b, _] = theme::TEXT.to_array();
    // The one-row caption is centered on the key at its caption height.
    glyphs::draw(
        tile,
        caption_job(label, SIDE as f32),
        [r, g, b],
        3..SIDE as i32 - 3,
        |galley, _| {
            let center = galley.mesh_bounds.center();
            [
                (SIDE as f32 * 0.5 - center.x).round() as i32,
                (SIDE as f32 * caption_center(has_avatar) - center.y).round() as i32,
            ]
        },
    );
}

pub(crate) fn command_tile(label: &str) -> RgbImage {
    let [r, g, b, _] = theme::WELL.to_array();
    let mut tile = RgbImage::from_pixel(SIDE, SIDE, Rgb([r, g, b]));
    if !label.is_empty() {
        paint_caption(&mut tile, label, false);
    }
    tile
}
