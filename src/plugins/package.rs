use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

const MAX_FILES: usize = 18_000;
pub(crate) const MAX_ARCHIVE: u64 = 200 * 1024 * 1024;
const MAX_EXTRACTED: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Action {
    #[serde(rename = "UUID")]
    pub uuid: String,
    pub name: String,
    #[serde(default)]
    pub tooltip: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub states: Vec<ActionState>,
    #[serde(default)]
    pub controllers: Vec<String>,
    pub property_inspector_path: Option<String>,
    #[serde(default = "visible")]
    pub visible_in_actions_list: bool,
    #[serde(default)]
    pub disable_automatic_states: bool,
    /// Resolved once by `Plugin::read`.
    #[serde(skip)]
    pub inspector: Option<PathBuf>,
}

fn visible() -> bool {
    true
}

impl Action {
    pub fn accepts_key(&self) -> bool {
        self.controllers.is_empty() || self.controllers.iter().any(|c| c == "Keypad")
    }

    pub fn accepts_dial(&self) -> bool {
        self.controllers.iter().any(|c| c == "Encoder")
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ActionState {
    #[serde(default)]
    pub image: String,
    #[serde(default)]
    pub title: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Manifest {
    #[serde(rename = "UUID", default)]
    uuid: String,
    pub name: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub category: String,
    pub code_path: Option<String>,
    pub code_path_win: Option<String>,
    pub property_inspector_path: Option<String>,
    #[serde(rename = "OS", default)]
    os: Vec<Platform>,
    #[serde(rename = "SDKVersion", default)]
    sdk_version: u8,
    pub nodejs: Option<NodeRuntime>,
    pub actions: Vec<Action>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Platform {
    platform: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct NodeRuntime {
    pub version: serde_json::Value,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backend {
    Node,
    Windows,
    Browser,
}

#[derive(Clone, Debug)]
pub struct Plugin {
    pub id: String,
    pub root: PathBuf,
    pub entry: PathBuf,
    pub backend: Backend,
    pub manifest: Manifest,
    actions: HashMap<String, usize>,
}

impl Plugin {
    pub fn read(root: &Path, fallback_id: Option<&str>) -> Result<Self> {
        let root = fs::canonicalize(root).context("Could not open the plugin folder")?;
        let manifest_path = local_file(&root, "manifest.json").context("Missing manifest.json")?;
        let mut raw = Vec::new();
        File::open(&manifest_path)?
            .take(1_048_577)
            .read_to_end(&mut raw)?;
        ensure!(raw.len() <= 1_048_576, "Plugin manifest exceeds 1 MiB");
        let mut manifest: Manifest =
            serde_json::from_slice(raw.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&raw))
                .context("Invalid plugin manifest")?;
        let id = if manifest.uuid.is_empty() {
            root.file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".sdPlugin"))
                .or(fallback_id)
                .context("Plugin UUID is missing")?
                .to_owned()
        } else {
            manifest.uuid.clone()
        };
        ensure!(identifier(&id), "Invalid plugin UUID");
        ensure!(
            manifest.os.is_empty()
                || manifest
                    .os
                    .iter()
                    .any(|p| p.platform.eq_ignore_ascii_case("windows")),
            "This plugin has no Windows backend"
        );
        ensure!(
            manifest.sdk_version <= 3,
            "Unsupported Stream Deck SDK version {}",
            manifest.sdk_version
        );
        ensure!(
            !manifest.actions.is_empty() && manifest.actions.len() <= 512,
            "Invalid number of plugin actions"
        );
        let mut actions = HashMap::new();
        for (index, action) in manifest.actions.iter().enumerate() {
            ensure!(
                identifier(&action.uuid) && actions.insert(action.uuid.clone(), index).is_none(),
                "Invalid or duplicate action UUID"
            );
        }
        for action in &mut manifest.actions {
            action.inspector = action
                .property_inspector_path
                .as_deref()
                .or(manifest.property_inspector_path.as_deref())
                .and_then(|entry| local_file(&root, entry))
                .filter(|path| {
                    path.extension().is_some_and(|ext| {
                        ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm")
                    })
                });
        }
        let entry = manifest
            .code_path_win
            .as_deref()
            .or(manifest.code_path.as_deref())
            .filter(|path| !path.is_empty())
            .context("Plugin does not provide a standalone entry point")?;
        let entry = local_file(&root, entry)
            .or_else(|| local_file(&root, &format!("{entry}.exe")))
            .context("Plugin entry point is missing or outside its folder")?;
        let extension = entry
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let backend = match extension.as_str() {
            "js" | "mjs" | "cjs" => Backend::Node,
            "exe" => Backend::Windows,
            "html" | "htm" => Backend::Browser,
            _ => bail!("Unsupported plugin entry point type: {extension}"),
        };
        Ok(Self {
            id,
            root,
            entry,
            backend,
            manifest,
            actions,
        })
    }

    pub fn action(&self, uuid: &str) -> Option<&Action> {
        self.actions
            .get(uuid)
            .and_then(|&index| self.manifest.actions.get(index))
    }
}

/// Plugin and action UUIDs, which DeckX also uses as file names.
pub(super) fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 240
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        && windows_name(value.as_ref())
}

/// Accepts only plain relative names. Joining an absolute, UNC, verbatim or drive-relative
/// path replaces the plugin root, so this runs before any filesystem call.
fn relative(name: &str) -> Option<&Path> {
    let path = Path::new(name);
    let mut parts = 0;
    for part in path.components() {
        match part {
            Component::Normal(part) if windows_name(part) => parts += 1,
            Component::CurDir => {}
            _ => return None,
        }
    }
    (parts > 0 && !name.contains(':')).then_some(path)
}

pub fn local_file(root: &Path, name: &str) -> Option<PathBuf> {
    let candidate = fs::canonicalize(root.join(relative(name)?)).ok()?;
    (candidate.starts_with(root) && candidate.is_file()).then_some(candidate)
}

pub fn image_path(root: &Path, name: &str) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    local_file(root, name).or_else(|| {
        ["@2x.png", ".png", ".svg", ".gif", ".jpg", ".webp"]
            .iter()
            .find_map(|ext| local_file(root, &format!("{name}{ext}")))
    })
}

/// Copies an installed .sdPlugin folder with the limits used for package extraction.
pub fn copy(source: &Path, destination: &Path) -> Result<()> {
    let mut files = 0;
    let mut remaining = MAX_EXTRACTED;
    let mut folders = vec![(source.to_owned(), destination.to_owned())];
    while let Some((from, to)) = folders.pop() {
        fs::create_dir(&to)?;
        for entry in fs::read_dir(&from)? {
            let entry = entry?;
            files += 1;
            ensure!(files <= MAX_FILES, "Plugin folder contains too many files");
            let kind = entry.file_type()?;
            // Windows reports junctions as symbolic links too.
            ensure!(
                !kind.is_symlink(),
                "Plugin folders cannot contain symbolic links"
            );
            let target = to.join(entry.file_name());
            if kind.is_dir() {
                folders.push((entry.path(), target));
                continue;
            }
            let bytes = entry.metadata()?.len();
            ensure!(bytes <= remaining, "Plugin folder exceeds 512 MiB");
            let mut out = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(target)?;
            let copied = io::copy(&mut File::open(entry.path())?.take(bytes + 1), &mut out)?;
            ensure!(copied == bytes, "A plugin file changed while it was copied");
            remaining -= bytes;
        }
    }
    Ok(())
}

pub fn extract(source: &Path, destination: &Path) -> Result<PathBuf> {
    let file = File::open(source).context("Could not open the plugin package")?;
    ensure!(
        file.metadata()?.len() <= MAX_ARCHIVE,
        "Plugin package is larger than 200 MiB"
    );
    let mut archive = zip::ZipArchive::new(file).context("Invalid .streamDeckPlugin archive")?;
    ensure!(
        archive.len() <= MAX_FILES,
        "Plugin package contains too many entries"
    );
    let mut remaining = MAX_EXTRACTED;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let enclosed = entry
            .enclosed_name()
            .context("Plugin archive contains an unsafe path")?;
        ensure!(
            enclosed
                .components()
                .all(|part| matches!(part, Component::Normal(name) if windows_name(name))),
            "Plugin archive contains an unsafe Windows path"
        );
        ensure!(
            !entry.is_symlink(),
            "Plugin archives cannot contain symbolic links"
        );
        let path = destination.join(enclosed);
        if entry.is_dir() {
            fs::create_dir_all(path)?;
            continue;
        }
        let bytes = entry.size();
        ensure!(
            bytes <= remaining,
            "Expanded plugin package exceeds 512 MiB"
        );
        fs::create_dir_all(path.parent().context("Invalid plugin path")?)?;
        let mut out = OpenOptions::new().write(true).create_new(true).open(path)?;
        let actual = io::copy(&mut (&mut entry).take(bytes + 1), &mut out)?;
        ensure!(actual == bytes, "Unexpected expanded plugin size");
        remaining -= bytes;
    }
    if destination.join("manifest.json").is_file() {
        return Ok(destination.to_owned());
    }
    let mut roots = fs::read_dir(destination)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("sdplugin"))
                && path.join("manifest.json").is_file()
        });
    let root = roots
        .next()
        .context("Package does not contain a .sdPlugin with a manifest")?;
    ensure!(roots.next().is_none(), "Import one plugin per archive");
    Ok(root)
}

fn windows_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    !name.ends_with(['.', ' '])
        && !name.contains([':', '*', '?', '"', '<', '>', '|', '\\'])
        && !matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        )
        && !["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|n| {
                matches!(
                    n,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        })
}
