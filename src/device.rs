use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use hidapi::{DeviceInfo, HidApi, HidDevice};

use crate::profile::{Control, Direction, PreparedLayout};

const VENDOR_ID: u16 = 0x0300;
const PRODUCT_ID: u16 = 0x3002;
const USAGE_PAGE: u16 = 0xffa0;
const REPORT_SIZE: usize = 1025;
const WAKE_DELAY: Duration = Duration::from_millis(500);
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(5);

pub struct Deck {
    hid: HidDevice,
    last_keep_alive: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Input {
    pub control: Control,
    pub down: bool,
}

impl Deck {
    /// Open only the deck's vendor collection. The same USB device exposes a keyboard.
    pub fn open() -> Result<Self> {
        let api = HidApi::new().context("Could not initialize Windows HID")?;
        let mut matches = api.device_list().filter(|device| is_deck(device));
        let device = matches
            .next()
            .context("Plug in an AJAZZ AKP03E (0300:3002)")?;
        ensure!(matches.next().is_none(), "Connect one AKP03E at a time");
        let hid = device.open_device(&api).context(
            "Could not open the AKP03E. Exit Stream Dock from the system tray and try again",
        )?;
        Ok(Self {
            hid,
            last_keep_alive: Instant::now(),
        })
    }

    pub fn connect(brightness: u8) -> Result<Self> {
        // The AKP03E can accept HID writes while still showing its welcome screen.
        // Wake it on a temporary handle before opening the working connection.
        let wake = Self::open()?;
        wake.initialize_display(brightness)
            .context("Could not wake the AKP03E display")?;
        drop(wake);
        std::thread::sleep(WAKE_DELAY);

        let mut deck = Self::open()?;
        deck.initialize_display(brightness)
            .context("Could not initialize the AKP03E display after waking it")?;
        deck.keep_alive()?;
        Ok(deck)
    }

    fn initialize_display(&self, brightness: u8) -> Result<()> {
        self.command(b"DIS", &[])?;
        self.set_brightness(brightness)?;
        self.command(b"CLE", &[0, 0, 0, 0xff])?;
        self.commit_image()
    }

    pub fn apply(&self, layout: &PreparedLayout, brightness: u8) -> Result<()> {
        self.set_brightness(brightness)?;
        self.set_key_images(&layout.images)
    }

    pub(crate) fn set_key_images(&self, images: &[Vec<u8>]) -> Result<()> {
        for (index, jpeg) in images.iter().enumerate() {
            self.set_key_image(index, jpeg)
                .with_context(|| format!("Could not update LCD key {}", index + 1))?;
        }
        Ok(())
    }

    pub fn set_brightness(&self, brightness: u8) -> Result<()> {
        self.command(b"LIG", &[0, 0, brightness])
    }

    pub fn set_key_image(&self, index: usize, jpeg: &[u8]) -> Result<()> {
        let size = u16::try_from(jpeg.len()).context("Key image exceeds HID size field")?;
        let [high, low] = size.to_be_bytes();
        self.command(b"BAT", &[0, 0, high, low, index as u8 + 1])?;
        for chunk in jpeg.chunks(REPORT_SIZE - 1) {
            let mut report = [0; REPORT_SIZE];
            report[1..1 + chunk.len()].copy_from_slice(chunk);
            self.write(&report)?;
        }
        self.commit_image()
    }

    fn commit_image(&self) -> Result<()> {
        self.command(b"STP", &[])
    }

    /// Blanks the LCD keys, for when DeckX stops driving the deck.
    pub fn clear(&self) -> Result<()> {
        self.command(b"CLE", &[0, 0, 0, 0xff])?;
        self.commit_image()
    }

    pub fn read(&mut self) -> Result<Option<Input>> {
        self.keep_alive_if_due()?;
        let mut input = [0; REPORT_SIZE];
        let size = self
            .hid
            .read_timeout(&mut input, 50)
            .context("HID read failed")?;
        Ok(decode(&input[..size]))
    }

    pub(crate) fn keep_alive_if_due(&mut self) -> Result<()> {
        if self.last_keep_alive.elapsed() >= KEEP_ALIVE_INTERVAL {
            self.keep_alive()?;
        }
        Ok(())
    }

    fn keep_alive(&mut self) -> Result<()> {
        self.command(b"CONNECT", &[])?;
        self.last_keep_alive = Instant::now();
        Ok(())
    }

