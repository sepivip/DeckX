use std::{
    os::windows::ffi::OsStrExt, path::Path, process::Command, ptr::null_mut, time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use url::Url;
use windows_sys::Win32::UI::{
    Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
        KEYEVENTF_UNICODE, SendInput,
    },
    Shell::ShellExecuteW,
    WindowsAndMessaging::SW_SHOWNORMAL,
};

#[derive(
    Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize, schemars::JsonSchema,
)]
pub enum ActionKind {
    #[default]
    None,
    Shortcut,
    OpenApp,
    OpenWebsite,
    CodexLaunch,
    CodexNewChat,
    CodexOpenChat,
    CodexDraft,
    VolumeUp,
    VolumeDown,
    Mute,
    PlayPause,
    PreviousTrack,
    NextTrack,
    TypeText,
    OpenFile,
    NextPage,
    PreviousPage,
    GoToPage,
    OpenFolder,
    Back,
    SwitchProfile,
    MultiAction,
    Delay,
    Plugin,
}

impl ActionKind {
    pub const DESKTOP: [Self; 4] = [
        Self::CodexLaunch,
        Self::CodexNewChat,
        Self::CodexOpenChat,
        Self::CodexDraft,
    ];
    pub const SYSTEM: [Self; 5] = [
        Self::OpenWebsite,
        Self::Shortcut,
        Self::OpenApp,
        Self::OpenFile,
        Self::TypeText,
    ];
    pub const MEDIA: [Self; 6] = [
        Self::VolumeUp,
        Self::VolumeDown,
        Self::Mute,
        Self::PlayPause,
        Self::PreviousTrack,
        Self::NextTrack,
    ];
    pub const NAVIGATION: [Self; 6] = [
        Self::NextPage,
        Self::PreviousPage,
        Self::GoToPage,
        Self::OpenFolder,
        Self::Back,
        Self::SwitchProfile,
    ];
    pub const AGENT_GROUPS: [(&'static str, &'static [Self]); 3] = [
        ("ChatGPT & Codex", &Self::DESKTOP),
        ("System", &Self::SYSTEM),
        ("Media", &Self::MEDIA),
    ];

