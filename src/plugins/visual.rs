use std::{
    fs::File,
    io::{Cursor, Read},
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use anyhow::{Result, ensure};
use base64::Engine;
use image::{DynamicImage, ImageReader, Limits, Rgb, RgbImage, RgbaImage, imageops};
use resvg::{tiny_skia, usvg};

use super::package::image_path;
use crate::deck_model::LCD_PIXELS;

const LIMIT: usize = 10 * 1024 * 1024;
const NESTED_SVG_LIMIT: usize = 1024 * 1024;

pub struct RenderSpec {
    pub root: PathBuf,
    pub image: Option<Arc<str>>,
    pub title: Option<String>,
    pub flash: Option<bool>,
}

pub fn render(spec: &RenderSpec, background: [u8; 3], custom_title: &str) -> RgbImage {
    let title = if custom_title.is_empty() {
        spec.title.as_deref().unwrap_or("")
    } else {
        custom_title
    };
    let mut tile = RgbImage::from_pixel(LCD_PIXELS, LCD_PIXELS, Rgb(background));
    let art = spec
        .image
        .as_deref()
        .and_then(|image| load(spec, image).ok());
    if let Some(art) = art {
        let image = DynamicImage::ImageRgba8(art)
            .resize(LCD_PIXELS, LCD_PIXELS, imageops::FilterType::Lanczos3)
            .into_rgba8();
        let left = (LCD_PIXELS - image.width()) / 2;
        let top = (LCD_PIXELS - image.height()) / 2;
        for (x, y, pixel) in image.enumerate_pixels() {
            let output = tile.get_pixel_mut(left + x, top + y);
            let opacity = u16::from(pixel[3]);
            for channel in 0..3 {
                output[channel] = ((u16::from(pixel[channel]) * opacity
                    + u16::from(output[channel]) * (255 - opacity))
                    / 255) as u8;
            }
        }
        if !title.is_empty() {
            let band = crate::profile::label_tile([15, 15, 15], title);
            let height = if title.lines().filter(|line| !line.trim().is_empty()).count() > 2 {
                34
            } else if title.chars().count() > 7 {
                24
            } else if title.chars().count() <= 3 {
                20
            } else {
                14
            };
            imageops::replace(
                &mut tile,
                &imageops::crop_imm(&band, 0, (LCD_PIXELS - height) / 2, LCD_PIXELS, height)
                    .to_image(),
                0,
                i64::from(LCD_PIXELS - height),
            );
        }
    } else if !title.is_empty() {
        tile = crate::profile::label_tile(background, title);
    }
    if let Some(ok) = spec.flash {
        let color = if ok { [34, 187, 117] } else { [233, 78, 76] };
        let inner = 2..LCD_PIXELS - 2;
        for y in 0..LCD_PIXELS {
            for x in 0..LCD_PIXELS {
                if !inner.contains(&x) || !inner.contains(&y) {
                    tile.put_pixel(x, y, Rgb(color));
                }
            }
        }
    }
    tile
}

fn load(spec: &RenderSpec, source: &str) -> Result<RgbaImage> {
    let (bytes, svg) = if let Some(data) = source.strip_prefix("data:image/") {
        let (kind, body) = data
            .split_once(";base64,")
            .ok_or_else(|| anyhow::anyhow!("Unsupported plugin image URI"))?;
        ensure!(body.len() < LIMIT * 2, "Plugin image is too large");
        let bytes = base64::engine::general_purpose::STANDARD.decode(body)?;
        (bytes, kind == "svg+xml")
    } else {
        let path = image_path(&spec.root, source)
            .ok_or_else(|| anyhow::anyhow!("Plugin image is missing"))?;
        let mut bytes = Vec::new();
        File::open(&path)?
            .take((LIMIT + 1) as u64)
            .read_to_end(&mut bytes)?;
        let svg = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"));
        (bytes, svg)
    };
    ensure!(bytes.len() <= LIMIT, "Plugin image is too large");
    if svg {
        return rasterize_svg(&bytes);
    }
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    Ok(reader.decode()?.into_rgba8())
}

fn svg_options(images: usvg::ImageHrefResolver<'static>) -> usvg::Options<'static> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    let fontdb = FONTS.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_font_data(include_bytes!("../../assets/Inter.ttf").to_vec());
        db.set_sans_serif_family("Inter");
        db.set_serif_family("Inter");
        db.set_monospace_family("Inter");
        Arc::new(db)
    });
    usvg::Options {
        font_family: "Inter".into(),
        fontdb: fontdb.clone(),
        image_href_resolver: images,
        ..Default::default()
    }
}

/// Resolves `<image>` references to inline SVG data only. File paths, UNC paths and URLs are
/// never opened. Raster data is dropped: this resvg build has no raster decoder, and dropping it
/// keeps nested images from skipping the limits `load` applies to plugin artwork.
pub(super) fn svg_images() -> usvg::ImageHrefResolver<'static> {
    usvg::ImageHrefResolver {
        resolve_data: Box::new(|mime: &str, data: Arc<Vec<u8>>, _: &usvg::Options| {
            if mime != "image/svg+xml" || data.len() > NESTED_SVG_LIMIT || gzip(&data) {
                return None;
            }
            // Nested SVG may not reference further images.
            let options = svg_options(usvg::ImageHrefResolver {
                resolve_data: Box::new(|_, _, _| None),
                resolve_string: Box::new(|_, _| None),
            });
            usvg::Tree::from_data(&data, &options)
                .ok()
                .map(usvg::ImageKind::SVG)
        }),
        resolve_string: Box::new(|_, _| None),
    }
}

fn gzip(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x1f, 0x8b])
}

pub(super) fn rasterize_svg(bytes: &[u8]) -> Result<RgbaImage> {
    // usvg inflates .svgz input without a size limit.
    ensure!(!gzip(bytes), "Compressed SVG images are not supported");
    let tree = usvg::Tree::from_data(bytes, &svg_options(svg_images()))?;
    let mut pixmap = tiny_skia::Pixmap::new(LCD_PIXELS, LCD_PIXELS).expect("fixed SVG canvas");
    let size = tree.size();
    let side = LCD_PIXELS as f32;
    let scale = (side / size.width()).min(side / size.height());
    let transform = tiny_skia::Transform::from_scale(scale, scale).post_translate(
        (side - size.width() * scale) / 2.0,
        (side - size.height() * scale) / 2.0,
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    // tiny-skia stores premultiplied colors; PNG encoding converts them to straight alpha.
    Ok(image::load_from_memory(&pixmap.encode_png()?)?.into_rgba8())
}
