use std::{
    fs::File,
    io::{Read, Take},
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use eframe::egui::{Color32, Context, FontFamily, FontId, epaint::text::LayoutJob};
use image::{Rgb, RgbImage};
use resvg::tiny_skia::{
    Color, Paint, Path as Outline, PathBuilder, Pixmap, Rect, Stroke, StrokeDash, Transform,
};
use serde::de::DeserializeOwned;
use tokio::{sync::watch, time::sleep};

use crate::{deck_model::LCD_PIXELS, glyphs, theme};

mod claude;
mod codex;

const MINUTE: u64 = 60_000;
const CACHED_AFTER: u64 = 5 * MINUTE;
const WEEK: u32 = 7 * 24 * 60;
const PROGRESS_WIDTH: u32 = 4;
const TEXT_INSET: i32 = PROGRESS_WIDTH as i32 + 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Provider {
    Codex,
    GrokBot,
    Claude,
}

impl Provider {
    fn poll_interval(self) -> Duration {
        Duration::from_secs(match self {
            Self::Codex => 90,
            Self::GrokBot | Self::Claude => 15,
        })
    }

    pub(crate) fn source_label(self) -> &'static str {
        match self {
            Self::Codex => "Codex account",
            Self::GrokBot => "Grok Bot’s saved usage",
            Self::Claude => "Claude Desktop’s saved usage",
        }
    }

    pub(crate) fn refresh_hint(self) -> &'static str {
        match self {
            Self::Codex => "Sign in to Codex with ChatGPT to see account limits.",
            Self::GrokBot => {
                "Open Settings → Usage & Billing in Grok Bot to update its saved usage."
            }
            Self::Claude => {
                "Right-click the Claude icon in the Windows notification area to refresh its saved usage."
            }
        }
    }

    async fn read(self) -> Option<Reading> {
        match self {
            Self::Codex => codex::read().await,
            Self::GrokBot => crate::grok::read_usage(now_ms()),
            Self::Claude => claude::read(now_ms()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Window {
    pub(crate) minutes: Option<u32>,
    pub(crate) remaining: u8,
    pub(crate) resets_at: Option<u64>,
}

impl Window {
    pub(crate) fn from_used(
        used_percent: f64,
        minutes: Option<u32>,
        resets_at: Option<u64>,
    ) -> Option<Self> {
        if !used_percent.is_finite() || used_percent < 0.0 {
            return None;
        }
        // Rounding down avoids showing unused quota the provider doesn't report.
        let remaining = (100.0 - used_percent).clamp(0.0, 100.0).floor() as u8;
        Some(Self {
            minutes: minutes.filter(|&minutes| minutes != 0),
            remaining,
            resets_at,
        })
    }

    fn period(minutes: Option<u32>) -> String {
        match minutes {
            Some(minutes) if minutes.is_multiple_of(1440) => format!("{}d", minutes / 1440),
            Some(minutes) if minutes.is_multiple_of(60) => format!("{}h", minutes / 60),
            Some(minutes) => format!("{minutes}m"),
            None => "Limit".into(),
        }
    }

    pub(crate) fn label(self) -> String {
        match self.minutes {
            Some(300) => "5-hour limit".into(),
            Some(WEEK) => "Weekly limit".into(),
            Some(_) => format!("{} limit", Self::period(self.minutes)),
            None => "Usage limit".into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Reading {
    pub(crate) windows: [Option<Window>; 2],
    pub(crate) observed_at: u64,
    pub(crate) expires_at: u64,
}

impl Reading {
    pub(crate) fn is_cached(self, now: u64) -> bool {
        now.saturating_sub(self.observed_at) >= CACHED_AFTER
    }

    pub(crate) fn current(mut self, now: u64) -> Option<Self> {
        if now < self.observed_at || now >= self.expires_at {
            return None;
        }
        for window in &mut self.windows {
            if window.is_some_and(|window| window.resets_at.is_some_and(|reset| reset <= now)) {
                *window = None;
            }
        }
        self.windows.iter().any(Option::is_some).then_some(self)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Report {
    #[default]
    Waiting,
    Unavailable,
    Available(Reading),
}

#[derive(Clone, Default)]
pub(crate) struct Readings(Arc<Mutex<[Report; 3]>>);

impl Readings {
    pub(crate) fn get(&self, provider: Provider) -> Report {
        let result = self.0.lock().unwrap_or_else(|err| err.into_inner())[provider as usize];
        match result {
            Report::Available(reading) => reading
                .current(now_ms())
                .map_or(Report::Unavailable, Report::Available),
            result => result,
        }
    }

    fn publish(&self, provider: Provider, report: Report) {
        self.0.lock().unwrap_or_else(|err| err.into_inner())[provider as usize] = report;
    }

    #[cfg(test)]
    pub(crate) fn fixture(&self, provider: Provider, report: Report) {
        self.publish(provider, report);
    }
}

#[derive(Default)]
pub(crate) struct Monitor {
    active: Option<watch::Sender<Option<Provider>>>,
    pub(crate) readings: Readings,
}

impl Monitor {
    pub(crate) fn start(ctx: Context) -> Self {
        let (active, wanted) = watch::channel(None);
        let readings = Readings::default();
        let worker = readings.clone();
        let _ = std::thread::Builder::new()
            .name("DeckX usage limits".into())
            .spawn(move || {
                if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    runtime.block_on(watch_usage(wanted, worker, ctx));
                }
            });
        Self {
            active: Some(active),
            readings,
        }
    }

    pub(crate) fn track(&self, provider: Option<Provider>) {
        if let Some(active) = &self.active {
            active.send_if_modified(|current| {
                if *current == provider {
                    return false;
                }
                *current = provider;
                true
            });
        }
    }
}

async fn watch_usage(
    mut wanted: watch::Receiver<Option<Provider>>,
    readings: Readings,
    ctx: Context,
) {
    loop {
        let provider = match wanted.wait_for(Option::is_some).await {
            Ok(next) => next.expect("the current preset has a usage key"),
            Err(_) => return,
        };
        loop {
            let reading = tokio::select! {
                reading = provider.read() => reading,
                changed = wanted.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    break;
                }
            };
            let report = reading.map_or(Report::Unavailable, Report::Available);
            readings.publish(provider, report);
            ctx.request_repaint();
            tokio::select! {
                () = sleep(provider.poll_interval()) => {}
                changed = wanted.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    break;
                }
            }
        }
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

/// Read a quota cache through a bounded stream, including when its writer replaces it.
pub(crate) fn read_json<T: DeserializeOwned>(path: &Path, max_bytes: u64) -> Option<T> {
    let mut bytes = Vec::new();
    let mut stream: Take<File> = File::open(path).ok()?.take(max_bytes + 1);
    stream.read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= max_bytes)
        .then(|| serde_json::from_slice(&bytes).ok())
        .flatten()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KeyFace {
    heading: &'static str,
    windows: [Option<(Option<u32>, u8)>; 2],
    placeholder: &'static str,
}

impl KeyFace {
    pub(crate) fn from_report(report: Report) -> Self {
        match report {
            Report::Available(reading) => Self {
                heading: if reading.is_cached(now_ms()) {
                    "Last seen"
                } else {
                    "Usage left"
                },
                windows: reading
                    .windows
                    .map(|window| window.map(|window| (window.minutes, window.remaining))),
                placeholder: "",
            },
            Report::Waiting | Report::Unavailable => Self {
                heading: "Usage",
                windows: [None; 2],
                placeholder: if report == Report::Waiting {
                    "…"
                } else {
                    "N/A"
                },
            },
        }
    }

    pub(crate) fn render(self, border: Color32) -> RgbImage {
        let rgb = |color: Color32| [color.r(), color.g(), color.b()];
        let background = Rgb(rgb(theme::ACCENT_WELL));
        let windows: Vec<_> = self.windows.into_iter().flatten().collect();
        let remaining = windows.iter().map(|&(_, remaining)| remaining).min();
        let mut tile = draw_key(remaining, border);
        let side = LCD_PIXELS as i32;
        let key_center = LCD_PIXELS as f32 / 2.0;
        let text_bounds = TEXT_INSET..side - TEXT_INSET;
        let mut line = |text: &str, y: f32, size: f32, color| {
            let mut layer = RgbImage::from_pixel(LCD_PIXELS, LCD_PIXELS, background);
            let foreground = rgb(color);
            glyphs::draw(
                &mut layer,
                LayoutJob::simple(
                    text.to_owned(),
                    FontId::new(size, FontFamily::Name("strong".into())),
                    Color32::WHITE,
                    f32::INFINITY,
                ),
                foreground,
                text_bounds.clone(),
                |galley, _| {
                    let center = galley.mesh_bounds.center();
                    [
                        (key_center - center.x).round() as i32,
                        (y - center.y).round() as i32,
                    ]
                },
            );
            // Antialiasing and glyph bearings can shift the visible center away from the mesh.
            let threshold = background[0].midpoint(foreground[0]);
            let mut left = side;
            let mut right = -1;
            for (x, _, pixel) in layer.enumerate_pixels() {
                if pixel[0] >= threshold {
                    left = left.min(x as i32);
                    right = right.max(x as i32);
                }
            }
            let shift = (side - 1 - left - right) / 2;
            for (x, y, pixel) in layer.enumerate_pixels() {
                let x = x as i32 + shift;
                if *pixel != background && text_bounds.contains(&x) {
                    tile.put_pixel(x as u32, y, *pixel);
                }
            }
        };
        line(self.heading, key_center - 17.0, 9.0, theme::TEXT);
        match windows.as_slice() {
            [] => line(self.placeholder, key_center, 17.0, theme::MUTED),
            &[(minutes, remaining)] => {
                let size = if remaining == 100 { 17.0 } else { 20.0 };
                line(&format!("{remaining}%"), key_center, size, theme::TEXT);
                line(
                    &Window::period(minutes),
                    key_center + 17.0,
                    11.0,
                    theme::MUTED,
                );
            }
            pairs => {
                for (index, &(minutes, remaining)) in pairs.iter().enumerate() {
                    let period = Window::period(minutes);
                    let text = format!("{period} {remaining}%");
                    let size = if remaining == 100 { 10.5 } else { 12.0 };
                    line(
                        &text,
                        key_center - 1.0 + 15.0 * index as f32,
                        size,
                        theme::TEXT,
                    );
                }
            }
        }
        tile
    }
}

fn draw_key(remaining: Option<u8>, border: Color32) -> RgbImage {
    let color = |value: Color32| Color::from_rgba8(value.r(), value.g(), value.b(), value.a());
    let transform = Transform::identity();
    let width = if remaining.is_some() {
        PROGRESS_WIDTH as f32
    } else {
        2.0
    };
    let fill = if border == theme::LINE {
        theme::PAGE_ACTIVE
    } else {
        theme::TEXT
    };
    let side = LCD_PIXELS as f32;
    let mut tile = Pixmap::new(LCD_PIXELS, LCD_PIXELS).expect("fixed LCD image size");
    tile.fill(color(if remaining == Some(100) { fill } else { border }));
    let mut paint = Paint::default();
    paint.set_color(color(theme::ACCENT_WELL));
    let inner = Rect::from_xywh(width, width, side - 2.0 * width, side - 2.0 * width)
        .expect("usage border fits the LCD");
    tile.fill_rect(inner, &paint, transform, None);

    if let Some(percent @ 1..=99) = remaining {
        let length = 4.0 * (side - width);
        let stroke = Stroke {
            width,
            dash: StrokeDash::new(vec![length * f32::from(percent) / 100.0, length], 0.0),
            ..Default::default()
        };
        paint.set_color(color(fill));
        tile.stroke_path(
            &key_perimeter(width / 2.0),
            &paint,
            &stroke,
            transform,
            None,
        );
    }

    // The LCD receives the entire square. Any corner clipping belongs to the keycap.
    let mut image = RgbImage::new(LCD_PIXELS, LCD_PIXELS);
    for (pixel, source) in image.pixels_mut().zip(tile.pixels()) {
        *pixel = Rgb([source.red(), source.green(), source.blue()]);
    }
    image
}

/// Starts at the top midpoint, then follows the LCD perimeter clockwise.
fn key_perimeter(inset: f32) -> Outline {
    let side = LCD_PIXELS as f32;
    let hi = side - inset;
    let mut path = PathBuilder::new();
    path.move_to(side / 2.0, inset);
    path.line_to(hi, inset);
    path.line_to(hi, hi);
    path.line_to(inset, hi);
    path.line_to(inset, inset);
    path.close();
    path.finish().expect("rectangular usage key path")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_progress_outline_starts_at_top_center_and_fills_clockwise() {
        let track = theme::LINE.r();
        let fill = theme::PAGE_ACTIVE.r();
        let edge = LCD_PIXELS - 1;
        let middle = LCD_PIXELS / 2;
        let red = |tile: &RgbImage, (x, y)| tile.get_pixel(x, y)[0];
        let boundaries = [
            [(edge, middle - 2), (edge, middle + 1)],
            [(middle + 1, edge), (middle - 2, edge)],
            [(0, middle + 1), (0, middle - 2)],
        ];
        let empty = draw_key(Some(0), theme::LINE);
        assert_eq!(red(&empty, (0, 0)), track);
        for inset in 0..PROGRESS_WIDTH {
            assert_eq!(red(&empty, (middle, inset)), track);
            assert_eq!(red(&empty, (inset, middle)), track);
        }
        assert_eq!(
            red(&empty, (middle, PROGRESS_WIDTH)),
            theme::ACCENT_WELL.r()
        );
        for (percent, [before, after]) in [25, 50, 75].into_iter().zip(boundaries) {
            let tile = draw_key(Some(percent), theme::LINE);
            assert_eq!(red(&tile, (middle, 0)), fill);
            assert_eq!(red(&tile, (middle - 1, 0)), track);
            assert_eq!(red(&tile, before), fill);
            assert_eq!(red(&tile, after), track);
        }
        let full = draw_key(Some(100), theme::LINE);
        for point in [(0, 0), (edge, 0), (edge, edge), (0, edge), (middle, 0)] {
            assert_eq!(red(&full, point), fill);
        }
    }

    #[test]
    fn the_usage_outline_tracks_the_tightest_window() {
        let edge = LCD_PIXELS - 1;
        let middle = LCD_PIXELS / 2;
        let face = KeyFace {
            heading: "Usage left",
            windows: [Some((Some(300), 75)), Some((Some(WEEK), 25))],
            placeholder: "",
        };
        let tile = face.render(theme::LINE);
        assert_eq!(tile.get_pixel(edge, middle - 2)[0], theme::PAGE_ACTIVE.r());
        assert_eq!(tile.get_pixel(edge, middle + 1)[0], theme::LINE.r());
        let unavailable = KeyFace::from_report(Report::Unavailable).render(theme::LINE);
        assert_eq!(unavailable.get_pixel(middle, 0)[0], theme::LINE.r());
        assert_eq!(unavailable.get_pixel(middle, 4)[0], theme::ACCENT_WELL.r());
    }

    #[test]
    fn full_usage_text_has_clearance_inside_the_outline() {
        for first in [None, Some((Some(300), 100))] {
            let tile = KeyFace {
                heading: "Usage left",
                windows: [first, Some((Some(WEEK), 100))],
                placeholder: "",
            }
            .render(theme::LINE);
            for y in PROGRESS_WIDTH..LCD_PIXELS - PROGRESS_WIDTH {
                for inset in PROGRESS_WIDTH..TEXT_INSET as u32 {
                    for x in [inset, LCD_PIXELS - 1 - inset] {
                        assert_eq!(tile.get_pixel(x, y)[0], theme::ACCENT_WELL.r());
                    }
                }
            }
        }
    }

    #[test]
    fn the_akp03e_jpeg_uses_full_blocks_and_keeps_the_border_even() {
        use image::{ImageFormat, imageops};

        let decoded = |percent, rotation| {
            let tile = image::load_from_memory_with_format(
                &crate::profile::encode_tile(draw_key(Some(percent), theme::LINE), rotation)
                    .unwrap(),
                ImageFormat::Jpeg,
            )
            .unwrap()
            .into_rgb8();
            assert_eq!(tile.dimensions(), (64, 64));
            match rotation {
                90 => imageops::rotate270(&tile),
                180 => imageops::rotate180(&tile),
                270 => imageops::rotate90(&tile),
                _ => tile,
            }
        };
        let edge = LCD_PIXELS - 1;
        let near = LCD_PIXELS / 4;
        let far = edge - near;
        for rotation in [0, 90, 180, 270] {
            let tile = decoded(25, rotation);

            let bright = |x, y| tile.get_pixel(x, y)[0] > 130;
            for y in 0..PROGRESS_WIDTH {
                assert!(bright(far, y), "filled top border at y={y}");
                assert!(!bright(near, y), "dim top border at y={y}");
                assert!(bright(edge - y, near), "filled right border");
                assert!(!bright(edge - y, far), "dim right border");
            }
            assert!(bright(edge, 0), "filled outer corner");

            let full = decoded(100, rotation);
            for cross in [near, far] {
                for distance in 0..=PROGRESS_WIDTH {
                    let opposite = edge - distance;
                    for (x, y) in [
                        (cross, distance),
                        (opposite, cross),
                        (cross, opposite),
                        (distance, cross),
                    ] {
                        assert_eq!(full.get_pixel(x, y)[0] > 130, distance < PROGRESS_WIDTH);
                    }
                }
            }
        }
    }

    #[tokio::test]
    #[ignore = "requires signed-in companion apps with recent usage"]
    async fn installed_providers_return_usable_windows() {
        for provider in [Provider::Codex, Provider::GrokBot, Provider::Claude] {
            let reading = provider.read().await;
            assert!(
                reading.is_some_and(|reading| reading.current(now_ms()).is_some()),
                "{provider:?} did not report a usable window"
            );
        }
    }
}