    pub fn allowed_in_agent_mode(self) -> bool {
        Self::AGENT_GROUPS
            .iter()
            .any(|(_, kinds)| kinds.contains(&self))
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "No action",
            Self::Shortcut => "Keyboard shortcut",
            Self::OpenApp => "Open an app",
            Self::OpenWebsite => "Open a website",
            Self::CodexLaunch => "Open ChatGPT desktop",
            Self::CodexNewChat => "New desktop chat",
            Self::CodexOpenChat => "Open a saved chat",
            Self::CodexDraft => "Draft a Codex prompt",
            Self::VolumeUp => "Volume up",
            Self::VolumeDown => "Volume down",
            Self::Mute => "Toggle mute",
            Self::PlayPause => "Play / pause",
            Self::PreviousTrack => "Previous track",
            Self::NextTrack => "Next track",
            Self::TypeText => "Type text",
            Self::OpenFile => "Open a file or folder",
            Self::NextPage => "Next page",
            Self::PreviousPage => "Previous page",
            Self::GoToPage => "Go to page",
            Self::OpenFolder => "Open deck folder",
            Self::Back => "Go back",
            Self::SwitchProfile => "Switch profile",
            Self::MultiAction => "Multi action",
            Self::Delay => "Delay",
            Self::Plugin => "Plugin action",
        }
    }

    pub fn tile_label(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Shortcut => "HOTKEY",
            Self::TypeText => "TEXT",
            Self::OpenApp => "APP",
            Self::OpenFile => "FILE",
            Self::OpenWebsite => "WEB",
            Self::CodexLaunch => "CODEX",
            Self::CodexNewChat => "NEW CHAT",
            Self::CodexOpenChat => "CHAT",
            Self::CodexDraft => "DRAFT",
            Self::VolumeUp => "VOL +",
            Self::VolumeDown => "VOL -",
            Self::Mute => "MUTE",
            Self::PlayPause => "PLAY",
            Self::PreviousTrack => "PREV",
            Self::NextTrack => "NEXT",
            Self::NextPage => "PAGE >",
            Self::PreviousPage => "< PAGE",
            Self::GoToPage => "PAGE",
            Self::OpenFolder => "FOLDER",
            Self::Back => "BACK",
            Self::SwitchProfile => "PROFILE",
            Self::MultiAction => "MULTI",
            Self::Delay => "WAIT",
            Self::Plugin => "PLUGIN",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct Action {
    pub kind: ActionKind,
    #[serde(default)]
    pub value: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<Action>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_context: Option<String>,
}

impl Action {
    pub fn from_kind(kind: ActionKind) -> Self {
        Self {
            kind,
            value: match kind {
                ActionKind::Delay => "500".into(),
                ActionKind::CodexNewChat => "codex".into(),
                _ => String::new(),
            },
            steps: Vec::new(),
            plugin_context: None,
        }
    }

    pub fn plugin(uuid: String) -> Self {
        Self {
            kind: ActionKind::Plugin,
            value: uuid,
            plugin_context: Some(crate::plugins::token()),
            steps: Vec::new(),
        }
    }

    pub fn prepare(&self) -> Result<PreparedAction> {
        let value = self.value.trim();
        let key = match self.kind {
            ActionKind::None => return Ok(PreparedAction::None),
            ActionKind::Shortcut => return Ok(PreparedAction::Keys(parse_shortcut(value)?)),
            ActionKind::TypeText => {
                ensure!(!self.value.is_empty(), "Enter the text to type");
                ensure!(
                    self.value.encode_utf16().count() <= 4096,
                    "Text is limited to 4,096 UTF-16 units"
                );
                return Ok(PreparedAction::Text(self.value.encode_utf16().collect()));
            }
            ActionKind::OpenFile => {
                let path = Path::new(value);
                ensure!(
                    path.is_absolute() && path.exists(),
                    "Choose an existing file or folder"
                );
                return Ok(PreparedAction::File(path.to_owned()));
            }
            ActionKind::OpenApp => {
                let path = Path::new(value);
                ensure!(
                    path.is_absolute()
                        && path
                            .extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe")),
                    "Choose an absolute path to an .exe file"
                );
                ensure!(
                    path.is_file(),
                    "App file does not exist: {}",
                    path.display()
                );
                return Ok(PreparedAction::App(path.to_owned()));
            }
            ActionKind::OpenWebsite => {
                let url = Url::parse(value).context("Enter a complete website URL")?;
                ensure!(
                    matches!(url.scheme(), "http" | "https")
                        && url.has_host()
                        && url.username().is_empty()
                        && url.password().is_none(),
                    "Use an HTTP or HTTPS URL without credentials"
                );
                return Ok(PreparedAction::Website(url));
            }
            ActionKind::CodexLaunch => return Ok(PreparedAction::Codex("codex://launch".into())),
            ActionKind::CodexNewChat => {
                let mode = if value.is_empty() { "codex" } else { value };
                ensure!(
                    matches!(mode, "chat" | "work" | "codex"),
                    "Choose a chat mode"
                );
                return Ok(PreparedAction::Codex(new_chat_link(mode, None)?));
            }
            ActionKind::CodexOpenChat => {
                return Ok(PreparedAction::CodexAgent(codex_chat_id(value)?));
            }
            ActionKind::CodexDraft => {
                ensure!(!value.is_empty(), "Enter a prompt to draft");
                ensure!(value.len() <= 2048, "Draft prompts are limited to 2 KiB");
                return Ok(PreparedAction::Codex(new_chat_link("codex", Some(value))?));
            }
            ActionKind::VolumeUp => 0xaf,
            ActionKind::VolumeDown => 0xae,
            ActionKind::Mute => 0xad,
            ActionKind::PlayPause => 0xb3,
            ActionKind::PreviousTrack => 0xb1,
            ActionKind::NextTrack => 0xb0,
            ActionKind::NextPage => return Ok(PreparedAction::Navigate(Navigation::Next)),
            ActionKind::PreviousPage => return Ok(PreparedAction::Navigate(Navigation::Previous)),
            ActionKind::Back => return Ok(PreparedAction::Navigate(Navigation::Back)),
            ActionKind::GoToPage | ActionKind::OpenFolder | ActionKind::SwitchProfile => {
                let target = value.parse::<u64>().context("Choose a destination")?;
                let navigation = match self.kind {
                    ActionKind::SwitchProfile => Navigation::Profile(target),
                    ActionKind::OpenFolder => Navigation::Folder(target),
                    _ => Navigation::Page(target),
                };
                return Ok(PreparedAction::Navigate(navigation));
            }
            ActionKind::Delay => {
                let ms = value
                    .parse::<u64>()
                    .context("Enter the delay in milliseconds")?;
                ensure!(ms <= 60_000, "Each delay can be at most 60 seconds");
                return Ok(PreparedAction::Delay(Duration::from_millis(ms)));
            }
            ActionKind::Plugin => {
                let context = self
                    .plugin_context
                    .as_deref()
                    .context("Plugin binding is missing its context")?;
                ensure!(
                    context.len() == 32 && context.bytes().all(|byte| byte.is_ascii_hexdigit()),
                    "Invalid plugin binding context"
                );
                ensure!(!value.is_empty(), "Plugin binding has no action UUID");
                return Ok(PreparedAction::Plugin {
                    action: value.to_owned(),
                    context: context.to_owned(),
                });
            }
            ActionKind::MultiAction => {
                ensure!(
                    !self.steps.is_empty(),
                    "Add at least one step to the multi action"
                );
                ensure!(
                    self.steps.len() <= 32,
                    "A multi action can have at most 32 steps"
                );
                let steps = self
                    .steps
                    .iter()
                    .enumerate()
                    .map(|(index, step)| {
                        ensure!(
                            step.kind != ActionKind::MultiAction,
                            "Multi actions cannot be nested"
                        );
                        step.prepare()
                            .with_context(|| format!("Step {}", index + 1))
                    })
                    .collect::<Result<Vec<_>>>()?;
                return Ok(PreparedAction::Sequence(steps));
            }
        };
        Ok(PreparedAction::Keys(vec![key]))
    }
}

#[derive(Clone, Copy)]
pub enum Navigation {
    Next,
    Previous,
    Back,
    Page(u64),
    Folder(u64),
    Profile(u64),
}

#[derive(Clone)]
pub enum PreparedAction {
    None,
    Keys(Vec<u16>),
    App(std::path::PathBuf),
    Website(Url),
    Codex(String),
    CodexAgent(uuid::Uuid),
    Claude(crate::claude::Target),
    Text(Vec<u16>),
    File(std::path::PathBuf),
    Navigate(Navigation),
    Delay(Duration),
    Sequence(Vec<Self>),
    Plugin { action: String, context: String },
}

impl PreparedAction {
    pub fn run(&self) -> Result<()> {
        match self {
            Self::None => Ok(()),
            Self::Keys(keys) => send_shortcut(keys),
            Self::Text(text) => send_text(text),
            Self::File(path) => shell_open(path.as_os_str().encode_wide()),
            Self::App(path) => {
                let mut command = Command::new(path);
                if let Some(parent) = path.parent() {
                    command.current_dir(parent);
                }
                command
                    .spawn()
                    .with_context(|| format!("Could not open {}", path.display()))?;
                Ok(())
            }
            Self::Website(url) => shell_open(url.as_str().encode_utf16()),
            Self::Codex(link) => shell_open(link.encode_utf16())
                .context("Could not open Codex. Check that the ChatGPT desktop app is installed"),
            Self::CodexAgent(id) => Self::Codex(format!("codex://threads/{id}")).run(),
            Self::Claude(target) => shell_open(target.link().encode_utf16())
                .context("Could not open Claude Desktop. Check that Claude Desktop is installed"),
            Self::Navigate(_) | Self::Delay(_) | Self::Sequence(_) | Self::Plugin { .. } => {
                bail!("This action requires a deck session")
            }
        }
    }
}

pub(crate) fn codex_chat_id(value: &str) -> Result<uuid::Uuid> {
    let id = value.strip_prefix("codex://threads/").unwrap_or(value);
    uuid::Uuid::parse_str(id).context("Paste a Codex chat link or enter its chat ID")
}

fn new_chat_link(mode: &str, draft: Option<&str>) -> Result<String> {
    let mut link = Url::parse("codex://threads/new")?;
    link.query_pairs_mut().append_pair("mode", mode);
    if let Some(draft) = draft {
        link.query_pairs_mut().append_pair("prompt", draft);
    }
    Ok(link.into())
}

pub fn open_http(raw: &str) -> Result<()> {
    let url = Url::parse(raw).context("Invalid web address")?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.has_host()
            && url.username().is_empty()
            && url.password().is_none(),
        "Use an HTTP or HTTPS address"
    );
    shell_open(url.as_str().encode_utf16())
}

