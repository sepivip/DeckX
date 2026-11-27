//! The two outer side buttons and the temporary LCD panorama.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use image::imageops;

use crate::{
    deck_model::{AKP03E, LCD_PIXELS},
    device::Input,
    profile::{self, BUTTONS, Control, DISPLAY_KEYS},
};

const TAP_TIME: Duration = Duration::from_millis(250);
const NEXT_PRESS: Duration = Duration::from_millis(600);
pub(crate) const HOLD_TIME: Duration = Duration::from_secs(2);
const DISPLAY_TIME: Duration = Duration::from_secs(5);
const LEFT: Control = Control::Button(DISPLAY_KEYS);
const RIGHT: Control = Control::Button(BUTTONS - 1);
const LEFT_DOWN: Input = Input {
    control: LEFT,
    down: true,
};
const LEFT_UP: Input = Input {
    control: LEFT,
    down: false,
};
const RIGHT_DOWN: Input = Input {
    control: RIGHT,
    down: true,
};
const RIGHT_UP: Input = Input {
    control: RIGHT,
    down: false,
};

#[derive(Clone, Copy, Default)]
enum Gesture {
    #[default]
    Ready,
    LeftPressed {
        until: Instant,
    },
    LeftTapped {
        until: Instant,
    },
    RightHeld {
        until: Instant,
    },
}

impl Gesture {
    fn pending_inputs(self) -> &'static [Input] {
        match self {
            Self::Ready => &[],
            Self::LeftPressed { .. } => &[LEFT_DOWN],
            Self::LeftTapped { .. } => &[LEFT_DOWN, LEFT_UP],
            Self::RightHeld { .. } => &[LEFT_DOWN, LEFT_UP, RIGHT_DOWN],
        }
    }
}

#[derive(Default)]
pub(crate) struct EasterEgg {
    gesture: Gesture,
    display_until: Option<Instant>,
    ignore_right_release: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Display {
    Sydney,
    Restore,
}

#[derive(Default)]
pub(crate) struct Step {
    pub display: Option<Display>,
    pub inputs: [Option<Input>; 4],
}

impl EasterEgg {
    pub(crate) fn visible(&self) -> bool {
        self.display_until.is_some()
    }

