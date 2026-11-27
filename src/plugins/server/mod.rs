//! The loopback Stream Deck SDK server that plugins and property inspectors connect to.

mod appearance;
mod http;
mod protocol;
mod storage;

use std::{
    collections::HashMap,
    net::{Ipv4Addr, TcpListener},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc, oneshot};

use super::package::{Action, Plugin};
use appearance::Looks;
#[cfg(test)]
pub(super) use http::local_request;
use storage::Data;

pub const DEVICE_ID: &str = "deckx.akp03e";
const MAX_MESSAGE: usize = 12 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Instance {
    pub context: String,
    pub action: String,
    pub control: crate::profile::Control,
}

impl Instance {
    fn encoder(&self) -> bool {
        matches!(self.control, crate::profile::Control::Dial(..))
    }

    fn coordinates(&self) -> (usize, usize) {
        match self.control {
            crate::profile::Control::Button(index) => (index % 3, index / 3),
            crate::profile::Control::Dial(index, _) => (index, 0),
        }
    }
}

#[derive(Clone)]
enum Identity {
    Plugin(String),
    Inspector { plugin: String, instance: Instance },
}

impl Identity {
    fn plugin(&self) -> &str {
        match self {
            Self::Plugin(plugin) | Self::Inspector { plugin, .. } => plugin,
        }
    }

    fn event(&self) -> &'static str {
        match self {
            Self::Plugin(_) => "registerPlugin",
            Self::Inspector { .. } => "registerPropertyInspector",
        }
    }
}

#[derive(Clone)]
struct Panel {
    identity: Identity,
    root: PathBuf,
    entry: String,
    created: Instant,
}

struct Peer {
    identity: Identity,
    tx: mpsc::Sender<Value>,
}

pub struct Hub {
    pub(super) port: u16,
    data_path: PathBuf,
    /// Shared with the writer so a snapshot costs nothing under the lock.
    data: Arc<Data>,
    /// Counts settings changes; the writer records the last one it saved.
    revision: u64,
    saves: Arc<Notify>,
    error: Option<String>,
    save_error: Option<String>,
    opened: HashMap<String, Instant>,
    connected: bool,
    /// Running plugins.
    plugins: HashMap<String, Plugin>,
    /// Action UUID to plugin UUID for every installed plugin, running or not.
    owners: HashMap<String, String>,
    tokens: HashMap<String, String>,
    peers: HashMap<String, Peer>,
    panels: HashMap<String, Panel>,
    visible: HashMap<String, Instance>,
    appearances: HashMap<String, Looks>,
}

pub type Shared = Arc<Mutex<Hub>>;

pub fn lock(hub: &Shared) -> MutexGuard<'_, Hub> {
    hub.lock().unwrap_or_else(|poison| poison.into_inner())
}

pub struct Server {
    pub hub: Shared,
    /// Serializes settings writes and holds the last revision written.
    written: Arc<Mutex<u64>>,
    stop: Option<oneshot::Sender<()>>,
}

impl Server {
    pub fn start(data_path: PathBuf) -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .context("Could not open the plugin server")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let data: Data = super::load_json(&data_path)?;
        let saves = Arc::new(Notify::new());
        let hub = Arc::new(Mutex::new(Hub {
            port,
            data_path,
            data: Arc::new(data),
            revision: 0,
            saves: saves.clone(),
            error: None,
            save_error: None,
            opened: HashMap::new(),
            connected: false,
            plugins: HashMap::new(),
            owners: HashMap::new(),
            tokens: HashMap::new(),
            peers: HashMap::new(),
            panels: HashMap::new(),
            visible: HashMap::new(),
            appearances: HashMap::new(),
        }));
        let state = hub.clone();
        let written = Arc::new(Mutex::new(0));
        let writer = (hub.clone(), written.clone());
        let (stop, shutdown) = oneshot::channel();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        std::thread::Builder::new()
            .name("DeckX plugins".into())
            .spawn(move || {
                runtime.block_on(async move {
                    let listener = match tokio::net::TcpListener::from_std(listener) {
                        Ok(listener) => listener,
                        Err(error) => {
                            lock(&state).error = Some(format!("Plugin server: {error}"));
                            return;
                        }
                    };
                    tokio::select! {
                        _ = axum::serve(listener, http::router(state)) => {},
                        _ = storage::write_settings(writer, saves) => {},
                        _ = shutdown => {},
                    }
                });
            })?;
        Ok(Self {
            hub,
            written,
            stop: Some(stop),
        })
    }

    /// Writes pending settings now instead of after the save delay.
    pub fn flush(&self) -> Result<()> {
        storage::flush(&self.hub, &self.written)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = self.flush();
    }
}

