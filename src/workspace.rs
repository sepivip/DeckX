use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    actions::{Action, ActionKind},
    agent_layout::MainDial,
    profile::{self, Layout, PreparedLayout},
};

const VERSION: u32 = 2;
pub const MAX_PROFILES: usize = 12;
pub const MAX_PAGES: usize = 24;
/// The longest profile or page name.
pub const NAME_CHARS: usize = 40;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, schemars::JsonSchema)]
pub struct Scene {
    pub profile: u64,
    pub page: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Page {
    pub id: u64,
    pub name: String,
    pub folder: bool,
    pub layout: Layout,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Profile {
    pub id: u64,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<PathBuf>,
    pub pages: Vec<Page>,
}

impl Profile {
    pub fn first_page(&self) -> Scene {
        Scene {
            profile: self.id,
            page: self
                .pages
                .iter()
                .find(|page| !page.folder)
                .expect("profile has a page")
                .id,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Workspace {
    version: u32,
    pub brightness: u8,
    pub rotation: u16,
    #[serde(default, skip_serializing_if = "MainDial::uses_assignments")]
    pub(crate) agent_main_dial: MainDial,
    #[serde(default)]
    pub(crate) easter_egg: bool,
    #[serde(default)]
    pub(crate) minimize_to_tray: bool,
    pub home: Scene,
    pub profiles: Vec<Profile>,
}

#[derive(Deserialize)]
struct LegacyProfile {
    brightness: u8,
    rotation: u16,
    #[serde(flatten)]
    layout: Layout,
}

impl Default for Workspace {
    fn default() -> Self {
        Self::from_legacy(LegacyProfile {
            brightness: 65,
            rotation: 90,
            layout: Layout::with_media_defaults(),
        })
    }
}

impl Workspace {
    fn from_legacy(legacy: LegacyProfile) -> Self {
        Self {
            version: VERSION,
            brightness: legacy.brightness,
            rotation: legacy.rotation,
            agent_main_dial: MainDial::default(),
            easter_egg: false,
            minimize_to_tray: false,
            home: Scene {
                profile: 1,
                page: 2,
            },
            profiles: vec![Profile {
                id: 1,
                name: "Default".into(),
                app: None,
                pages: vec![Page {
                    id: 2,
                    name: "Page 1".into(),
                    folder: false,
                    layout: legacy.layout,
                }],
            }],
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error).context("Could not open the DeckX profile"),
        };
        let json: serde_json::Value =
            serde_json::from_reader(BufReader::new(file)).context("Invalid DeckX profile")?;
        let workspace = if json.get("version").is_some() {
            serde_json::from_value(json).context("Invalid DeckX workspace")?
        } else {
            Self::from_legacy(serde_json::from_value(json).context("Invalid DeckX 0.1 profile")?)
        };
        workspace.validate_structure()?;
        Ok(workspace)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        profile::save_json(path, self).context("Could not save the DeckX workspace")
    }

    /// Saves over the live workspace file and keeps the previous version as `.bak`.
    pub fn replace(&self, path: &Path) -> Result<()> {
        match std::fs::copy(path, append_extension(path, "bak")) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).context("Could not back up the previous DeckX workspace");
            }
        }
        self.save(path)
    }

    /// Renames a workspace file DeckX could not read, so a later save cannot replace it.
    pub fn set_aside(path: &Path) -> Result<PathBuf> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let target = append_extension(path, &format!("unreadable-{stamp}"));
        std::fs::rename(path, &target)
            .context("Could not keep a copy of the unreadable workspace")?;
        Ok(target)
    }

    pub fn profile(&self, id: u64) -> Option<&Profile> {
        self.profiles.iter().find(|profile| profile.id == id)
    }

    pub fn page(&self, scene: Scene) -> Option<&Page> {
        self.profile(scene.profile)?
            .pages
            .iter()
            .find(|page| page.id == scene.page)
    }

    pub fn profile_mut(&mut self, id: u64) -> Option<&mut Profile> {
        self.profiles.iter_mut().find(|profile| profile.id == id)
    }

    pub fn page_mut(&mut self, scene: Scene) -> Option<&mut Page> {
        self.profile_mut(scene.profile)?
            .pages
            .iter_mut()
            .find(|page| page.id == scene.page)
    }