    pub(crate) fn poll(&mut self, enabled: bool, now: Instant, mut input: Option<Input>) -> Step {
        let mut step = Step::default();
        if self.ignore_right_release && input.is_some_and(|input| input.control == RIGHT) {
            self.ignore_right_release = input != Some(RIGHT_UP);
            input = None;
        }

        if let Some(until) = self.display_until {
            if !enabled || now >= until {
                self.display_until = None;
                step.display = Some(Display::Restore);
            } else {
                return step;
            }
        }

        if let Gesture::RightHeld { until } = self.gesture
            && enabled
            && now >= until
        {
            self.gesture = Gesture::Ready;
            self.display_until = Some(now + DISPLAY_TIME);
            self.ignore_right_release = input != Some(RIGHT_UP);
            step.display = Some(Display::Sydney);
            return step;
        }

        let press = input.filter(|input| input.down).map(|input| input.control);
        let interrupted_by = |expected| press.is_some_and(|control| control != expected);
        let canceled = match self.gesture {
            Gesture::Ready => false,
            Gesture::LeftPressed { until } => now >= until || interrupted_by(LEFT),
            Gesture::LeftTapped { until } => now >= until || interrupted_by(RIGHT),
            Gesture::RightHeld { .. } => input == Some(RIGHT_UP) || interrupted_by(RIGHT),
        };
        if !enabled || canceled {
            // A missed or interrupted sequence still runs the usual bindings in input order.
            for (slot, &pending) in step.inputs.iter_mut().zip(self.gesture.pending_inputs()) {
                *slot = Some(pending);
            }
            self.gesture = Gesture::Ready;
        }
        // Releasing the left button before pressing the right works on devices with single-key rollover.
        match (self.gesture, input) {
            (Gesture::Ready, Some(LEFT_DOWN)) if enabled => {
                self.gesture = Gesture::LeftPressed {
                    until: now + TAP_TIME,
                };
            }
            (Gesture::LeftPressed { .. }, Some(LEFT_UP)) => {
                self.gesture = Gesture::LeftTapped {
                    until: now + NEXT_PRESS,
                };
            }
            (Gesture::LeftTapped { .. }, Some(RIGHT_DOWN)) => {
                self.gesture = Gesture::RightHeld {
                    until: now + HOLD_TIME,
                };
            }
            (Gesture::LeftPressed { .. }, Some(LEFT_DOWN))
            | (Gesture::RightHeld { .. }, Some(RIGHT_DOWN)) => {}
            _ => step.inputs[3] = input,
        }
        step
    }
}

/// Splits the photo in LCD order and uses the same size and rotation as regular key art.
pub(crate) fn sydney(rotation: u16) -> Result<Vec<Vec<u8>>> {
    let [columns, rows] = AKP03E.lcd_grid;
    let photo = image::load_from_memory(include_bytes!("../assets/sydney-sweeney.jpg"))
        .context("Could not decode the Sydney Sweeney photo")?;
    let canvas = photo
        .resize_to_fill(
            columns * LCD_PIXELS,
            rows * LCD_PIXELS,
            imageops::FilterType::Lanczos3,
        )
        .into_rgb8();
    (0..DISPLAY_KEYS as u32)
        .map(|index| {
            let x = (index % columns) * LCD_PIXELS;
            let y = (index / columns) * LCD_PIXELS;
            let tile = imageops::crop_imm(&canvas, x, y, LCD_PIXELS, LCD_PIXELS).to_image();
            profile::encode_tile(tile, rotation)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delivered(step: Step) -> Vec<Input> {
        step.inputs.into_iter().flatten().collect()
    }

    fn tap_left(egg: &mut EasterEgg, now: Instant) -> Instant {
        assert_eq!(egg.poll(true, now, Some(LEFT_DOWN)).inputs, [None; 4]);
        let released = now + Duration::from_millis(110);
        assert_eq!(egg.poll(true, released, Some(LEFT_UP)).inputs, [None; 4]);
        released
    }

    #[test]
    fn left_tap_and_two_second_right_hold_show_sydney_once() {
        let mut egg = EasterEgg::default();
        let left_released = tap_left(&mut egg, Instant::now());
        let right_pressed = left_released + Duration::from_millis(180);
        assert_eq!(
            egg.poll(true, right_pressed, Some(RIGHT_DOWN)).inputs,
            [None; 4]
        );
        let reveal = right_pressed + HOLD_TIME;
        assert_eq!(
            egg.poll(true, reveal - Duration::from_millis(1), None)
                .display,
            None
        );
        let shown = egg.poll(true, reveal, None);
        assert_eq!(shown.display, Some(Display::Sydney));
        assert_eq!(shown.inputs, [None; 4]);
        assert!(egg.visible());

        let regular = Some(Input {
            control: Control::Button(0),
            down: true,
        });
        assert_eq!(
            egg.poll(true, reveal + Duration::from_secs(1), regular)
                .inputs,
            [None; 4]
        );
        assert_eq!(
            egg.poll(true, reveal + Duration::from_secs(4), None)
                .display,
            None
        );
        let end = reveal + DISPLAY_TIME;
        assert_eq!(egg.poll(true, end, None).display, Some(Display::Restore));
        assert!(!egg.visible());
        assert_eq!(egg.poll(true, end + HOLD_TIME, None).display, None);
        assert_eq!(
            egg.poll(true, end + HOLD_TIME, Some(RIGHT_UP)).inputs,
            [None; 4]
        );
        assert_eq!(
            delivered(egg.poll(true, end + HOLD_TIME, Some(RIGHT_DOWN))),
            vec![RIGHT_DOWN]
        );
    }

    #[test]
    fn ordinary_presses_and_incomplete_sequences_keep_their_bindings() {
        let mut egg = EasterEgg::default();
        let start = Instant::now();
        for input in [LEFT_DOWN, LEFT_UP, RIGHT_DOWN, RIGHT_UP] {
            assert_eq!(delivered(egg.poll(false, start, Some(input))), vec![input]);
        }

        let left_released = tap_left(&mut egg, start);
        let expired = left_released + NEXT_PRESS;
        assert_eq!(
            egg.poll(true, expired - Duration::from_millis(1), None)
                .inputs,
            [None; 4]
        );
        assert_eq!(
            delivered(egg.poll(true, expired, Some(RIGHT_DOWN))),
            vec![LEFT_DOWN, LEFT_UP, RIGHT_DOWN]
        );
        assert_eq!(egg.poll(true, expired + HOLD_TIME, None).display, None);
        egg.poll(true, expired + HOLD_TIME, Some(RIGHT_UP));

        let left_released = tap_left(&mut egg, start + Duration::from_secs(5));
        egg.poll(true, left_released, Some(RIGHT_DOWN));
        let short_hold = egg.poll(true, left_released + Duration::from_secs(1), Some(RIGHT_UP));
        assert_eq!(short_hold.display, None);
        assert_eq!(
            delivered(short_hold),
            vec![LEFT_DOWN, LEFT_UP, RIGHT_DOWN, RIGHT_UP]
        );
    }

    #[test]
    fn held_left_button_is_an_ordinary_press() {
        let mut egg = EasterEgg::default();
        let start = Instant::now();
        egg.poll(true, start, Some(LEFT_DOWN));
        assert_eq!(
            delivered(egg.poll(true, start + TAP_TIME, None)),
            vec![LEFT_DOWN]
        );
        let left_released = start + Duration::from_secs(1);
        assert_eq!(
            delivered(egg.poll(true, left_released, Some(LEFT_UP))),
            vec![LEFT_UP]
        );
        assert_eq!(
            delivered(egg.poll(true, left_released, Some(RIGHT_DOWN))),
            vec![RIGHT_DOWN]
        );
        assert_eq!(
            egg.poll(true, left_released + HOLD_TIME, None).display,
            None
        );
    }

    #[test]
    fn disabling_the_setting_restores_the_layout_immediately() {
        let mut egg = EasterEgg::default();
        let left_released = tap_left(&mut egg, Instant::now());
        egg.poll(true, left_released, Some(RIGHT_DOWN));
        let reveal = left_released + HOLD_TIME;
        assert_eq!(egg.poll(true, reveal, None).display, Some(Display::Sydney));
        assert_eq!(
            egg.poll(false, reveal, Some(RIGHT_UP)).display,
            Some(Display::Restore)
        );
        assert!(!egg.visible());
    }

    #[test]
    fn sydney_uploads_every_tile_in_the_decks_native_format() {
        let tiles = sydney(90).unwrap();
        assert_eq!(tiles.len(), DISPLAY_KEYS);
        for (index, tile) in tiles.iter().enumerate() {
            assert_eq!(image::guess_format(tile).unwrap(), image::ImageFormat::Jpeg);
            let tile = image::load_from_memory(tile).unwrap();
            assert_eq!((tile.width(), tile.height()), (LCD_PIXELS, LCD_PIXELS));
            assert!(!tiles[..index].iter().any(|other| other == &tiles[index]));
        }
    }
}
