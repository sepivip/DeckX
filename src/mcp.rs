use std::{
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, SyncSender, sync_channel},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use eframe::egui;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    net::windows::named_pipe::{ClientOptions, NamedPipeServer},
    sync::{Semaphore, oneshot},
    time::timeout,
};

use crate::{
    actions::Action,
    codex::{Binding, KeyStyle},
    profile::{self, BUTTONS, Control, DIALS, DISPLAY_KEYS, Direction},
    workspace::Scene,
};

pub mod codex_setup;
mod security;
use security::PipeSecurity;

const PIPE_PREFIX: &str = r"\\.\pipe\deckx-mcp-";
const MESSAGE_LIMIT: usize = 4 * 1024 * 1024;
const PIPE_TIMEOUT: Duration = Duration::from_secs(15);
const READ_DEADLINE: Duration = Duration::from_secs(2);
const RETRY_MIN: Duration = Duration::from_millis(100);
const RETRY_MAX: Duration = Duration::from_secs(5);
const VERSION: &str = "2025-11-25";
pub const PROBE: &str = "deckx_check_local_connection";

#[derive(Deserialize, Serialize)]
struct Settings {
    enabled: bool,
}

pub fn enabled() -> Result<bool> {
    let path = profile::path()?.with_file_name("mcp-settings.json");
    match std::fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice::<Settings>(&bytes)?.enabled),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error).context("Could not read MCP settings"),
    }
}

pub fn save_enabled(enabled: bool) -> Result<()> {
    profile::save_json(
        &profile::path()?.with_file_name("mcp-settings.json"),
        &Settings { enabled },
    )
    .context("Could not save MCP settings")
}

pub fn test_connection() -> Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(call_app(PROBE.into(), json!({})))?;
    Ok(())
}

#[derive(Deserialize, Serialize)]
struct Endpoint {
    instance: uuid::Uuid,
    token: uuid::Uuid,
}

impl Endpoint {
    fn pipe(&self) -> String {
        format!("{PIPE_PREFIX}{}", self.instance.simple())
    }
}

fn endpoint_path() -> Result<PathBuf> {
    Ok(profile::path()?.with_file_name("mcp-endpoint.json"))
}

#[derive(Serialize, Deserialize)]
struct Call {
    token: uuid::Uuid,
    name: String,
    arguments: Value,
}

#[derive(Serialize, Deserialize)]
enum Reply {
    Success(Value),
    Error(String),
}

pub struct Pending {
    pub name: String,
    pub arguments: Value,
    reply: oneshot::Sender<Reply>,
}

impl Pending {
    pub fn answer(self, apply: impl FnOnce(&str, Value) -> Result<Value>) -> bool {
        if self.reply.is_closed() {
            return false;
        }
        let result = apply(&self.name, self.arguments);
        let _ = self.reply.send(match result {
            Ok(value) => Reply::Success(value),
            Err(error) => Reply::Error(format!("{error:#}")),
        });
        true
    }
}

pub struct AppEndpoint {
    pub incoming: Receiver<Pending>,
    stop: Option<oneshot::Sender<()>>,
}

impl AppEndpoint {
    pub fn start(ctx: egui::Context) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let endpoint = Endpoint {
            instance: uuid::Uuid::new_v4(),
            token: uuid::Uuid::new_v4(),
        };
        let security = PipeSecurity::current_user()?;
        let pipe = {
            let _entered = runtime.enter();
            security
                .create(&endpoint.pipe(), true)
                .context("Could not start the DeckX MCP server")?
        };
        let path = endpoint_path()?;
        profile::save_json(&path, &endpoint).context("Could not publish the DeckX MCP endpoint")?;
        let (send, incoming) = sync_channel(32);
        let (stop, shutdown) = oneshot::channel();
        std::thread::Builder::new()
            .name("DeckX MCP".into())
            .spawn(move || {
                runtime.block_on(listen(pipe, &endpoint, &security, send, ctx, shutdown));
            })?;
        Ok(Self {
            incoming,
            stop: Some(stop),
        })
    }
}

