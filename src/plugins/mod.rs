mod package;
mod process;
pub mod server;
#[cfg(test)]
mod tests;
mod visual;

use std::{
    collections::HashMap,
    fs::{self, File},
    io::BufReader,
    os::windows::process::CommandExt,
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub(crate) use package::MAX_ARCHIVE;
use package::{Backend, Plugin};
use process::{CREATE_NO_WINDOW, Job};
pub use server::{Instance, Shared};
use server::{Server, lock};
pub use visual::render;

#[derive(Default, Deserialize, Serialize)]
struct Registry {
    plugins: Vec<Registration>,
    #[serde(default)]
    node: Option<PathBuf>,
}

#[derive(Clone, Deserialize, Serialize)]
struct Registration {
    folder: PathBuf,
    id: String,
    enabled: bool,
    /// Where the package came from, shown before the plugin is enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<String>,
}

/// A package extracted or copied into the managed Packages folder, ready to register.
pub struct Staged {
    directory: tempfile::TempDir,
    plugin: Plugin,
    source: String,
}

impl Staged {
    pub fn name(&self) -> &str {
        &self.plugin.manifest.name
    }

    pub fn author(&self) -> &str {
        &self.plugin.manifest.author
    }
}

/// Extracts a .streamDeckPlugin, or copies an .sdPlugin folder, into `packages`. This does no
/// registry work, so it can run on a worker thread.
pub fn stage(packages: &Path, path: &Path, source: String) -> Result<Staged> {
    fs::create_dir_all(packages)?;
    let directory = tempfile::Builder::new()
        .prefix("plugin-")
        .tempdir_in(packages)?;
    let (root, fallback) = if path.is_dir() {
        // Check the folder before copying it. DeckX runs the copy, so later changes to the
        // folder do not change what it starts.
        Plugin::read(path, None)?;
        let copy = directory
            .path()
            .join(path.file_name().context("Choose a plugin folder")?);
        package::copy(path, &copy)?;
        (copy, None)
    } else {
        let root = package::extract(path, directory.path())?;
        (root, path.file_stem().and_then(|stem| stem.to_str()))
    };
    let plugin = Plugin::read(&root, fallback)?;
    Ok(Staged {
        directory,
        plugin,
        source,
    })
}

struct Running {
    child: Child,
    job: Job,
    launched: Instant,
}

#[derive(Debug)]
pub struct Installed {
    pub id: String,
    pub name: String,
    pub detail: String,
    pub enabled: bool,
    pub status: String,
    pub source: Option<String>,
}

#[derive(Clone)]
pub struct CatalogItem {
    pub uuid: String,
    pub name: String,
    pub category: String,
    pub tooltip: String,
    pub key: bool,
    pub dial: bool,
    pub inspector: bool,
}

pub struct PluginHost {
    root: PathBuf,
    registry: Registry,
    plugins: HashMap<String, Plugin>,
    /// Action UUID to plugin UUID.
    actions: HashMap<String, String>,
    /// Visible actions, rebuilt only when plugins change.
    catalog: Vec<CatalogItem>,
    /// Action UUID to its position in `catalog`.
    listed: HashMap<String, usize>,
    running: HashMap<String, Running>,
    errors: HashMap<String, String>,
    server: Server,
}

impl PluginHost {
    pub fn new() -> Result<Self> {
        let root = crate::profile::path()?
            .parent()
            .context("Missing DeckX folder")?
            .join("PluginHost");
        Self::at(root)
    }

    pub(crate) fn at(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root)?;
        let registry: Registry = load_json(&root.join("plugins.json"))?;
        let server = Server::start(root.join("settings.json"))?;
        let mut host = Self {
            root,
            registry,
            plugins: HashMap::new(),
            actions: HashMap::new(),
            catalog: Vec::new(),
            listed: HashMap::new(),
            running: HashMap::new(),
            errors: HashMap::new(),
            server,
        };
        for item in host.registry.plugins.clone() {
            match Plugin::read(&item.folder, Some(&item.id)).and_then(|plugin| {
                ensure!(plugin.id == item.id, "The plugin UUID changed");
                host.validate_unique(&plugin, false)?;
                host.insert(plugin);
                if item.enabled {
                    host.launch(&item.id)?;
                }
                Ok(())
            }) {
                Ok(()) => {}
                Err(error) => {
                    host.errors.insert(item.id, format!("{error:#}"));
                }
            }
        }
        host.reindex();
        Ok(host)
    }

    pub fn hub(&self) -> Shared {
        self.server.hub.clone()
    }

    /// The folder that holds extracted and copied packages.
    pub fn packages(&self) -> PathBuf {
        self.root.join("Packages")
    }

    pub fn import(&mut self, path: &Path) -> Result<String> {
        let staged = stage(&self.packages(), path, path.display().to_string())?;
        self.install(staged)
    }

    /// Registers a staged package. A package with an installed UUID replaces it as an update;
    /// the update stays disabled until it is approved again.
    pub fn install(&mut self, staged: Staged) -> Result<String> {
        let Staged {
            directory,
            plugin,
            source,
        } = staged;
        let previous = self
            .registry
            .plugins
            .iter()
            .position(|entry| entry.id == plugin.id);
        ensure!(
            previous.is_some() || self.registry.plugins.len() < 64,
            "DeckX can hold 64 plugins"
        );
        self.validate_unique(&plugin, previous.is_some())?;
        let registration = Registration {
            folder: plugin.root.clone(),
            id: plugin.id.clone(),
            enabled: false,
            source: Some(source),
        };
        let replaced = match previous {
            Some(index) => Some(std::mem::replace(
                &mut self.registry.plugins[index],
                registration,
            )),
            None => {
                self.registry.plugins.push(registration);
                None
            }
        };
        if let Err(error) = self.save() {
            match (previous, replaced) {
                (Some(index), Some(old)) => self.registry.plugins[index] = old,
                _ => {
                    self.registry.plugins.pop();
                }
            }
            return Err(error);
        }
        let _ = directory.keep();
        if let Some(old) = replaced {
            self.stop(&old.id);
            self.remove(&old.id);
            self.errors.remove(&old.id);
            self.delete_package(&old.folder);
        }
        let name = plugin.manifest.name.clone();
        self.insert(plugin);
        self.reindex();
        Ok(name)
    }

    /// Stops a plugin and removes its registration, settings and DeckX-managed files.
    pub fn uninstall(&mut self, id: &str) -> Result<String> {
        let index = self
            .registry
            .plugins
            .iter()
            .position(|entry| entry.id == id)
            .context("Plugin is missing")?;
        let entry = self.registry.plugins.remove(index);
        if let Err(error) = self.save() {
            self.registry.plugins.insert(index, entry);
            return Err(error);
        }
        self.stop(id);
        let name = self
            .plugins
            .get(id)
            .map_or_else(|| id.to_owned(), |plugin| plugin.manifest.name.clone());
        self.remove(id);
        self.errors.remove(id);
        self.reindex();
        lock(&self.server.hub).forget(id);
        self.delete_package(&entry.folder);
        // A registry entry that never loaded was not validated as a file name.
        if package::identifier(id) {
            let _ = fs::remove_dir_all(self.root.join("Browser").join(id));
            let _ = fs::remove_file(self.root.join("Logs").join(format!("{id}.log")));
        }
        Ok(name)
    }

    /// Deletes a package folder only when DeckX created it under Packages. Folders imported
    /// before DeckX copied them stay where they are.
    fn delete_package(&self, folder: &Path) {
        let Ok(packages) = fs::canonicalize(self.packages()) else {
            return;
        };
        if let Ok(relative) = folder.strip_prefix(&packages)
            && let Some(Component::Normal(name)) = relative.components().next()
        {
            let _ = fs::remove_dir_all(packages.join(name));
        }
    }

    fn validate_unique(&self, plugin: &Plugin, update: bool) -> Result<()> {
        ensure!(
            update || !self.plugins.contains_key(&plugin.id),
            "{} is already imported",
            plugin.manifest.name
        );
        if let Some(action) = plugin.manifest.actions.iter().find(|action| {
            self.actions
                .get(&action.uuid)
                .is_some_and(|owner| *owner != plugin.id)
        }) {
            bail!("Action UUID already in use: {}", action.uuid);
        }
        Ok(())
    }

    fn insert(&mut self, plugin: Plugin) {
        for action in &plugin.manifest.actions {
            self.actions.insert(action.uuid.clone(), plugin.id.clone());
        }
        self.plugins.insert(plugin.id.clone(), plugin);
    }

    fn remove(&mut self, id: &str) {
        if let Some(plugin) = self.plugins.remove(id) {
            for action in &plugin.manifest.actions {
                self.actions.remove(&action.uuid);
            }
        }
    }

    /// Rebuilds the catalog and the hub's action index after plugins change.
    fn reindex(&mut self) {
        let mut catalog = self
            .registry
            .plugins
            .iter()
            .filter_map(|entry| self.plugins.get(&entry.id))
            .flat_map(|plugin| {
                plugin
                    .manifest
                    .actions
                    .iter()
                    .filter(|action| action.visible_in_actions_list)
                    .map(|action| CatalogItem {
                        uuid: action.uuid.clone(),
                        name: action.name.clone(),
                        category: if plugin.manifest.category.is_empty() {
                            plugin.manifest.name.clone()
                        } else {
                            plugin.manifest.category.clone()
                        },
                        tooltip: action.tooltip.clone(),
                        key: action.accepts_key(),
                        dial: action.accepts_dial(),
                        inspector: action.inspector.is_some(),
                    })
            })
            .collect::<Vec<_>>();
        catalog.sort_by(|a, b| (&a.category, &a.name).cmp(&(&b.category, &b.name)));
        self.listed = catalog
            .iter()
            .enumerate()
            .map(|(index, item)| (item.uuid.clone(), index))
            .collect();
        self.catalog = catalog;
        lock(&self.server.hub).set_owners(self.actions.clone());
    }

    fn save(&self) -> Result<()> {
        crate::profile::save_json(&self.root.join("plugins.json"), &self.registry)
    }

    pub fn set_node(&mut self, node: PathBuf) -> Result<()> {
        ensure!(
            node.is_file()
                && node
                    .file_name()
                    .is_some_and(|s| s.eq_ignore_ascii_case("node.exe")),
            "Choose a node.exe runtime"
        );
        let old = self.registry.node.replace(node);
        if let Err(error) = self.save() {
            self.registry.node = old;
            return Err(error);
        }
        Ok(())
    }

    pub fn node(&self) -> Option<&Path> {
        self.registry.node.as_deref()
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<()> {
        let index = self
            .registry
            .plugins
            .iter()
            .position(|entry| entry.id == id)
            .context("Plugin is missing")?;
        if enabled {
            self.launch(id).inspect_err(|error| {
                self.errors.insert(id.to_owned(), format!("{error:#}"));
            })?;
        }
        let previous = self.registry.plugins[index].enabled;
        self.registry.plugins[index].enabled = enabled;
        if let Err(error) = self.save() {
            self.registry.plugins[index].enabled = previous;
            if enabled {
                self.stop(id);
            }
            return Err(error);
        }
        if !enabled {
            self.stop(id);
            self.errors.remove(id);
        }
        Ok(())
    }

    fn launch(&mut self, id: &str) -> Result<()> {
        self.stop(id);
        let plugin = self
            .plugins
            .get(id)
            .context("The plugin folder could not be loaded")?;
        let token = token();
        let (port, info) = {
            let mut hub = lock(&self.server.hub);
            hub.begin(plugin.clone(), token.clone());
            (hub.port, hub.info(plugin).to_string())
        };
        let result = self.command(plugin, &token).and_then(|mut command| {
            if plugin.backend != Backend::Browser {
                command.args([
                    "-port",
                    &port.to_string(),
                    "-pluginUUID",
                    &token,
                    "-registerEvent",
                    "registerPlugin",
                    "-info",
                    &info,
                ]);
            }
            let logs = self.root.join("Logs");
            fs::create_dir_all(&logs)?;
            let log = File::create(logs.join(format!("{}.log", plugin.id)))?;
            command
                .current_dir(process_path(&plugin.root))
                .stdin(Stdio::null())
                .stdout(Stdio::from(log.try_clone()?))
                .stderr(Stdio::from(log));
            process::spawn(&mut command)
                .with_context(|| format!("Could not start {}", plugin.manifest.name))
        });
        match result {
            Ok((child, job)) => {
                self.running.insert(
                    id.to_owned(),
                    Running {
                        child,
                        job,
                        launched: Instant::now(),
                    },
                );
                self.errors.remove(id);
                Ok(())
            }
            Err(error) => {
                lock(&self.server.hub).stop(id);
                Err(error)
            }
        }
    }

    fn command(&self, plugin: &Plugin, token: &str) -> Result<Command> {
        match plugin.backend {
            Backend::Node => {
                let configured = self
                    .registry
                    .node
                    .clone()
                    .or_else(|| process::search_path("node.exe"))
                    .context("Node.js is missing. Select node.exe in Plugins")?;
                if let Some(nodejs) = &plugin.manifest.nodejs {
                    let required = nodejs
                        .version
                        .as_str()
                        .and_then(|s| s.parse::<u64>().ok())
                        .or_else(|| nodejs.version.as_u64());
                    if let Some(required) = required {
                        let version = Command::new(&configured)
                            .arg("--version")
                            .creation_flags(CREATE_NO_WINDOW)
                            .output()
                            .context("Node.js is missing. Select node.exe in Plugins")?;
                        let version = String::from_utf8_lossy(&version.stdout);
                        let major = version
                            .trim()
                            .trim_start_matches('v')
                            .split('.')
                            .next()
                            .and_then(|s| s.parse::<u64>().ok());
                        ensure!(
                            major.is_some_and(|major| major >= required),
                            "This plugin requires Node.js {required} or later"
                        );
                    }
                }
                let mut command = Command::new(configured);
                command.arg(plugin.entry.strip_prefix(&plugin.root)?);
                Ok(command)
            }
            Backend::Windows => Ok(Command::new(&plugin.entry)),
            Backend::Browser => {
                let exe = [
                    std::env::var_os("ProgramFiles(x86)"),
                    std::env::var_os("ProgramFiles"),
                ]
                .into_iter()
                .flatten()
                .map(PathBuf::from)
                .map(|path| path.join("Microsoft/Edge/Application/msedge.exe"))
                .find(|path| path.is_file())
                .context("This legacy plugin requires Microsoft Edge")?;
                let url = lock(&self.server.hub).open_backend(plugin, token)?;
                let data = self.root.join("Browser").join(&plugin.id);
                fs::create_dir_all(&data)?;
                let mut command = Command::new(exe);
                command
                    .args([
                        "--headless=new",
                        "--no-first-run",
                        "--disable-gpu",
                        "--disable-extensions",
                        // Keep Edge in the process DeckX owns when Windows applies an AppCompat layer.
                        "--edge-skip-compat-layer-relaunch",
                    ])
                    .arg(format!("--user-data-dir={}", data.display()))
                    .arg(url);
                Ok(command)
            }
        }
    }

    fn stop(&mut self, id: &str) {
        lock(&self.server.hub).stop(id);
        if let Some(mut running) = self.running.remove(id) {
            running.job.terminate();
            let _ = running.child.kill();
            let _ = running.child.wait();
        }
    }

    pub fn poll(&mut self) {
        let mut exited = Vec::new();
        for (id, running) in &mut self.running {
            match running.child.try_wait() {
                Ok(Some(status)) => exited.push((id.clone(), format!("Plugin exited ({status})"))),
                Err(error) => exited.push((
                    id.clone(),
                    format!("Could not check plugin process: {error}"),
                )),
                Ok(None) => {}
            }
        }
        for (id, error) in exited {
            self.stop(&id);
            self.errors.insert(id, error);
        }
    }

    /// Tuple form of `installed` for ui/mcp_api.rs.
    pub fn installed(&self) -> Vec<Installed> {
        let hub = lock(&self.server.hub);
        self.registry
            .plugins
            .iter()
            .map(|entry| {
                let plugin = self.plugins.get(&entry.id);
                let name = plugin.map_or(entry.id.as_str(), |plugin| plugin.manifest.name.as_str());
                let detail = plugin
                    .map(|plugin| {
                        format!("{} · {}", plugin.manifest.author, plugin.manifest.version)
                    })
                    .unwrap_or_default();
                let status = if let Some(error) = self.errors.get(&entry.id) {
                    error.clone()
                } else if hub.running(&entry.id) {
                    "Running".into()
                } else if let Some(running) = self.running.get(&entry.id) {
                    if running.launched.elapsed() < Duration::from_secs(12) {
                        "Starting…".into()
                    } else {
                        "Waiting for plugin registration".into()
                    }
                } else {
                    "Disabled".into()
                };
                Installed {
                    id: entry.id.clone(),
                    name: name.to_owned(),
                    detail,
                    enabled: entry.enabled,
                    status,
                    source: entry.source.clone(),
                }
            })
            .collect()
    }

    pub fn catalog(&self) -> Vec<CatalogItem> {
        self.catalog.clone()
    }

    pub fn item(&self, id: &str) -> Option<CatalogItem> {
        self.listed
            .get(id)
            .map(|&index| self.catalog[index].clone())
    }

    pub fn configure(
        &self,
        id: &str,
        instance: Option<&Instance>,
        settings: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        ensure!(self.plugins.contains_key(id), "Plugin is missing");
        if let Some(instance) = instance {
            ensure!(
                self.owner(&instance.action) == Some(id),
                "This binding does not belong to the plugin"
            );
        }
        let saving = settings.is_some();
        let value = lock(&self.server.hub).configure(id, instance, settings)?;
        if saving {
            self.server.flush()?;
        }
        Ok(value)
    }

    pub fn owner(&self, action: &str) -> Option<&str> {
        self.actions.get(action).map(String::as_str)
    }

    pub fn inspect(&self, instance: Instance) -> Result<()> {
        let uri = lock(&self.server.hub).open_panel(instance)?;
        crate::actions::open_http(&uri)
    }

    pub fn error(&self) -> Option<String> {
        lock(&self.server.hub).error().map(str::to_owned)
    }
}

impl Drop for PluginHost {
    fn drop(&mut self) {
        for id in self.running.keys().cloned().collect::<Vec<_>>() {
            self.stop(&id);
        }
    }
}

pub fn token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn process_path(path: &Path) -> PathBuf {
    // Node's module resolver expects a drive or UNC path in its working directory.
    let Some(path) = path.to_str() else {
        return path.to_owned();
    };
    if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{unc}"));
    }
    PathBuf::from(path.strip_prefix(r"\\?\").unwrap_or(path))
}

fn load_json<T: Default + DeserializeOwned>(path: &Path) -> Result<T> {
    match File::open(path) {
        Ok(file) => serde_json::from_reader(BufReader::new(file))
            .with_context(|| format!("Invalid {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(error.into()),
    }
}
