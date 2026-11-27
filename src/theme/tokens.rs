use eframe::egui::{Color32, Margin, Shadow, Vec2, vec2};

pub const CANVAS: Color32 = Color32::from_gray(45);
pub const PANEL: Color32 = Color32::from_gray(41);
pub const SURFACE: Color32 = Color32::from_gray(59);
pub const WELL: Color32 = Color32::from_gray(34);
pub const LINE: Color32 = Color32::from_gray(71);
pub const DIVIDER: Color32 = Color32::from_gray(32);
pub const PAGE_TRACK: Color32 = Color32::from_gray(55);
pub const PAGE_ACTIVE: Color32 = Color32::from_gray(200);
pub const TEXT: Color32 = Color32::from_gray(245);
pub const MUTED: Color32 = Color32::from_gray(185);
pub const DIM: Color32 = Color32::from_gray(148);
pub const ACCENT: Color32 = Color32::from_rgb(0, 123, 255);
pub const ACCENT_WELL: Color32 = Color32::from_rgb(35, 59, 80);
pub const AGENT_TITLE: [u8; 3] = [49, 53, 64];
pub const MINT: Color32 = Color32::from_rgb(120, 221, 180);
pub const GOLD: Color32 = Color32::from_rgb(244, 196, 117);
pub const ERROR: Color32 = Color32::from_rgb(255, 157, 164);

pub(super) const HOVER: Color32 = Color32::from_gray(69);
pub(super) const PRESSED: Color32 = Color32::from_gray(77);
pub(super) const PRIMARY: Color32 = Color32::from_rgb(0, 88, 181);
pub(super) const PRIMARY_HOVER: Color32 = Color32::from_rgb(0, 101, 198);
pub(super) const PRIMARY_PRESSED: Color32 = Color32::from_rgb(0, 70, 146);
pub(super) const SELECTED_HOVER: Color32 = Color32::from_rgb(38, 72, 105);
pub(super) const PREVIEW_OVERLAY: Color32 = Color32::from_black_alpha(100);

pub const CAPTION: f32 = 11.0;
pub const DETAIL: f32 = 12.0;
pub const BODY: f32 = 13.0;
pub const ROW_TITLE: f32 = 14.0;
pub const SECTION_TITLE: f32 = 15.0;
pub const EMPTY_TITLE: f32 = 17.0;
pub const TOOLBAR_TITLE: f32 = 18.0;
pub const DIALOG_TITLE: f32 = 19.0;
pub const PAGE_TITLE: f32 = 20.0;

pub const TIGHT: f32 = 4.0;
pub const GAP: f32 = 8.0;
pub const SECTION_GAP: f32 = 12.0;
pub const INSET: f32 = 16.0;
pub const PAGE_INSET: f32 = 24.0;
pub const STACK_GAP: f32 = 3.0;
pub const FIELD_HEIGHT: f32 = 28.0;
pub const CONTROL_COMPACT_HEIGHT: f32 = 24.0;
pub const FORM_ROW_HEIGHT: f32 = 34.0;
pub const PAGE_CHIP_WIDTH: f32 = 33.0;
pub const ICON_SIZE: f32 = 19.0;
pub const LIST_ROW_HEIGHT: f32 = 36.0;
pub const CARD_PADDING: i8 = 10;
pub const SECTION_PADDING: i8 = 12;
pub const FIELD_RADIUS: u8 = 4;
pub const CARD_RADIUS: u8 = 7;
pub const TRACK_RADIUS: u8 = 15;
pub const CHIP_RADIUS: u8 = 12;
pub const PREVIEW_RADIUS: u8 = 14;
pub const PREVIEW_SIZE: f32 = 78.0;
pub const PREVIEW_INSET: f32 = 7.0;
pub const PREVIEW_BORDER: f32 = 4.0;
pub const PALETTE_GROUP_HEIGHT: f32 = 50.0;
pub const PALETTE_ACTION_HEIGHT: f32 = 40.0;
pub const PALETTE_ICON_X: f32 = 45.0;
pub const PALETTE_TEXT_X: f32 = 66.0;
pub const TOOL_WINDOW_WIDTH: f32 = 552.0;
pub(super) const COMMAND_MENU_WIDTH: f32 = 176.0;
pub(super) const DIALOG_RADIUS: u8 = 10;
pub(super) const DIALOG_PADDING: i8 = 16;
pub(super) const DIALOG_GAP: Vec2 = vec2(GAP, 9.0);
pub(super) const DIALOG_WIDTH: f32 = 390.0;
pub const FOOTER_MARGIN: Margin = Margin::symmetric(20, 6);
pub const TOOLBAR_MARGIN: Margin = Margin::symmetric(20, 10);
pub const PALETTE_MARGIN: Margin = Margin {
    left: 17,
    right: 13,
    top: 12,
    bottom: 14,
};

pub(super) const BORDER_WIDTH: f32 = 1.0;
pub(super) const DIALOG_SHADOW: Shadow = Shadow {
    offset: [0, 7],
    blur: 18,
    spread: 0,
    color: Color32::from_black_alpha(90),
};
