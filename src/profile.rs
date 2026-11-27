use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, PoisonError},
    time::SystemTime,
};

use anyhow::{Context, Result, bail, ensure};
use font8x8::{BASIC_FONTS, UnicodeFonts};
use image::{ImageReader, Limits, Rgb, RgbImage, RgbaImage, codecs::jpeg::JpegEncoder, imageops};
use serde::{Deserialize, Serialize};

use crate::actions::{Action, ActionKind, PreparedAction};

use crate::deck_model::{AKP03E, LCD_PIXELS};

pub const DISPLAY_KEYS: usize = AKP03E.lcd.len();
pub const BUTTONS: usize = DISPLAY_KEYS + AKP03E.side.len();
pub const DIALS: usize = AKP03E.dials.len();

pub const TILE_COLORS: [[u8; 3]; DISPLAY_KEYS] = [[34, 34, 34]; DISPLAY_KEYS];

/// The longest single-line LCD label. It wraps after 7 characters.
pub const LABEL_CHARS: usize = 14;

/// Checks that a label fits an LCD key as `label_tile` draws it: one line of up to 14
/// characters, or up to three lines of up to 7.
pub fn check_label(label: &str) -> Result<()> {
    let lines: Vec<_> = label
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let fits = match lines.as_slice() {
        [] => true,
        [line] => line.chars().count() <= LABEL_CHARS,
        lines => lines.len() <= 3 && lines.iter().all(|line| line.chars().count() <= 7),
    };
    ensure!(
        fits,
        "LCD labels fit one line of {LABEL_CHARS} characters or up to three lines of 7"
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Control {
    Button(usize),
    Dial(usize, Direction),
}

impl Control {
    pub fn is_lcd(self) -> bool {
        matches!(self, Self::Button(index) if index < DISPLAY_KEYS)
    }

    pub fn label(self) -> String {
        match self {
            Self::Button(n) if n < DISPLAY_KEYS => format!("LCD key {}", n + 1),
            Self::Button(n) => format!("Side button {}", n - DISPLAY_KEYS + 1),
            Self::Dial(n, direction) => format!("Dial {} · {}", n + 1, direction.label()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    Counterclockwise,
    Press,
    Clockwise,
}

impl Direction {
    pub const ALL: [Self; 3] = [Self::Counterclockwise, Self::Press, Self::Clockwise];

    pub fn label(self) -> &'static str {
        match self {
            Self::Counterclockwise => "Turn left",
            Self::Press => "Press",
            Self::Clockwise => "Turn right",
        }
    }

    pub(crate) fn index(self) -> usize {
        match self {
            Self::Counterclockwise => 0,
            Self::Press => 1,
            Self::Clockwise => 2,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Button {
    pub label: String,
    pub image: Option<PathBuf>,
    pub action: Action,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[u8; 3]>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Layout {
    pub buttons: [Button; BUTTONS],
    pub dials: [[Action; 3]; DIALS],
}

impl Layout {
    pub fn with_media_defaults() -> Self {
        use ActionKind::*;
        Self {
            buttons: std::array::from_fn(|_| Button::default()),
            dials: [
                [VolumeDown, Mute, VolumeUp].map(Action::from_kind),
                [PreviousTrack, PlayPause, NextTrack].map(Action::from_kind),
                [None; 3].map(Action::from_kind),
            ],
        }
    }
    /// Binds an action to a control. A plugin encoder owns all three gestures of its dial, so
    /// binding one fills the dial, and binding anything else to a plugin dial clears the dial.
    pub fn set_action(&mut self, control: Control, action: Action) {
        match control {
            Control::Dial(index, _) if action.kind == ActionKind::Plugin => {
                self.dials[index] = std::array::from_fn(|_| action.clone());
            }
            Control::Dial(index, _) => {
                if self.dials[index]
                    .iter()
                    .any(|action| action.kind == ActionKind::Plugin)
                {
                    self.dials[index] = std::array::from_fn(|_| Action::default());
                }
                *self.action_mut(control) = action;
            }
            Control::Button(_) => *self.action_mut(control) = action,
        }
    }

    /// Swaps two controls' bindings. LCD keys swap their appearance too. Plugin dials move as a
    /// whole dial, and plugin bindings only move between controls of the same type.
    pub fn swap(&mut self, a: Control, b: Control) -> Result<()> {
        if [a, b]
            .iter()
            .any(|&control| self.action(control).kind == ActionKind::Plugin)
        {
            match (a, b) {
                (Control::Dial(i, _), Control::Dial(j, _)) => {
                    self.dials.swap(i, j);
                    return Ok(());
                }
                (Control::Button(_), Control::Button(_)) => {}
                _ => bail!("Move plugin bindings between controls of the same type"),
            }
        }
        if let (Control::Button(i), Control::Button(j)) = (a, b)
            && i < DISPLAY_KEYS
            && j < DISPLAY_KEYS
        {
            self.buttons.swap(i, j);
        } else {
            let action = self.action(a).clone();
            *self.action_mut(a) = self.action(b).clone();
            *self.action_mut(b) = action;
        }
        Ok(())
    }

    pub fn action(&self, control: Control) -> &Action {
        match control {
            Control::Button(index) => &self.buttons[index].action,
            Control::Dial(index, direction) => &self.dials[index][direction.index()],
        }
    }

    pub fn action_mut(&mut self, control: Control) -> &mut Action {
        match control {
            Control::Button(index) => &mut self.buttons[index].action,
            Control::Dial(index, direction) => &mut self.dials[index][direction.index()],
        }
    }

    pub fn prepare(&self, rotation: u16) -> Result<PreparedLayout> {
        let buttons = self
            .buttons
            .iter()
            .enumerate()
            .map(|(i, button)| {
                button
                    .action
                    .prepare()
                    .with_context(|| Control::Button(i).label())
            })
            .collect::<Result<Vec<_>>>()?;
        let dials = (0..DIALS)
            .map(|i| {
                Direction::ALL
                    .into_iter()
                    .map(|direction| {
                        let control = Control::Dial(i, direction);
                        self.action(control)
                            .prepare()
                            .with_context(|| control.label())
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .collect::<Result<Vec<_>>>()?;
        let images = (0..DISPLAY_KEYS)
            .map(|i| {
                let tile = self.tile(i).with_context(|| Control::Button(i).label())?;
                encode_tile(tile, rotation)
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(PreparedLayout {
            images,
            labels: std::array::from_fn(|index| self.buttons[index].label.clone()),
            colors: std::array::from_fn(|index| {
                self.buttons[index].color.unwrap_or(TILE_COLORS[index])
            }),
            custom_images: std::array::from_fn(|index| self.buttons[index].image.is_some()),
            buttons,
            dials,
        })
    }

    pub fn tile(&self, index: usize) -> Result<RgbImage> {
        let button = &self.buttons[index];
        let background = button.color.unwrap_or(TILE_COLORS[index]);
        if let Some(path) = &button.image {
            let image = key_photo(path)?;
            let mut tile = RgbImage::from_pixel(LCD_PIXELS, LCD_PIXELS, Rgb(background));
            // Blend transparency into the tile's background before converting to JPEG.
            for (pixel, foreground) in tile.pixels_mut().zip(image.pixels()) {
                let alpha = u16::from(foreground[3]);
                for channel in 0..3 {
                    pixel[channel] = ((u16::from(foreground[channel]) * alpha
                        + u16::from(pixel[channel]) * (255 - alpha))
                        / 255) as u8;
                }
            }
            return Ok(tile);
        }

        let label = if button.label.trim().is_empty() {
            if button.action.kind == ActionKind::None {
                format!("{}", index + 1)
            } else {
                button.action.kind.tile_label().to_owned()
            }
        } else {
            button.label.trim().to_ascii_uppercase()
        };
        Ok(label_tile(background, &label))
    }
}

pub struct PreparedLayout {
    pub images: Vec<Vec<u8>>,
    pub labels: [String; DISPLAY_KEYS],
    pub colors: [[u8; 3]; DISPLAY_KEYS],
    pub custom_images: [bool; DISPLAY_KEYS],
    buttons: Vec<PreparedAction>,
    dials: Vec<Vec<PreparedAction>>,
}

impl PreparedLayout {
    pub fn action(&self, control: Control) -> &PreparedAction {
        match control {
            Control::Button(index) => &self.buttons[index],
            Control::Dial(index, direction) => &self.dials[index][direction.index()],
        }
    }

    pub fn instances(&self) -> Vec<crate::plugins::Instance> {
        let controls = (0..BUTTONS).map(Control::Button).chain(
            (0..DIALS)
                .flat_map(|index| Direction::ALL.map(|direction| Control::Dial(index, direction))),
        );
        let mut seen = std::collections::HashSet::new();
        controls
            .filter_map(|control| {
                let PreparedAction::Plugin { action, context } = self.action(control) else {
                    return None;
                };
                seen.insert(context).then(|| crate::plugins::Instance {
                    context: context.clone(),
                    action: action.clone(),
                    control,
                })
            })
            .collect()
    }
}

type PhotoCache = HashMap<PathBuf, (Option<SystemTime>, u64, Arc<RgbaImage>)>;

/// Returns a key photo scaled to the LCD size. Decoded photos are cached by path and file
/// version, so redrawing a key or saving the workspace does not decode its photo again.
fn key_photo(path: &Path) -> Result<Arc<RgbaImage>> {
    static CACHE: OnceLock<Mutex<PhotoCache>> = OnceLock::new();
    const CAPACITY: usize = 128;
    let info = fs::metadata(path).with_context(|| format!("Could not open {}", path.display()))?;
    let version = (info.modified().ok(), info.len());
    let cache = CACHE.get_or_init(Default::default);
    if let Some((modified, len, photo)) = cache
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(path)
        && (*modified, *len) == version
    {
        return Ok(photo.clone());
    }
    let mut reader = ImageReader::open(path)
        .with_context(|| format!("Could not open {}", path.display()))?
        .with_guessed_format()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(80 * 1024 * 1024);
    reader.limits(limits);
    let photo = reader
        .decode()
        .with_context(|| format!("Invalid image: {}", path.display()))?
        .resize_to_fill(LCD_PIXELS, LCD_PIXELS, imageops::FilterType::Lanczos3)
        .into_rgba8();
    let photo = Arc::new(photo);
    let mut cache = cache.lock().unwrap_or_else(PoisonError::into_inner);
    if cache.len() >= CAPACITY {
        cache.clear();
    }
    cache.insert(path.to_owned(), (version.0, version.1, photo.clone()));
    Ok(photo)
}

pub fn encode_tile(tile: RgbImage, rotation: u16) -> Result<Vec<u8>> {
    ensure!(
        tile.dimensions() == (LCD_PIXELS, LCD_PIXELS),
        "{} LCD images must be {LCD_PIXELS} × {LCD_PIXELS} pixels",
        AKP03E.name
    );
    let tile = match rotation {
        90 => imageops::rotate90(&tile),
        180 => imageops::rotate180(&tile),
        270 => imageops::rotate270(&tile),
        _ => tile,
    };
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, 90).encode_image(&tile)?;
    Ok(bytes)
}

pub fn path() -> Result<PathBuf> {
    let local_data = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(known_local_app_data)
        .context("Windows did not report a local app data folder")?;
    Ok(local_data.join("DeckX").join("profile.json"))
}

/// Asks Windows for the local app data folder. MCP clients can start DeckX with a reduced
/// environment that has no LOCALAPPDATA variable.
fn known_local_app_data() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath};
    let mut path = std::ptr::null_mut();
    // Windows allocates `path`, which is freed below whether or not the call succeeded.
    let result =
        unsafe { SHGetKnownFolderPath(&FOLDERID_LocalAppData, 0, std::ptr::null_mut(), &mut path) };
    let folder = (result >= 0 && !path.is_null()).then(|| {
        let length = (0..).take_while(|&i| unsafe { *path.add(i) } != 0).count();
        let wide = unsafe { std::slice::from_raw_parts(path, length) };
        PathBuf::from(std::ffi::OsString::from_wide(wide))
    });
    unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(path.cast_const().cast())) };
    folder
}

pub(crate) fn save_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let folder = path
        .parent()
        .context("Settings path has no parent folder")?;
    fs::create_dir_all(folder).context("Could not create the DeckX settings folder")?;
    let mut file =
        tempfile::NamedTempFile::new_in(folder).context("Could not stage the settings")?;
    serde_json::to_writer_pretty(file.as_file_mut(), value)?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist(path)
        .context("Could not replace the settings file")?;
    Ok(())
}

pub(crate) fn label_tile(background: [u8; 3], label: &str) -> RgbImage {
    let mut tile = RgbImage::from_pixel(LCD_PIXELS, LCD_PIXELS, Rgb(background));
    let lines: Vec<_> = label
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let rows: Vec<Vec<char>> = if lines.len() > 1 {
        lines
            .into_iter()
            .take(3)
            .map(|line| line.chars().take(7).collect())
            .collect()
    } else {
        let chars: Vec<_> = lines
            .first()
            .copied()
            .unwrap_or("")
            .chars()
            .take(14)
            .collect();
        chars.chunks(7).map(|row| row.to_vec()).collect()
    };
    let scale = if rows.len() == 1 && rows[0].len() <= 3 {
        2
    } else {
        1
    };
    let width = 8 * scale;
    let line_height = width + 3;
    let first_y = (LCD_PIXELS as usize - rows.len() * line_height + 3) / 2;
    for (row, line) in rows.iter().enumerate() {
        let start_x = (LCD_PIXELS as usize - line.len() * width) / 2;
        for (column, &character) in line.iter().enumerate() {
            let glyph = BASIC_FONTS
                .get(character)
                .or_else(|| BASIC_FONTS.get('?'))
                .unwrap_or([0; 8]);
            for (y, bits) in glyph.into_iter().enumerate() {
                for x in 0..8 {
                    if bits & (1 << x) == 0 {
                        continue;
                    }
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let px = start_x + column * width + x * scale + dx;
                            let py = first_y + row * line_height + y * scale + dy;
                            tile.put_pixel(px as u32, py as u32, Rgb([249, 250, 252]));
                        }
                    }
                }
            }
        }
    }
    tile
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout::with_media_defaults()
    }

    #[test]
    fn plugin_actions_fill_their_dial_and_others_clear_it() {
        let mut layout = layout();
        let plugin = Action::plugin("com.example.volume".into());
        layout.set_action(Control::Dial(0, Direction::Press), plugin.clone());
        for direction in Direction::ALL {
            assert_eq!(layout.action(Control::Dial(0, direction)), &plugin);
        }
        layout.set_action(
            Control::Dial(0, Direction::Clockwise),
            Action::from_kind(ActionKind::VolumeUp),
        );
        assert_eq!(
            layout.action(Control::Dial(0, Direction::Clockwise)).kind,
            ActionKind::VolumeUp
        );
        for direction in [Direction::Counterclockwise, Direction::Press] {
            assert_eq!(
                layout.action(Control::Dial(0, direction)).kind,
                ActionKind::None
            );
        }
    }

    #[test]
    fn ordinary_dial_gestures_are_set_one_at_a_time() {
        let mut layout = layout();
        let before = layout
            .action(Control::Dial(1, Direction::Clockwise))
            .clone();
        layout.set_action(
            Control::Dial(1, Direction::Press),
            Action::from_kind(ActionKind::Mute),
        );
        assert_eq!(
            layout.action(Control::Dial(1, Direction::Clockwise)),
            &before
        );
    }

    #[test]
    fn swapping_moves_lcd_appearance_and_whole_plugin_dials() {
        let mut layout = layout();
        layout.buttons[0].label = "A".into();
        layout.buttons[1].label = "B".into();
        layout.swap(Control::Button(0), Control::Button(1)).unwrap();
        assert_eq!(layout.buttons[0].label, "B");

        let plugin = Action::plugin("com.example.volume".into());
        layout.set_action(Control::Dial(0, Direction::Press), plugin.clone());
        layout
            .swap(
                Control::Dial(0, Direction::Press),
                Control::Dial(2, Direction::Clockwise),
            )
            .unwrap();
        assert_eq!(
            layout.action(Control::Dial(2, Direction::Counterclockwise)),
            &plugin
        );
        assert_ne!(layout.action(Control::Dial(0, Direction::Press)), &plugin);

        assert!(
            layout
                .swap(Control::Dial(2, Direction::Press), Control::Button(0))
                .is_err()
        );
    }

    #[test]
    fn labels_must_fit_the_key() {
        assert!(check_label("").is_ok());
        assert!(check_label("Fourteen chars").is_ok());
        assert!(check_label("Fifteen chars!!").is_err());
        assert!(check_label("Open\nMy\nMail").is_ok());
        assert!(check_label("Open\nMy\nMail\nNow").is_err());
        assert!(check_label("Eight ch\nok").is_err());
    }

    #[test]
    fn windows_reports_the_local_app_data_folder() {
        let folder = known_local_app_data().expect("known folder");
        assert!(
            folder.is_absolute() && folder.is_dir(),
            "{}",
            folder.display()
        );
    }
}