pub(crate) fn open_grok_bot() -> Result<()> {
    shell_open("grokbot://".encode_utf16())
        .context("Could not open Grok Bot. Check that Grok Bot is installed")
}

pub(crate) fn open_grok_agent(id: &str) -> Result<()> {
    let link = grok_agent_link(id)?;
    shell_open(link.encode_utf16()).context("Could not open the bot in Grok Bot")
}

pub(crate) fn grok_agent_link(id: &str) -> Result<String> {
    ensure!(
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte)),
        "Grok Bot returned an invalid bot ID"
    );
    Ok(format!("grokbot://app/v1/agent?id={id}"))
}

fn shell_open(path: impl Iterator<Item = u16>) -> Result<()> {
    let wide: Vec<u16> = path.chain([0]).collect();
    // The arguments are null or point to a terminated UTF-16 string for this call.
    let result = unsafe {
        ShellExecuteW(
            null_mut(),
            std::ptr::null(),
            wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    } as isize;
    ensure!(
        result > 32,
        "Windows could not open the target (error {result})"
    );
    Ok(())
}

fn send_text(text: &[u16]) -> Result<()> {
    let mut events = Vec::with_capacity(text.len() * 2);
    for &unit in text {
        for up in [false, true] {
            if unit == b'\n' as u16 {
                events.push(key_event(0x0d, up));
            } else if unit != b'\r' as u16 {
                events.push(INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wScan: unit,
                            dwFlags: KEYEVENTF_UNICODE | if up { KEYEVENTF_KEYUP } else { 0 },
                            ..Default::default()
                        },
                    },
                });
            }
        }
    }
    let size = std::mem::size_of::<INPUT>() as i32;
    let sent = unsafe { SendInput(events.len() as u32, events.as_ptr(), size) } as usize;
    ensure!(
        sent == events.len(),
        "Windows could not deliver the text to the foreground app"
    );
    Ok(())
}