    fn command(&self, command: &[u8], payload: &[u8]) -> Result<()> {
        let mut report = [0; REPORT_SIZE];
        report[1..4].copy_from_slice(b"CRT");
        let command_end = 6 + command.len();
        report[6..command_end].copy_from_slice(command);
        report[command_end..command_end + payload.len()].copy_from_slice(payload);
        self.write(&report)
    }

    fn write(&self, report: &[u8; REPORT_SIZE]) -> Result<()> {
        let written = self.hid.write(report).context("HID write failed")?;
        // HIDAPI's Windows native backend returns zero for a synchronous WriteFile success.
        if written != 0 && written != report.len() {
            bail!("Short HID write ({written}/{} bytes)", report.len());
        }
        Ok(())
    }
}

/// Whether an error came from the HID transport, such as an unplugged or reset deck.
pub fn is_transport_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| cause.is::<hidapi::HidError>())
}

fn is_deck(device: &DeviceInfo) -> bool {
    device.vendor_id() == VENDOR_ID
        && device.product_id() == PRODUCT_ID
        && device.usage_page() == USAGE_PAGE
}

fn decode(input: &[u8]) -> Option<Input> {
    let input = input.strip_prefix(&[0]).unwrap_or(input);
    if !input.starts_with(b"ACK") {
        return None;
    }
    let code = *input.get(9)?;
    let down = *input.get(10)? != 0;

    use Control::{Button, Dial};
    use Direction::{Clockwise, Counterclockwise, Press};
    let control = match code {
        0x01..=0x06 => Button(usize::from(code - 1)),
        0x25 => Button(6),
        0x30 => Button(7),
        0x31 => Button(8),
        // Deck order is the large upper knob, then the two lower knobs left to right.
        0x35 => Dial(0, Press),
        0x50 => Dial(0, Counterclockwise),
        0x51 => Dial(0, Clockwise),
        0x33 => Dial(1, Press),
        0x90 => Dial(1, Counterclockwise),
        0x91 => Dial(1, Clockwise),
        0x34 => Dial(2, Press),
        0x60 => Dial(2, Counterclockwise),
        0x61 => Dial(2, Clockwise),
        _ => return None,
    };
    let down = down || matches!(control, Dial(_, Clockwise | Counterclockwise));
    Some(Input { control, down })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(code: u8, state: u8) -> Vec<u8> {
        let mut report = vec![0; 16];
        report[1..4].copy_from_slice(b"ACK");
        report[10] = code;
        report[11] = state;
        report
    }

    #[test]
    fn decodes_lcd_keys_and_side_buttons() {
        let input = decode(&report(0x03, 1)).unwrap();
        assert_eq!(input.control, Control::Button(2));
        assert!(input.down);
        let input = decode(&report(0x31, 0)).unwrap();
        assert_eq!(input.control, Control::Button(8));
        assert!(!input.down);
    }

    #[test]
    fn dial_reports_follow_the_physical_positions() {
        let codes = [[0x50, 0x35, 0x51], [0x90, 0x33, 0x91], [0x60, 0x34, 0x61]];
        for (index, gestures) in codes.into_iter().enumerate() {
            for (code, direction) in gestures.into_iter().zip(Direction::ALL) {
                let input = decode(&report(code, 1)).unwrap();
                assert_eq!(input.control, Control::Dial(index, direction));
                assert!(input.down);
                let input = decode(&report(code, 0)).unwrap();
                assert_eq!(input.control, Control::Dial(index, direction));
                assert_eq!(input.down, direction != Direction::Press);
            }
        }
    }

    #[test]
    fn ignores_reports_that_are_not_input() {
        assert!(decode(&report(0x7f, 1)).is_none());
        let mut other = report(0x01, 1);
        other[1..4].copy_from_slice(b"NAK");
        assert!(decode(&other).is_none());
        assert!(decode(&[0, b'A', b'C', b'K']).is_none());
    }

    #[test]
    fn only_hid_failures_count_as_transport_errors() {
        let lost = anyhow::Error::new(hidapi::HidError::HidApiErrorEmpty)
            .context("HID read failed")
            .context("Could not update LCD key 1");
        assert!(is_transport_error(&lost));
        assert!(!is_transport_error(&anyhow::anyhow!(
            "The selected page was removed"
        )));
    }
}