    pub fn add_profile(&mut self, name: String) -> Result<Scene> {
        ensure!(
            self.profiles.len() < MAX_PROFILES,
            "A workspace can hold {MAX_PROFILES} profiles"
        );
        let id = self.next_id()?;
        let page = id.checked_add(1).context("Profile identifier exhausted")?;
        self.profiles.push(Profile {
            id,
            name,
            app: None,
            pages: vec![Page {
                id: page,
                name: "Page 1".into(),
                folder: false,
                layout: Layout::with_media_defaults(),
            }],
        });
        Ok(Scene { profile: id, page })
    }

    pub fn add_page(&mut self, profile: u64, name: String, folder: bool) -> Result<Scene> {
        let id = self.next_id()?;
        let profile = self.profile_mut(profile).context("Profile is missing")?;
        ensure!(
            profile.pages.len() < MAX_PAGES,
            "A profile can hold {MAX_PAGES} pages and folders"
        );
        let mut layout = Layout::default();
        if folder {
            layout.buttons[0].action = Action::from_kind(ActionKind::Back);
        }
        profile.pages.push(Page {
            id,
            name,
            folder,
            layout,
        });
        Ok(Scene {
            profile: profile.id,
            page: id,
        })
    }

    pub fn remove_page(&mut self, scene: Scene) -> Result<Scene> {
        let profile = self.profile(scene.profile).context("Profile is missing")?;
        let page = self.page(scene).context("Page is missing")?;
        ensure!(
            page.folder || profile.pages.iter().filter(|p| !p.folder).count() > 1,
            "Keep at least one page in each profile"
        );
        ensure!(
            !profile
                .pages
                .iter()
                .filter(|p| p.id != scene.page)
                .flat_map(|p| page_actions(&p.layout))
                .any(|action| references(
                    action,
                    &[ActionKind::OpenFolder, ActionKind::GoToPage],
                    scene.page
                )),
            "Remove bindings that open this page or folder first"
        );
        let profile = self
            .profile_mut(scene.profile)
            .context("Profile is missing")?;
        profile.pages.retain(|p| p.id != scene.page);
        let next = profile.first_page();
        if self.home == scene {
            self.home = next;
        }
        Ok(next)
    }

    pub fn remove_profile(&mut self, id: u64) -> Result<Scene> {
        ensure!(self.profiles.len() > 1, "Keep at least one profile");
        ensure!(
            !self
                .profiles
                .iter()
                .filter(|p| p.id != id)
                .flat_map(|p| &p.pages)
                .flat_map(|p| page_actions(&p.layout))
                .any(|action| references(action, &[ActionKind::SwitchProfile], id)),
            "Remove bindings that switch to this profile first"
        );
        self.profiles.retain(|profile| profile.id != id);
        let next = self.profiles[0].first_page();
        if self.home.profile == id {
            self.home = next;
        }
        Ok(next)
    }

    fn next_id(&self) -> Result<u64> {
        self.profiles
            .iter()
            .flat_map(|profile| {
                std::iter::once(profile.id).chain(profile.pages.iter().map(|p| p.id))
            })
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .context("Workspace identifier exhausted")
    }

    fn validate_structure(&self) -> Result<()> {
        ensure!(
            self.version == VERSION,
            "Unsupported workspace version {}",
            self.version
        );
        ensure!(
            self.brightness <= 100,
            "Brightness must be between 0 and 100"
        );
        ensure!(
            matches!(self.rotation, 0 | 90 | 180 | 270),
            "Invalid key image rotation"
        );
        ensure!(
            (1..=MAX_PROFILES).contains(&self.profiles.len()),
            "Invalid profile count"
        );
        let mut ids = HashSet::new();
        for profile in &self.profiles {
            ensure!(ids.insert(profile.id), "Duplicate profile ID");
            ensure!(
                profile.pages.len() <= MAX_PAGES && profile.pages.iter().any(|p| !p.folder),
                "Invalid pages in {}",
                profile.name
            );
            for page in &profile.pages {
                ensure!(ids.insert(page.id), "Duplicate page ID");
            }
        }
        ensure!(
            self.page(self.home).is_some(),
            "The startup page is missing"
        );
        Ok(())
    }