fn parse_shortcut(value: &str) -> Result<Vec<u16>> {
    let mut keys = Vec::new();
    let mut parts = value.split('+').map(str::trim).peekable();
    while let Some(part) = parts.next() {
        let upper = part.to_ascii_uppercase();
        let modifier = match upper.as_str() {
            "CTRL" | "CONTROL" => Some(0x11),
            "ALT" => Some(0x12),
            "SHIFT" => Some(0x10),
            "WIN" => Some(0x5b),
            _ => None,
        };
        if parts.peek().is_some() {
            let key = modifier.with_context(|| format!("Unknown shortcut modifier: {part}"))?;
            ensure!(!keys.contains(&key), "Duplicate modifier: {part}");
            keys.push(key);
        } else {
            keys.push(
                main_key(&upper).with_context(|| format!("Unsupported shortcut key: {part:?}"))?,
            );
        }
    }
    Ok(keys)
}

fn main_key(name: &str) -> Option<u16> {
    if let [byte @ (b'A'..=b'Z' | b'0'..=b'9')] = name.as_bytes() {
        return Some(u16::from(*byte));
    }
    if let Some(function) = name.strip_prefix('F').and_then(|n| n.parse::<u16>().ok())
        && (1..=24).contains(&function)
    {
        return Some(0x6f + function);
    }
    Some(match name {
        "TAB" => 0x09,
        "ENTER" => 0x0d,
        "ESC" | "ESCAPE" => 0x1b,
        "SPACE" => 0x20,
        "PAGEUP" => 0x21,
        "PAGEDOWN" => 0x22,
        "END" => 0x23,
        "HOME" => 0x24,
        "LEFT" => 0x25,
        "UP" => 0x26,
        "RIGHT" => 0x27,
        "DOWN" => 0x28,
        "INSERT" => 0x2d,
        "DELETE" => 0x2e,
        "BACKSPACE" => 0x08,
        _ => return None,
    })
}

fn key_event(key: u16, up: bool) -> INPUT {
    let mut flags = if up { KEYEVENTF_KEYUP } else { 0 };
    if matches!(key, 0x21..=0x28 | 0x2d..=0x2e | 0x5b | 0xad..=0xb3) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                dwFlags: flags,
                ..Default::default()
            },
        },
    }
}

fn send_shortcut(keys: &[u16]) -> Result<()> {
    let events: Vec<_> = keys
        .iter()
        .map(|&key| key_event(key, false))
        .chain(keys.iter().rev().map(|&key| key_event(key, true)))
        .collect();
    let size = std::mem::size_of::<INPUT>() as i32;
    // The slice supplies exactly the specified number of initialized INPUT records.
    let sent = unsafe { SendInput(events.len() as u32, events.as_ptr(), size) } as usize;
    if sent != events.len() {
        if sent > 0 {
            let still_down = sent.min(events.len() - sent);
            let release: Vec<_> = keys[..still_down]
                .iter()
                .rev()
                .map(|&key| key_event(key, true))
                .collect();
            unsafe { SendInput(release.len() as u32, release.as_ptr(), size) };
        }
        bail!("Windows did not deliver the shortcut. Check whether the target app is elevated");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_draft_cannot_add_deep_link_parameters() -> Result<()> {
        let mut action = Action::from_kind(ActionKind::CodexDraft);
        action.value = "Review this change &mode=work".into();
        let PreparedAction::Codex(link) = action.prepare()? else {
            bail!("Expected a Codex desktop link");
        };
        let link = Url::parse(&link)?;
        assert_eq!(link.host_str(), Some("threads"));
        assert_eq!(link.path(), "/new");
        assert_eq!(
            link.query_pairs().collect::<Vec<_>>(),
            [
                ("mode".into(), "codex".into()),
                ("prompt".into(), action.value.into())
            ]
        );
        Ok(())
    }
}
