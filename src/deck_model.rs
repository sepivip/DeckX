/// Hardware geometry in the model's face coordinates. UI layout scales these
/// coordinates into the available space, independent of the active mode.
pub(crate) struct Placement {
    pub center: [f32; 2],
    pub size: f32,
}

pub(crate) const LCD_PIXELS: u32 = AKP03E.lcd_pixels;

pub(crate) struct DeckModel {
    pub name: &'static str,
    pub face: [f32; 2],
    pub mark: &'static str,
    pub mark_position: [f32; 2],
    /// Side length of the square image uploaded to each LCD, in pixels.
    pub lcd_pixels: u32,
    /// Columns and rows of the row-major LCD positions.
    pub lcd_grid: [u32; 2],
    pub lcd: &'static [Placement],
    pub side: &'static [Placement],
    pub dials: &'static [Placement],
}

impl DeckModel {
    pub fn aspect(&self) -> f32 {
        self.face[0] / self.face[1]
    }
}

const fn place(x: f32, y: f32, size: f32) -> Placement {
    Placement {
        center: [x, y],
        size,
    }
}

// Top-down measurements from AJAZZ's 293N3.png. Indices follow the HID driver.
pub(crate) const AKP03E: DeckModel = DeckModel {
    name: "AJAZZ AKP03E",
    face: [141.0, 87.0],
    mark: "AJAZZ",
    mark_position: [17.0, 9.0],
    // Complete JPEG blocks avoid exposing repeated edge pixels on the LCD.
    lcd_pixels: 64,
    lcd_grid: [3, 2],
    lcd: &[
        place(25.0, 23.5, 17.0),
        place(45.5, 23.5, 17.0),
        place(66.0, 23.5, 17.0),
        place(25.0, 44.0, 17.0),
        place(45.5, 44.0, 17.0),
        place(66.0, 44.0, 17.0),
    ],
    side: &[
        place(25.0, 70.5, 13.2),
        place(45.5, 70.5, 13.2),
        place(66.0, 70.5, 13.2),
    ],
    dials: &[
        place(104.5, 33.0, 28.4),
        place(91.5, 68.5, 14.8),
        place(118.5, 68.5, 14.8),
    ],
};