impl Drop for AppEndpoint {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

async fn listen(
    first: NamedPipeServer,
    endpoint: &Endpoint,
    security: &PipeSecurity,
    requests: SyncSender<Pending>,
    ctx: egui::Context,
    mut shutdown: oneshot::Receiver<()>,
) {
    let permits = std::sync::Arc::new(Semaphore::new(16));
    let name = endpoint.pipe();
    let mut next = Some(first);
    let mut delay = RETRY_MIN;
    loop {
        let pipe = match next
            .take()
            .map_or_else(|| security.create(&name, false), Ok)
        {
            Ok(pipe) => pipe,
            Err(_) => {
                if !pause(&mut shutdown, &mut delay).await {
                    return;
                }
                continue;
            }
        };
        let connected = tokio::select! {
            _ = &mut shutdown => return,
            result = pipe.connect() => result,
        };
        // Reserve the name with a new instance before this one can close.
        next = security.create(&name, false).ok();
        if connected.is_err() {
            drop(pipe);
            if !pause(&mut shutdown, &mut delay).await {
                return;
            }
            continue;
        }
        delay = RETRY_MIN;
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            continue;
        };
        let requests = requests.clone();
        let ctx = ctx.clone();
        let token = endpoint.token;
        tokio::spawn(async move {
            let _permit = permit;
            let _ = timeout(PIPE_TIMEOUT, serve_call(pipe, token, requests, ctx)).await;
        });
    }
}

/// Waits before the listener retries and doubles the next delay. Returns false
/// when DeckX is shutting the endpoint down.
async fn pause(shutdown: &mut oneshot::Receiver<()>, delay: &mut Duration) -> bool {
    let wait = *delay;
    *delay = (wait * 2).min(RETRY_MAX);
    tokio::select! {
        _ = shutdown => false,
        _ = tokio::time::sleep(wait) => true,
    }
}

async fn line(reader: &mut (impl AsyncBufReadExt + Unpin)) -> Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    reader
        .take((MESSAGE_LIMIT + 1) as u64)
        .read_until(b'\n', &mut bytes)
        .await?;
    ensure!(
        bytes.last() == Some(&b'\n') && bytes.len() <= MESSAGE_LIMIT,
        "Invalid DeckX MCP message"
    );
    Ok(bytes)
}

async fn serve_call(
    pipe: impl AsyncRead + AsyncWrite + Unpin,
    token: uuid::Uuid,
    requests: SyncSender<Pending>,
    ctx: egui::Context,
) -> Result<()> {
    let mut io = BufReader::new(pipe);
    // An idle client must not hold one of the connection permits.
    let request = timeout(READ_DEADLINE, line(&mut io))
        .await
        .map_err(|_| anyhow::anyhow!("The DeckX MCP client did not send a request"))??;
    let call: Call = serde_json::from_slice(&request).context("Invalid DeckX MCP request")?;
    ensure!(call.token == token, "Invalid DeckX MCP token");
    let (reply, receive) = oneshot::channel();
    requests
        .try_send(Pending {
            name: call.name,
            arguments: call.arguments,
            reply,
        })
        .map_err(|_| anyhow::anyhow!("DeckX is busy"))?;
    ctx.request_repaint();
    let result = timeout(PIPE_TIMEOUT - Duration::from_secs(1), receive)
        .await
        .map_err(|_| anyhow::anyhow!("DeckX did not process the request"))??;
    let mut bytes = serde_json::to_vec(&result)?;
    bytes.push(b'\n');
    io.get_mut().write_all(&bytes).await?;
    Ok(())
}

async fn call_app(name: String, arguments: Value) -> Result<Value> {
    let endpoint: Endpoint = serde_json::from_slice(
        &std::fs::read(endpoint_path()?).context("Open DeckX to configure the AJAZZ")?,
    )?;
    let mut pipe = ClientOptions::new()
        .open(endpoint.pipe())
        .context("Open DeckX or restart it to reconnect the MCP server")?;
    let mut bytes = serde_json::to_vec(&Call {
        token: endpoint.token,
        name,
        arguments,
    })?;
    bytes.push(b'\n');
    ensure!(
        bytes.len() <= MESSAGE_LIMIT,
        "DeckX MCP request is too large"
    );
    pipe.write_all(&bytes).await?;
    let response = timeout(
        PIPE_TIMEOUT + Duration::from_secs(1),
        line(&mut BufReader::new(pipe)),
    )
    .await
    .context("DeckX did not reply")??;
    match serde_json::from_slice(&response)? {
        Reply::Success(value) => Ok(value),
        Reply::Error(message) => bail!("{message}"),
    }
}

