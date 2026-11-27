//! Draws text onto native LCD key tiles from one shared glyph atlas.

use std::{
    ops::Range,
    sync::{Arc, LazyLock, Mutex},
};

use eframe::egui::{
    Color32, FontData, FontDefinitions, FontFamily, FontId,
    epaint::{
        AlphaFromCoverage, Fonts,
        text::{Galley, LayoutJob, PlacedRow},
    },
};
use image::RgbImage;

use crate::{deck_model::LCD_PIXELS, theme};

const ATLAS_SIDE: usize = 1024;
const COVERAGE: AlphaFromCoverage = AlphaFromCoverage::TwoCoverageMinusCoverageSq;

/// The Codex title family: OpenAI Sans from the installed Codex app when
/// available, then Inter and egui's fallbacks.
pub(crate) const CODEX_TITLE: &str = "codex-title";

/// The DeckX interface fonts plus the Codex title family.
static FONTS: LazyLock<Mutex<Fonts>> = LazyLock::new(|| {
    let mut definitions = theme::font_definitions();
    let mut title = vec!["Inter".to_owned()];
    if let Some(font) = crate::codex::title_font() {
        definitions.font_data.insert(
            "OpenAI Sans Medium".into(),
            Arc::new(FontData::from_owned(font)),
        );
        title.insert(0, "OpenAI Sans Medium".into());
    }
    title.extend(
        FontDefinitions::default()
            .families
            .remove(&FontFamily::Proportional)
            .unwrap_or_default(),
    );
    definitions
        .families
        .insert(FontFamily::Name(CODEX_TITLE.into()), title);
    Mutex::new(Fonts::new(ATLAS_SIDE, COVERAGE, definitions))
});

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct TitleStyle {
    pub font: &'static str,
    pub background: [u8; 3],
    pub text: [u8; 3],
    pub outline: Option<[u8; 3]>,
}

impl TitleStyle {
    pub fn render(self, title: &str) -> RgbImage {
        const SIDE: u32 = LCD_PIXELS;
        const INSET: i32 = 6;
        let mut tile = RgbImage::from_pixel(SIDE, SIDE, image::Rgb(self.background));
        if let Some(outline) = self.outline {
            for (x, y, pixel) in tile.enumerate_pixels_mut() {
                if !(2..SIDE - 2).contains(&x) || !(2..SIDE - 2).contains(&y) {
                    *pixel = image::Rgb(outline);
                }
            }
        }

        let mut job = LayoutJob::simple(
            title.split_whitespace().collect::<Vec<_>>().join(" "),
            FontId::new(11.0, FontFamily::Name(self.font.into())),
            Color32::WHITE,
            (SIDE as i32 - 2 * INSET - 1) as f32,
        );
        job.wrap.max_rows = 3;
        job.wrap.overflow_character = Some('…');
        // Left-align each row's ink and center the whole block vertically.
        draw(
            &mut tile,
            job,
            self.text,
            INSET..SIDE as i32 - INSET,
            |galley, row| {
                [
                    INSET - row.visuals.mesh_bounds.left().round() as i32,
                    (SIDE as f32 / 2.0 - galley.mesh_bounds.center().y).round() as i32,
                ]
            },
        );
        tile
    }
}

/// Lays out `job` at one pixel per point and blends it into `tile` in
/// `color`. `origin` gives each row's pixel offset, and each glyph lands at
/// that offset plus its rounded position in the galley. Pixels outside `clip`
/// on either axis are left alone.
pub(crate) fn draw(
    tile: &mut RgbImage,
    job: LayoutJob,
    color: [u8; 3],
    clip: Range<i32>,
    origin: impl Fn(&Galley, &PlacedRow) -> [i32; 2],
) {
    let mut fonts = FONTS.lock().unwrap_or_else(|error| error.into_inner());
    fonts.begin_pass(ATLAS_SIDE, COVERAGE);
    let galley = fonts.with_pixels_per_point(1.0).layout_job(job);
    if !galley.mesh_bounds.is_positive() {
        return;
    }
    let atlas = fonts.texture_atlas().image();
    for row in &galley.rows {
        let [left, top] = origin(&galley, row);
        let visuals = &row.visuals;
        // At 1 px/pt each glyph quad maps one atlas texel to one LCD pixel.
        for quad in visuals.mesh.vertices[visuals.glyph_vertex_range.clone()].chunks_exact(4) {
            let x = left + (row.pos.x + quad[0].pos.x).round() as i32;
            let y = top + (row.pos.y + quad[0].pos.y).round() as i32;
            let u = quad[0].uv.x as usize;
            let v = quad[0].uv.y as usize;
            let width = quad[3].uv.x as usize - u;
            let height = quad[3].uv.y as usize - v;
            for dy in 0..height {
                for dx in 0..width {
                    let px = x + dx as i32;
                    let py = y + dy as i32;
                    if clip.contains(&px) && clip.contains(&py) {
                        let alpha = u16::from(atlas[(u + dx, v + dy)].a());
                        let pixel = tile.get_pixel_mut(px as u32, py as u32);
                        for (channel, foreground) in pixel.0.iter_mut().zip(color) {
                            let blended =
                                u16::from(*channel) * (255 - alpha) + u16::from(foreground) * alpha;
                            *channel = ((blended + 127) / 255) as u8;
                        }
                    }
                }
            }
        }
    }
}