impl Hub {
    pub(super) fn begin(&mut self, plugin: Plugin, token: String) {
        self.stop(&plugin.id);
        for action in &plugin.manifest.actions {
            self.owners.insert(action.uuid.clone(), plugin.id.clone());
        }
        self.tokens.insert(token, plugin.id.clone());
        self.plugins.insert(plugin.id.clone(), plugin);
    }

    /// Replaces the action index after plugins are installed or removed.
    pub(super) fn set_owners(&mut self, owners: HashMap<String, String>) {
        self.owners = owners;
    }

    pub(super) fn stop(&mut self, plugin: &str) {
        if self.plugins.remove(plugin).is_some() {
            self.appearances.retain(|context, _| {
                !self.visible.get(context).is_some_and(|instance| {
                    self.owners
                        .get(&instance.action)
                        .is_some_and(|owner| owner == plugin)
                })
            });
        }
        self.tokens.retain(|_, id| id != plugin);
        self.opened.remove(plugin);
        self.peers
            .retain(|_, peer| peer.identity.plugin() != plugin);
        self.panels
            .retain(|_, panel| panel.identity.plugin() != plugin);
    }

    pub(super) fn running(&self, plugin: &str) -> bool {
        self.peers
            .values()
            .any(|peer| matches!(&peer.identity, Identity::Plugin(id) if id == plugin))
    }

    pub(super) fn error(&self) -> Option<&str> {
        self.error.as_deref().or(self.save_error.as_deref())
    }

    pub fn available(&self, action: &str) -> bool {
        self.lookup(action).is_some()
    }

    pub(super) fn info(&self, plugin: &Plugin) -> Value {
        json!({
            "application": {"name":"DeckX","font":"Inter","language":"en","platform":"windows","platformVersion":"10","version":"7.6.1"},
            "colors": {"buttonMouseOverBackgroundColor":"#3a3a3aff","buttonPressedBackgroundColor":"#3e3e3eff",
                       "buttonPressedBorderColor":"#686868ff","buttonPressedTextColor":"#ffffffff","highlightColor":"#007bffff"},
            "devicePixelRatio": 1,
            "devices": if self.connected { vec![device_info()] } else { Vec::new() },
            "plugin": {"uuid": plugin.id, "version": plugin.manifest.version}
        })
    }

    pub fn set_visible(&mut self, instances: Vec<Instance>) {
        let next: HashMap<_, _> = instances
            .into_iter()
            .map(|instance| (instance.context.clone(), instance))
            .collect();
        for (context, previous) in &self.visible {
            if next.get(context) != Some(previous) {
                self.broadcast_instance(previous, "willDisappear", json!({}));
            }
        }
        for (context, current) in &next {
            if self.visible.get(context) != Some(current) {
                self.broadcast_instance(current, "willAppear", json!({}));
            }
        }
        self.visible = next;
        // Hidden instances are reinitialized by the plugin when they appear again.
        self.appearances
            .retain(|context, _| self.visible.contains_key(context));
    }

    pub fn device(&mut self, connected: bool) {
        if self.connected == connected {
            return;
        }
        self.connected = connected;
        if !connected {
            self.set_visible(Vec::new());
        }
        let message = if connected {
            json!({"event":"deviceDidConnect","device":DEVICE_ID,"deviceInfo":device_info()})
        } else {
            json!({"event":"deviceDidDisconnect","device":DEVICE_ID})
        };
        for peer in self
            .peers
            .values()
            .filter(|p| matches!(&p.identity, Identity::Plugin(_)))
        {
            let _ = peer.tx.try_send(message.clone());
        }
    }

    pub fn input(&mut self, instance: &Instance, event: &str, payload: Value) -> bool {
        let Some((owner, action)) = self.lookup(&instance.action) else {
            return false;
        };
        if !self.running(owner) {
            return false;
        }
        if event == "keyUp" && !action.disable_automatic_states && action.states.len() == 2 {
            self.toggle_state(&instance.context);
        }
        self.broadcast_instance(instance, event, payload);
        true
    }