pub fn stdio() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut stdout = io::stdout().lock();
    for input in io::stdin().lock().lines() {
        let input = input?;
        let result = match serde_json::from_str::<Value>(&input) {
            Ok(request) => respond(request, &runtime),
            Err(_) => Some(rpc_error(Value::Null, -32700, "Invalid JSON")),
        };
        if let Some(result) = result {
            serde_json::to_writer(&mut stdout, &result)?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

fn respond(request: Value, runtime: &tokio::runtime::Runtime) -> Option<Value> {
    let method = request["method"].as_str();
    if request["jsonrpc"] != "2.0" || method.is_none() {
        return Some(rpc_error(request["id"].clone(), -32600, "Invalid request"));
    }
    let id = request.get("id")?.clone();
    let params = &request["params"];
    let result = match method? {
        "initialize" => {
            let version = match params["protocolVersion"].as_str() {
                Some(version @ ("2024-11-05" | "2025-03-26" | "2025-06-18" | VERSION)) => version,
                _ => VERSION,
            };
            json!({"protocolVersion":version,"serverInfo":{"name":"DeckX","version":crate::APP_VERSION},"capabilities":{"tools":{}},"instructions":"Open DeckX on this PC. Read deckx_state before editing. Indices on the AKP03E are one-based. Use the named tools to configure the user's deck."})
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => {
            let Some(name) = params["name"].as_str() else {
                return Some(rpc_error(id, -32602, "Expected a tool name"));
            };
            if !tools().iter().any(|tool| tool["name"] == name) {
                return Some(rpc_error(id, -32602, "Unknown DeckX tool"));
            }
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if !args.is_object() {
                return Some(rpc_error(id, -32602, "Tool arguments must be an object"));
            }
            match execute(runtime, name, args) {
                Ok(result) => {
                    json!({"content":[{"type":"text","text":result.to_string()}],"structuredContent":result})
                }
                Err(error) => {
                    json!({"content":[{"type":"text","text":format!("{error:#}")}],"isError":true})
                }
            }
        }
        _ => return Some(rpc_error(id, -32601, "Method not found")),
    };
    Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
}

fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

fn execute(runtime: &tokio::runtime::Runtime, name: &str, args: Value) -> Result<Value> {
    match name {
        "deckx_search_marketplace" => {
            let SearchMarketplace { query, offset } = decode(args)?;
            crate::marketplace::mcp_search(&query, offset)
        }
        "deckx_install_marketplace" => {
            let InstallMarketplace {
                query,
                slug,
                offset,
            } = decode(args)?;
            let package = crate::marketplace::mcp_download(&query, &slug, offset)?;
            runtime.block_on(call_app(
                "deckx_edit_plugin".into(),
                json!({"op":"import", "path": package.path}),
            ))
        }
        _ => runtime.block_on(call_app(name.into(), args)),
    }
}

fn tools() -> &'static [Value] {
    static TOOLS: std::sync::LazyLock<Vec<Value>> = std::sync::LazyLock::new(|| {
        vec![
            tool::<Empty>(
                "deckx_state",
                "Read the saved workspace, profile and page IDs, hardware connection, and applied brightness.",
                true,
                false,
            ),
            tool::<EditWorkspace>(
                "deckx_edit_workspace",
                "Save a batch of profile, page, LCD key, side button, dial, and display edits. Changes are validated together. Names and page IDs come from deckx_state.",
                false,
                true,
            ),
            tool::<ImportWorkspace>(
                "deckx_import_workspace",
                "Replace the workspace with a DeckX workspace JSON file. Use deckx_state to inspect the current workspace first.",
                false,
                true,
            ),
            tool::<Brightness>(
                "deckx_set_brightness",
                "Save the AJAZZ LCD brightness (0–100%) and send it to the connected deck.",
                false,
                false,
            ),
            tool::<Connection>(
                "deckx_connect",
                "Connect or disconnect DeckX from the AJAZZ. Read deckx_state for the connection outcome.",
                false,
                false,
            ),
            tool::<Empty>(
                "deckx_codex_mode",
                "Read Codex Mode state, recent chat IDs, key style, and key and dial assignments.",
                true,
                false,
            ),
            tool::<EditCodex>(
                "deckx_edit_codex_mode",
                "Enter or exit Codex Mode, select a key style, reset assignments, or edit key and dial bindings.",
                false,
                true,
            ),
            tool::<Empty>(
                "deckx_plugins",
                "List installed plugins, their status, supported actions, and the selected Node.js runtime.",
                true,
                false,
            ),
            tool::<EditPlugin>(
                "deckx_edit_plugin",
                "Import a local Stream Deck plugin package or folder, or enable or disable a plugin. Imported plugins start disabled. Enabling asks the user to approve the plugin in DeckX; it starts only after they do.",
                false,
                true,
            ),
            tool::<PluginSettings>(
                "deckx_plugin_settings",
                "Read or replace a plugin's global settings or a saved key or dial's settings. Omit settings to read. Plugin settings may contain credentials.",
                false,
                true,
            ),
            tool::<SearchMarketplace>(
                "deckx_search_marketplace",
                "Search the Elgato Marketplace plugin catalog and get listing slugs.",
                true,
                false,
            ),
            tool::<InstallMarketplace>(
                "deckx_install_marketplace",
                "Install a free Windows plugin with a public publisher package. Use the query and listing slug from search. The plugin starts disabled.",
                false,
                true,
            ),
        ]
    });
    &TOOLS
}

fn tool<T: JsonSchema>(name: &str, description: &str, read: bool, destructive: bool) -> Value {
    let mut schema = serde_json::to_value(
        schemars::generate::SchemaSettings::draft07()
            .into_generator()
            .into_root_schema_for::<T>(),
    )
    .expect("JSON schema");
    schema["type"] = json!("object");
    json!({"name":name,"description":description,"inputSchema":schema,"annotations":{"readOnlyHint":read,"destructiveHint":destructive,"openWorldHint": name.ends_with("marketplace")}})
}

pub fn decode<T: DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).context("Invalid tool arguments")
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Target {
    Lcd { index: usize },
    Side { index: usize },
    Dial { index: usize, gesture: Gesture },
}

impl Target {
    pub fn control(self) -> Result<Control> {
        match self {
            Self::Lcd { index } if (1..=DISPLAY_KEYS).contains(&index) => {
                Ok(Control::Button(index - 1))
            }
            Self::Side { index } if (1..=BUTTONS - DISPLAY_KEYS).contains(&index) => {
                Ok(Control::Button(DISPLAY_KEYS + index - 1))
            }
            Self::Dial { index, gesture } if (1..=DIALS).contains(&index) => Ok(Control::Dial(
                index - 1,
                match gesture {
                    Gesture::Left => Direction::Counterclockwise,
                    Gesture::Press => Direction::Press,
                    Gesture::Right => Direction::Clockwise,
                },
            )),
            _ => bail!("Control index is out of range. Use LCD 1–6, side 1–3, or dial 1–3"),
        }
    }
}

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Gesture {
    Left,
    Press,
    Right,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditWorkspace {
    pub changes: Vec<WorkspaceChange>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkspaceChange {
    AddProfile {
        name: String,
    },
    RenameProfile {
        profile: u64,
        name: String,
    },
    RemoveProfile {
        profile: u64,
    },
    LinkApp {
        profile: u64,
        app: Option<PathBuf>,
    },
    AddPage {
        profile: u64,
        name: String,
        #[serde(default)]
        folder: bool,
    },
    RenamePage {
        scene: Scene,
        name: String,
    },
    RemovePage {
        scene: Scene,
    },
    SelectPage {
        scene: Scene,
    },
    SetDisplay {
        brightness: Option<u8>,
        rotation: Option<u16>,
    },
    SetAction {
        scene: Scene,
        target: Target,
        action: Action,
    },
    SetLcdAppearance {
        scene: Scene,
        index: usize,
        label: Option<String>,
        image: Option<PathBuf>,
        color: Option<[u8; 3]>,
        #[serde(default)]
        clear_image: bool,
        #[serde(default)]
        clear_color: bool,
    },
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportWorkspace {
    pub path: PathBuf,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Brightness {
    #[schemars(range(max = 100))]
    pub brightness: u8,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub connected: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditCodex {
    pub enabled: Option<bool>,
    pub key_style: Option<KeyStyle>,
    #[serde(default)]
    pub reset_assignments: bool,
    #[serde(default)]
    pub bindings: Vec<CodexAssignment>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CodexAssignment {
    pub target: Target,
    pub binding: Binding,
}

#[derive(Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum EditPlugin {
    Import { path: PathBuf },
    Enable { plugin_id: String, enabled: bool },
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PluginSettings {
    pub scope: PluginScope,
    pub settings: Option<serde_json::Map<String, Value>>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginScope {
    Global { plugin_id: String },
    Binding { scene: Scene, target: Target },
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchMarketplace {
    #[serde(default)]
    query: String,
    #[serde(default)]
    offset: usize,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct InstallMarketplace {
    query: String,
    slug: String,
    #[serde(default)]
    offset: usize,
}

/// Accepts only absolute paths on a drive letter. UNC, verbatim, and device
/// paths could make DeckX authenticate to a remote host or open a pipe.
pub fn absolute(path: &Path) -> Result<()> {
    use std::path::{Component, Prefix};

    ensure!(
        path.is_absolute(),
        "Use an absolute Windows path such as C:\\Users\\you\\image.png"
    );
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => ensure!(
                matches!(prefix.kind(), Prefix::Disk(_)),
                "Use a path on a local drive letter. Network, device, and \\\\?\\ paths are not supported"
            ),
            Component::ParentDir => bail!("Use a path without .. components"),
            Component::Normal(name) => ensure!(
                !reserved_device(&name.to_string_lossy()),
                "{} is a reserved Windows device name",
                name.to_string_lossy()
            ),
            Component::RootDir | Component::CurDir => {}
        }
    }
    Ok(())
}

/// Windows opens these names as devices in any folder, with or without an extension.
fn reserved_device(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    let upper = stem.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|device| {
        upper.strip_prefix(device).is_some_and(|port| {
            (port.len() == 1 && port.as_bytes()[0].is_ascii_digit())
                || matches!(port, "¹" | "²" | "³")
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    fn request(token: uuid::Uuid, name: &str, arguments: Value) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(&Call {
            token,
            name: name.into(),
            arguments,
        })
        .unwrap();
        bytes.push(b'\n');
        bytes
    }

    #[tokio::test]
    async fn calls_with_the_wrong_token_never_reach_the_app() {
        let (send, incoming) = sync_channel::<Pending>(1);
        let (mut client, server) = duplex(4096);
        let token = uuid::Uuid::new_v4();
        client
            .write_all(&request(uuid::Uuid::new_v4(), "deckx_state", json!({})))
            .await
            .unwrap();
        let error = serve_call(server, token, send, egui::Context::default())
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "Invalid DeckX MCP token");
        assert!(incoming.try_recv().is_err());
    }

    #[tokio::test]
    async fn malformed_requests_are_rejected() {
        let (send, incoming) = sync_channel::<Pending>(1);
        let (mut client, server) = duplex(4096);
        client.write_all(b"{\"token\":42}\n").await.unwrap();
        let ctx = egui::Context::default();
        let error = serve_call(server, uuid::Uuid::nil(), send, ctx)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "Invalid DeckX MCP request");
        assert!(incoming.try_recv().is_err());
    }

    #[tokio::test]
    async fn idle_clients_are_disconnected_quickly() {
        let (send, _incoming) = sync_channel::<Pending>(1);
        let (mut client, server) = duplex(4096);
        client.write_all(b"{\"token\":").await.unwrap();
        let start = std::time::Instant::now();
        let ctx = egui::Context::default();
        let error = serve_call(server, uuid::Uuid::nil(), send, ctx)
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "The DeckX MCP client did not send a request"
        );
        assert!(start.elapsed() < PIPE_TIMEOUT);
    }

    #[tokio::test]
    async fn valid_calls_are_decoded_and_answered() {
        let (send, incoming) = sync_channel::<Pending>(1);
        let (mut client, server) = duplex(4096);
        let token = uuid::Uuid::new_v4();
        client
            .write_all(&request(
                token,
                "deckx_set_brightness",
                json!({"brightness": 40}),
            ))
            .await
            .unwrap();
        let app = std::thread::spawn(move || {
            let pending = incoming.recv().unwrap();
            assert!(pending.answer(|name, arguments| {
                Ok(json!({"name": name, "brightness": arguments["brightness"]}))
            }));
        });
        serve_call(server, token, send, egui::Context::default())
            .await
            .unwrap();
        app.join().unwrap();
        let reply = line(&mut BufReader::new(client)).await.unwrap();
        let Reply::Success(value) = serde_json::from_slice::<Reply>(&reply).unwrap() else {
            panic!("DeckX reported an error");
        };
        assert_eq!(
            value,
            json!({"name": "deckx_set_brightness", "brightness": 40})
        );
    }

    #[test]
    fn only_local_drive_paths_are_absolute() {
        for path in [
            r"C:\Users\me\key.png",
            "C:/Users/me/key.png",
            r"d:\Decks\.\profile.json",
            r"C:\Users\me\console.png",
            r"C:\Users\me\com10.png",
        ] {
            assert!(absolute(Path::new(path)).is_ok(), "{path}");
        }
        for path in [
            r"\\host\share\key.png",
            "//host/share/key.png",
            r"\\?\C:\Users\me\key.png",
            r"\\?\UNC\host\share\key.png",
            r"\\.\pipe\deckx",
            r"\\.\C:\key.png",
            r"\\?\pipe\deckx",
            r"key.png",
            r"Decks\key.png",
            r"C:key.png",
            r"\Users\me\key.png",
            r"C:\Users\me\..\other\key.png",
            r"C:\Users\..",
            r"C:\Temp\NUL",
            r"C:\Temp\con.png",
            r"C:\Temp\COM1",
            r"C:\Temp\lpt9.txt",
            r"C:\Temp\AUX .txt",
            r"C:\Temp\CONOUT$",
        ] {
            assert!(absolute(Path::new(path)).is_err(), "{path}");
        }
    }
}