    pub fn prepare(&self) -> Result<PreparedWorkspace> {
        self.validate_structure()?;
        let mut pages = HashMap::new();
        let mut top_level = HashMap::new();
        let mut app_profiles = HashMap::new();
        for profile in &self.profiles {
            if let Some(app) = &profile.app {
                ensure!(
                    app.is_absolute()
                        && app
                            .extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe")),
                    "{} needs an absolute .exe path for its app association",
                    profile.name
                );
                let key = crate::foreground::app_key(app);
                ensure!(
                    app_profiles.insert(key, profile.first_page()).is_none(),
                    "Assign each app to only one profile"
                );
            }
            let mut order = Vec::new();
            for page in &profile.pages {
                let scene = Scene {
                    profile: profile.id,
                    page: page.id,
                };
                if !page.folder {
                    order.push(scene);
                }
                let prepare = || {
                    for action in page_actions(&page.layout) {
                        self.validate_targets(profile, action, false)?;
                    }
                    page.layout.prepare(self.rotation)
                };
                pages.insert(
                    scene,
                    prepare().with_context(|| format!("{} / {}", profile.name, page.name))?,
                );
            }
            top_level.insert(profile.id, order);
        }
        Ok(PreparedWorkspace {
            brightness: self.brightness,
            rotation: self.rotation,
            agent_main_dial: self.agent_main_dial,
            easter_egg: self.easter_egg,
            home: self.home,
            pages,
            top_level,
            app_profiles,
        })
    }

    fn validate_targets(&self, profile: &Profile, action: &Action, step: bool) -> Result<()> {
        use ActionKind::*;
        match action.kind {
            GoToPage | OpenFolder => {
                let id = action
                    .value
                    .trim()
                    .parse::<u64>()
                    .context("Choose a page or folder")?;
                ensure!(
                    profile
                        .pages
                        .iter()
                        .any(|p| p.id == id && p.folder == (action.kind == OpenFolder)),
                    "The destination page or folder is missing"
                );
            }
            SwitchProfile => {
                let id = action
                    .value
                    .trim()
                    .parse::<u64>()
                    .context("Choose a profile")?;
                ensure!(
                    self.profile(id).is_some(),
                    "The destination profile is missing"
                );
            }
            MultiAction => {
                ensure!(!step, "Multi actions cannot be nested");
                for action in &action.steps {
                    self.validate_targets(profile, action, true)?;
                    ensure!(
                        !ActionKind::NAVIGATION.contains(&action.kind),
                        "Add navigation as a separate binding"
                    );
                }
            }
            Delay => ensure!(step, "Delays belong inside a multi action"),
            Plugin => ensure!(!step, "Place plugin actions directly on a key or dial"),
            _ => {}
        }
        Ok(())
    }
}

fn page_actions(layout: &Layout) -> impl Iterator<Item = &Action> {
    layout
        .buttons
        .iter()
        .map(|button| &button.action)
        .chain(layout.dials.iter().flatten())
}

fn references(action: &Action, kinds: &[ActionKind], id: u64) -> bool {
    // Matches Action::prepare, which trims values before parsing them.
    (kinds.contains(&action.kind) && action.value.trim().parse::<u64>() == Ok(id))
        || action.steps.iter().any(|step| references(step, kinds, id))
}

fn append_extension(path: &Path, extension: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".");
    name.push(extension);
    name.into()
}

pub struct PreparedWorkspace {
    pub brightness: u8,
    pub rotation: u16,
    pub(crate) agent_main_dial: MainDial,
    pub(crate) easter_egg: bool,
    pub home: Scene,
    pub pages: HashMap<Scene, PreparedLayout>,
    pub top_level: HashMap<u64, Vec<Scene>>,
    pub app_profiles: HashMap<String, Scene>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacing_keeps_the_previous_workspace_as_a_backup() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("profile.json");
        let first = Workspace {
            brightness: 10,
            ..Workspace::default()
        };
        first.replace(&path).unwrap();
        assert!(!folder.path().join("profile.json.bak").exists());

        let mut second = first.clone();
        second.brightness = 20;
        second.replace(&path).unwrap();
        let backup = Workspace::load(&folder.path().join("profile.json.bak")).unwrap();
        assert_eq!(backup.brightness, 10);
        assert_eq!(Workspace::load(&path).unwrap().brightness, 20);
    }

    #[test]
    fn unreadable_workspace_is_set_aside_intact() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("profile.json");
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(Workspace::load(&path).is_err());

        let kept = Workspace::set_aside(&path).unwrap();
        assert!(!path.exists());
        assert_eq!(std::fs::read(&kept).unwrap(), b"{ not json");
        let name = kept.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("profile.json.unreadable-"), "{name}");
    }

    #[test]
    fn missing_workspace_loads_the_default() {
        let folder = tempfile::tempdir().unwrap();
        let loaded = Workspace::load(&folder.path().join("profile.json")).unwrap();
        assert_eq!(loaded, Workspace::default());
    }
}
