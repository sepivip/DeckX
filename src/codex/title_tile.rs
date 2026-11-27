use std::path::Path;

use super::Status;
use crate::{
    asar::Archive,
    glyphs::{self, TitleStyle},
    theme,
};

pub(crate) fn style(status: Status, selected: bool) -> TitleStyle {
    let (background, dark_text) = match status {
        Status::Waiting => (theme::AGENT_TITLE, false),
        Status::Idle => ([218, 222, 231], true),
        Status::Working => ([47, 77, 226], false),
        Status::Approval | Status::Input => ([251, 130, 37], true),
        Status::Unread => ([63, 213, 119], true),
        Status::Error => ([194, 34, 60], false),
    };
    let text = if dark_text { [18, 20, 26] } else { [255; 3] };
    TitleStyle {
        font: glyphs::CODEX_TITLE,
        background,
        text,
        outline: selected.then_some(text),
    }
}

/// OpenAI Sans Medium from the installed Codex desktop app.
pub(crate) fn installed_font() -> Option<Vec<u8>> {
    super::find_in_desktop(|root| read_font(&root.join("app/resources/app.asar")))
}

fn read_font(path: &Path) -> Option<Vec<u8>> {
    const ASSETS: &str = "webview/assets";
    let mut archive = Archive::open(path).ok()?;
    let name = archive
        .files(ASSETS)?
        .find(|name| name.starts_with("OpenAISans-Medium.") && name.ends_with(".woff2"))?
        .to_owned();
    let bytes = archive.read(ASSETS, &name, 1024 * 1024).ok()?;
    woff2_patched::decode::convert_woff2_to_ttf(&mut bytes.as_slice()).ok()
}