    pub(super) fn open_panel(&mut self, instance: Instance) -> Result<String> {
        let (owner, action) = self
            .lookup(&instance.action)
            .context("Enable the plugin first")?;
        let plugin = &self.plugins[owner];
        let entry = action
            .inspector
            .as_deref()
            .context("This action has no property inspector")?;
        let panel = Panel {
            identity: Identity::Inspector {
                plugin: owner.to_owned(),
                instance,
            },
            root: plugin.root.clone(),
            entry: relative_url(&plugin.root, entry)?,
            created: Instant::now(),
        };
        self.add_panel(super::token(), panel)
    }

    pub(super) fn open_backend(&mut self, plugin: &Plugin, token: &str) -> Result<String> {
        self.add_panel(
            token.to_owned(),
            Panel {
                identity: Identity::Plugin(plugin.id.clone()),
                root: plugin.root.clone(),
                entry: relative_url(&plugin.root, &plugin.entry)?,
                created: Instant::now(),
            },
        )
    }

    fn add_panel(&mut self, token: String, panel: Panel) -> Result<String> {
        self.panels.retain(|id, panel| {
            self.peers.contains_key(id)
                || matches!(&panel.identity, Identity::Plugin(_))
                || panel.created.elapsed() < Duration::from_secs(600)
        });
        ensure!(
            self.panels.len() < 64,
            "Close a plugin inspector before opening another"
        );
        let uri = format!("http://127.0.0.1:{}/p/{}/{}", self.port, token, panel.entry);
        self.panels.insert(token, panel);
        Ok(uri)
    }

    /// Finds a running plugin's action.
    fn lookup(&self, action: &str) -> Option<(&str, &Action)> {
        let owner = self.owners.get(action)?;
        let action = self.plugins.get(owner)?.action(action)?;
        Some((owner, action))
    }

    fn packet(&self, instance: &Instance, event: &str, extras: Value) -> Value {
        let (column, row) = instance.coordinates();
        let owner = self
            .lookup(&instance.action)
            .map(|(owner, _)| owner)
            .unwrap_or("");
        let mut payload = json!({
            "controller": if instance.encoder() { "Encoder" } else { "Keypad" },
            "coordinates": {"column":column,"row":row},
            "settings": self.settings(owner, &instance.context),
            "isInMultiAction":false,
            "resources": {}
        });
        if let Some((_, action)) = self.lookup(&instance.action)
            && action.states.len() > 1
        {
            payload["state"] = json!(self.state(&instance.context));
        }
        if let (Some(payload), Some(extras)) = (payload.as_object_mut(), extras.as_object()) {
            payload.extend(extras.clone());
        }
        json!({"action":instance.action,"context":instance.context,"device":DEVICE_ID,"event":event,"payload":payload})
    }

    fn broadcast_instance(&self, instance: &Instance, event: &str, extras: Value) {
        if let Some((owner, _)) = self.lookup(&instance.action) {
            let packet = self.packet(instance, event, extras);
            self.to_plugin(owner, &packet);
        }
    }

    fn to_plugin(&self, owner: &str, packet: &Value) {
        for peer in self
            .peers
            .values()
            .filter(|p| matches!(&p.identity, Identity::Plugin(id) if id == owner))
        {
            let _ = peer.tx.try_send(packet.clone());
        }
    }

    fn to_inspector(&self, owner: &str, context: &str, packet: &Value) {
        for peer in self.peers.values().filter(|p| matches!(&p.identity, Identity::Inspector { plugin, instance } if plugin == owner && instance.context == context)) {
            let _ = peer.tx.try_send(packet.clone());
        }
    }
}

fn device_info() -> Value {
    json!({"id":DEVICE_ID,"name":"AJAZZ AKP03E","type":1,"size":{"columns":3,"rows":2}})
}

fn relative_url(root: &Path, entry: &Path) -> Result<String> {
    let relative = entry.strip_prefix(root)?;
    let mut url = url::Url::parse("http://127.0.0.1/")?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("Invalid plugin entry point"))?;
        segments.clear();
        for part in relative.components() {
            segments.push(
                part.as_os_str()
                    .to_str()
                    .context("Plugin file name must use Unicode")?,
            );
        }
    }
    Ok(url.path().trim_start_matches('/').to_owned())
}
