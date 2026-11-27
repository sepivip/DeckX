use std::{
    collections::{BTreeSet, HashMap},
    fs::{self, File},
    io::{BufRead, BufReader, ErrorKind},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result, ensure};
use eframe::egui;
use image::{Rgb, RgbImage};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json, value::RawValue};
use tokio::{
    io::{self, AsyncReadExt, AsyncWriteExt},
    net::windows::named_pipe::ClientOptions,
    sync::watch,
    time::{sleep, timeout},
};
use uuid::Uuid;

use crate::{deck_model::LCD_PIXELS, profile};

mod desktop;
mod layout;
mod title_tile;
pub(crate) use desktop::find_in_desktop;
pub use layout::{Agent, Binding, KeyStyle, Layout, Mode, ResolvedLayout};
pub(crate) use title_tile::{installed_font as title_font, style as title_style};

const PIPE: &str = r"\\.\pipe\codex-ipc";
const LOCAL: &str = "local";
const FOLLOW: &str = "thread-stream-following-changed";
const STREAM: &str = "thread-stream-state-changed";
const MAX_FRAME: u32 = 32 * 1024 * 1024;
pub(crate) const STATUS_FORMAT_ERROR: &str = "DeckX couldn't read some Codex status updates.";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Waiting,
    Idle,
    Working,
    Approval,
    Input,
    Unread,
    Error,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Waiting => "Waiting for live Codex status",
            Self::Idle => "Idle",
            Self::Working => "Working",
            Self::Approval => "Awaiting approval",
            Self::Input => "Awaiting response",
            Self::Unread => "Unread",
            Self::Error => "Error",
        }
    }

    pub fn color(self) -> [u8; 3] {
        match self {
            Self::Waiting => [0, 0, 0],
            Self::Idle => [255, 255, 255],
            Self::Working => [48, 79, 254],
            Self::Approval | Self::Input => [255, 109, 0],
            Self::Unread => [0, 255, 76],
            Self::Error => [255, 0, 51],
        }
    }
}

pub fn classic_color(has_chat: bool, has_action: bool, status: Status) -> [u8; 3] {
    if has_chat {
        status.color()
    } else if has_action {
        [26, 28, 33]
    } else {
        [0; 3]
    }
}

pub fn tile(
    chat: Option<&Agent>,
    action_label: &str,
    status: Status,
    selected: bool,
    style: KeyStyle,
) -> RgbImage {
    if style == KeyStyle::Classic {
        return RgbImage::from_pixel(
            LCD_PIXELS,
            LCD_PIXELS,
            Rgb(classic_color(
                chat.is_some(),
                !action_label.is_empty(),
                status,
            )),
        );
    }
    let Some(chat) = chat else {
        return if action_label.is_empty() {
            RgbImage::new(LCD_PIXELS, LCD_PIXELS)
        } else {
            profile::label_tile([26, 28, 33], action_label)
        };
    };
    title_style(status, selected).render(&chat.title)
}

type Statuses = Arc<Mutex<HashMap<Uuid, Status>>>;
type MonitorError = Arc<Mutex<Option<String>>>;

#[derive(Clone)]
pub struct Activity {
    watched: watch::Sender<BTreeSet<Uuid>>,
    statuses: Statuses,
    error: MonitorError,
}

impl Activity {
    #[cfg(test)]
    pub(crate) fn fixture() -> Self {
        let (watched, _) = watch::channel(BTreeSet::new());
        Self {
            watched,
            statuses: Statuses::default(),
            error: MonitorError::default(),
        }
    }

    pub fn start(ctx: egui::Context) -> Self {
        let (watched, incoming) = watch::channel(BTreeSet::new());
        let statuses = Statuses::default();
        let error = MonitorError::default();
        let worker = (statuses.clone(), error.clone());
        let _ = std::thread::Builder::new()
            .name("DeckX Codex activity".into())
            .spawn(move || {
                if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    runtime.block_on(monitor(incoming, worker.0, worker.1, ctx));
                }
            });
        Self {
            watched,
            statuses,
            error,
        }
    }

    /// Reports a connection problem or unreadable status data from Codex.
    pub fn error(&self) -> Option<String> {
        self.error
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn track(&self, chats: BTreeSet<Uuid>) {
        self.watched.send_if_modified(|current| {
            if *current == chats {
                return false;
            }
            *current = chats;
            true
        });
    }

    pub fn status(&self, id: Uuid) -> Status {
        self.statuses
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&id)
            .copied()
            .unwrap_or_default()
    }
}

async fn monitor(
    mut watched: watch::Receiver<BTreeSet<Uuid>>,
    statuses: Statuses,
    error: MonitorError,
    ctx: egui::Context,
) {
    while watched.wait_for(|chats| !chats.is_empty()).await.is_ok() {
        if let Err(failure) = follow_chats(&mut watched, &statuses, &error, &ctx).await {
            report(&error, &ctx, Some(format!("{failure:#}")));
        }
        publish(&statuses, &ctx, HashMap::new());
        sleep(Duration::from_secs(2)).await;
    }
}

fn report(error: &MonitorError, ctx: &egui::Context, next: Option<String>) {
    let mut error = error.lock().unwrap_or_else(|error| error.into_inner());
    if *error != next {
        *error = next;
        ctx.request_repaint();
    }
}

fn publish(statuses: &Statuses, ctx: &egui::Context, next: HashMap<Uuid, Status>) {
    let mut statuses = statuses.lock().unwrap_or_else(|error| error.into_inner());
    if *statuses != next {
        *statuses = next;
        ctx.request_repaint();
    }
}

fn publish_chats(
    statuses: &Statuses,
    error: &MonitorError,
    ctx: &egui::Context,
    chats: &HashMap<Uuid, ChatState>,
) {
    let next: HashMap<_, _> = chats
        .iter()
        .map(|(&id, chat)| (id, chat.status()))
        .collect();
    // A missing chat is still waiting for its first snapshot. A tracked chat
    // with Waiting consumed a snapshot or change it could not interpret.
    let incompatible = next.values().any(|&status| status == Status::Waiting);
    publish(statuses, ctx, next);
    report(
        error,
        ctx,
        incompatible.then(|| STATUS_FORMAT_ERROR.to_owned()),
    );
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Envelope<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    method: Option<&'a str>,
    source_client_id: Option<&'a str>,
    request_id: Option<&'a str>,
    result_type: Option<&'a str>,
    result: Option<Registration<'a>>,
    #[serde(borrow)]
    params: Option<&'a RawValue>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Registration<'a> {
    client_id: Option<&'a str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Conversation<'a> {
    conversation_id: &'a str,
    host_id: &'a str,
    #[serde(borrow)]
    change: Option<&'a RawValue>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Change<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    revision: u64,
    base_revision: Option<u64>,
    conversation_state: Option<Metadata>,
    #[serde(borrow)]
    patches: Option<Vec<Patch<'a>>>,
}

#[derive(Deserialize)]
struct Patch<'a> {
    op: &'a str,
    path: Vec<Value>,
    #[serde(borrow, default, deserialize_with = "present_patch_value")]
    value: Option<&'a RawValue>,
}

fn present_patch_value<'de, D: serde::Deserializer<'de>>(
    input: D,
) -> Result<Option<&'de RawValue>, D::Error> {
    <&RawValue>::deserialize(input).map(Some)
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Metadata {
    thread_runtime_status: Option<RuntimeStatus>,
    has_unread_turn: Option<bool>,
    requests: Vec<ServerRequest>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RuntimeStatus {
    #[serde(rename = "type")]
    kind: String,
    active_flags: Vec<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ServerRequest {
    method: String,
}

struct ChatState {
    owner: String,
    snapshot: Option<ChatSnapshot>,
}

struct ChatSnapshot {
    revision: u64,
    metadata: Metadata,
}

impl ChatState {
    fn status(&self) -> Status {
        self.snapshot
            .as_ref()
            .map_or(Status::Waiting, |snapshot| snapshot.metadata.status())
    }
}

impl Metadata {
    fn status(&self) -> Status {
        let runtime = self.thread_runtime_status.as_ref();
        let kind = runtime.map(|runtime| runtime.kind.as_str());
        let flag = |name| {
            runtime.is_some_and(|runtime| runtime.active_flags.iter().any(|flag| flag == name))
        };
        let request = |methods: &[&str]| {
            self.requests
                .iter()
                .any(|request| methods.contains(&request.method.as_str()))
        };
        if kind == Some("systemError") {
            Status::Error
        } else if flag("waitingOnApproval")
            || request(&[
                "item/commandExecution/requestApproval",
                "item/fileChange/requestApproval",
                "item/permissions/requestApproval",
            ])
        {
            Status::Approval
        } else if flag("waitingOnUserInput")
            || request(&[
                "item/tool/requestUserInput",
                "item/tool/requestOptionPicker",
            ])
        {
            Status::Input
        } else if kind == Some("active") {
            Status::Working
        } else {
            match (kind, self.has_unread_turn) {
                (Some("idle" | "notLoaded") | None, Some(true)) => Status::Unread,
                (Some("idle" | "notLoaded"), _) | (None, Some(false)) => Status::Idle,
                _ => Status::Waiting,
            }
        }
    }

    fn apply(&mut self, patch: &Patch<'_>) -> Option<()> {
        let Some((field, rest)) = patch.path.split_first() else {
            *self = patch.parse()?;
            return Some(());
        };
        match field.as_str() {
            Some("threadRuntimeStatus") => {
                if rest.is_empty() {
                    self.thread_runtime_status = patch.optional()?;
                } else {
                    let runtime = self.thread_runtime_status.as_mut()?;
                    match rest[0].as_str() {
                        Some("type") if rest.len() == 1 => runtime.kind = patch.parse()?,
                        Some("type") => return None,
                        Some("activeFlags") => patch.list(&mut runtime.active_flags, &rest[1..])?,
                        _ => {}
                    }
                }
            }
            Some("hasUnreadTurn") => {
                if !rest.is_empty() {
                    return None;
                }
                self.has_unread_turn = patch.optional()?;
            }
            Some("requests") => {
                if rest.get(1).and_then(Value::as_str) == Some("method") {
                    if rest.len() != 2 {
                        return None;
                    }
                    self.requests.get_mut(index(&rest[0])?)?.method = patch.parse()?;
                } else if rest.len() <= 1 {
                    patch.list(&mut self.requests, rest)?;
                }
            }
            _ => {}
        }
        Some(())
    }
}

fn index(value: &Value) -> Option<usize> {
    value.as_u64()?.try_into().ok()
}

impl Patch<'_> {
    fn parse<T: DeserializeOwned>(&self) -> Option<T> {
        if !matches!(self.op, "add" | "replace") {
            return None;
        }
        serde_json::from_str(self.value?.get()).ok()
    }

    fn optional<T: DeserializeOwned>(&self) -> Option<Option<T>> {
        if self.op == "remove" {
            Some(None)
        } else {
            self.parse()
        }
    }

    fn list<T: DeserializeOwned>(&self, list: &mut Vec<T>, path: &[Value]) -> Option<()> {
        match path {
            [] => *list = self.optional()?.unwrap_or_default(),
            [position] if position.as_str() == Some("length") => {
                let length: usize = self.parse()?;
                if length > list.len() {
                    return None;
                }
                list.truncate(length);
            }
            [position] => {
                let index = index(position)?;
                match self.op {
                    "add" if index <= list.len() => list.insert(index, self.parse()?),
                    "replace" => *list.get_mut(index)? = self.parse()?,
                    "remove" if index < list.len() => {
                        list.remove(index);
                    }
                    _ => return None,
                }
            }
            _ => return None,
        }
        Some(())
    }
}

async fn follow_chats(
    watched: &mut watch::Receiver<BTreeSet<Uuid>>,
    statuses: &Statuses,
    error: &MonitorError,
    ctx: &egui::Context,
) -> Result<()> {
    let pipe = ClientOptions::new()
        .open(PIPE)
        .context("Open the Codex desktop app to show live chat status")?;
    let (mut reader, mut writer) = io::split(pipe);
    let request = Uuid::new_v4().to_string();
    send(
        &mut writer,
        json!({
            "type": "request", "requestId": request, "sourceClientId": "deckx",
            "method": "initialize", "version": 0, "params": { "clientType": "deckx" },
        }),
    )
    .await?;
    let mut frame = Vec::new();
    let client = timeout(Duration::from_secs(3), async {
        loop {
            read(&mut reader, &mut frame).await?;
            let Ok(message) = serde_json::from_slice::<Envelope>(&frame) else {
                continue;
            };
            if message.request_id == Some(request.as_str()) && message.kind == "response" {
                ensure!(
                    message.result_type == Some("success"),
                    "Codex IPC registration failed"
                );
                return message
                    .result
                    .and_then(|result| result.client_id)
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow::anyhow!("Missing Codex IPC client ID"));
            }
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("Codex did not answer the live chat status request"))??;
    report(error, ctx, None);
    let mut followed = BTreeSet::new();
    let mut chats = HashMap::new();
    sync_followed(
        &mut writer,
        &client,
        &mut followed,
        watched.borrow_and_update().clone(),
    )
    .await?;
    while !followed.is_empty() {
        // A change in the selected keys must not cancel a partially read IPC frame.
        {
            let reading = read(&mut reader, &mut frame);
            tokio::pin!(reading);
            loop {
                tokio::select! {
                    result = &mut reading => { result?; break; }
                    result = watched.changed() => {
                        if result.is_err() {
                            return Ok(());
                        }
                        let next = watched.borrow_and_update().clone();
                        sync_followed(&mut writer, &client, &mut followed, next).await?;
                        chats.retain(|id, _| followed.contains(id));
                        publish_chats(statuses, error, ctx, &chats);
                        if followed.is_empty() {
                            return Ok(());
                        }
                    }
                }
            }
        }
        on_frame(&frame, &client, &followed, &mut chats, &mut writer).await?;
        publish_chats(statuses, error, ctx, &chats);
    }
    Ok(())
}

async fn sync_followed(
    writer: &mut (impl AsyncWriteExt + Unpin),
    client: &str,
    followed: &mut BTreeSet<Uuid>,
    next: BTreeSet<Uuid>,
) -> Result<()> {
    for &id in followed.difference(&next) {
        subscribe(writer, client, id, false, None).await?;
    }
    for &id in next.difference(followed) {
        subscribe(writer, client, id, true, None).await?;
    }
    *followed = next;
    Ok(())
}

async fn on_frame(
    frame: &[u8],
    client: &str,
    followed: &BTreeSet<Uuid>,
    chats: &mut HashMap<Uuid, ChatState>,
    writer: &mut (impl AsyncWriteExt + Unpin),
) -> Result<()> {
    let Ok(message) = serde_json::from_slice::<Envelope>(frame) else {
        return Ok(());
    };
    if message.kind != "broadcast" {
        return Ok(());
    }
    match message.method {
        Some("ipc-connection-reset") => anyhow::bail!("Codex IPC reset"),
        Some("client-status-changed") => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Client<'a> {
                client_id: &'a str,
                status: &'a str,
            }
            if let Some(params) = message.params
                && let Ok(client) = serde_json::from_str::<Client>(params.get())
                && client.status == "disconnected"
            {
                chats.retain(|_, chat| chat.owner != client.client_id);
            }
        }
        Some(STREAM | "thread-stream-following-status-requested") => {
            let Some((params, owner)) = message.params.zip(message.source_client_id) else {
                return Ok(());
            };
            let Ok(route) = serde_json::from_str::<Conversation>(params.get()) else {
                return Ok(());
            };
            let Ok(id) = Uuid::parse_str(route.conversation_id) else {
                return Ok(());
            };
            if route.host_id != LOCAL || !followed.contains(&id) {
                return Ok(());
            }
            if message.method != Some(STREAM) {
                subscribe(writer, client, id, true, Some(owner)).await?;
            } else {
                update_chat(route.change, owner, id, chats, writer, client).await?;
            }
        }
        _ => {}
    }
    Ok(())
}

async fn update_chat(
    change: Option<&RawValue>,
    owner: &str,
    id: Uuid,
    chats: &mut HashMap<Uuid, ChatState>,
    writer: &mut (impl AsyncWriteExt + Unpin),
    client: &str,
) -> Result<()> {
    // Stream versions can change independently of the status and revision fields.
    let change = change.and_then(|change| serde_json::from_str::<Change>(change.get()).ok());
    match change {
        Some(Change {
            kind: "snapshot",
            revision,
            conversation_state: Some(metadata),
            ..
        }) => {
            chats.insert(
                id,
                ChatState {
                    owner: owner.to_owned(),
                    snapshot: Some(ChatSnapshot { revision, metadata }),
                },
            );
        }
        Some(Change {
            kind: "patches",
            revision,
            base_revision: Some(base_revision),
            patches: Some(patches),
            ..
        }) => {
            let Some(chat) = chats.get_mut(&id).filter(|chat| chat.owner == owner) else {
                return Ok(());
            };
            let Some(snapshot) = chat.snapshot.as_mut().filter(|s| revision > s.revision) else {
                return Ok(());
            };
            if base_revision != snapshot.revision {
                chats.remove(&id);
            } else if patches
                .iter()
                .all(|patch| snapshot.metadata.apply(patch).is_some())
            {
                snapshot.revision = revision;
                return Ok(());
            } else {
                chat.snapshot = None;
            }
            subscribe(writer, client, id, true, Some(owner)).await?;
        }
        _ if chats.get(&id).is_none_or(|chat| chat.owner == owner) => {
            let previous = chats.insert(
                id,
                ChatState {
                    owner: owner.to_owned(),
                    snapshot: None,
                },
            );
            if previous.is_some_and(|chat| chat.snapshot.is_some()) {
                subscribe(writer, client, id, true, Some(owner)).await?;
            }
        }
        _ => {}
    }
    Ok(())
}

async fn subscribe(
    writer: &mut (impl AsyncWriteExt + Unpin),
    client: &str,
    id: Uuid,
    following: bool,
    target: Option<&str>,
) -> Result<()> {
    let mut message = json!({
        "type": "broadcast", "method": FOLLOW, "sourceClientId": client, "version": 1,
        "params": { "conversationId": id.to_string(), "hostId": LOCAL, "following": following },
    });
    if let Some(target) = target {
        message["targetClientIds"] = json!([target]);
    }
    send(writer, message).await
}

async fn send(writer: &mut (impl AsyncWriteExt + Unpin), message: Value) -> Result<()> {
    let body = serde_json::to_vec(&message)?;
    timeout(Duration::from_secs(2), async {
        writer.write_u32_le(body.len() as u32).await?;
        writer.write_all(&body).await
    })
    .await??;
    Ok(())
}

async fn read(reader: &mut (impl AsyncReadExt + Unpin), frame: &mut Vec<u8>) -> Result<()> {
    let length = reader.read_u32_le().await?;
    ensure!(
        length <= 256 * 1024 * 1024,
        "Invalid Codex IPC frame length"
    );
    if length <= MAX_FRAME {
        frame.resize(length as usize, 0);
        reader.read_exact(frame).await?;
    } else {
        frame.clear();
        let copied = io::copy(&mut reader.take(u64::from(length)), &mut io::sink()).await?;
        ensure!(copied == u64::from(length), "Incomplete Codex IPC frame");
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct RecentChat {
    #[serde(deserialize_with = "chat_id")]
    pub id: Uuid,
    pub thread_name: String,
    updated_at: String,
}

fn chat_id<'de, D: serde::Deserializer<'de>>(input: D) -> Result<Uuid, D::Error> {
    let id = String::deserialize(input)?;
    Uuid::parse_str(&id).map_err(serde::de::Error::custom)
}

static CHAT_INDEX: Mutex<ChatIndex> = Mutex::new(ChatIndex {
    file: None,
    chats: Vec::new(),
    error: None,
});

/// Recent local chats, newest first. The session index is parsed again only
/// after its modification time or length changes.
pub fn recent_chats() -> Vec<RecentChat> {
    let mut index = CHAT_INDEX.lock().unwrap_or_else(|error| error.into_inner());
    index.load(session_index().as_deref());
    index.chats.clone()
}

/// The error from the last `recent_chats` call, unless the index is missing.
pub fn recent_chats_error() -> Option<String> {
    CHAT_INDEX
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .error
        .clone()
}

fn session_index() -> Option<PathBuf> {
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|profile| PathBuf::from(profile).join(".codex"))
        })?;
    Some(home.join("session_index.jsonl"))
}

#[derive(Default)]
struct ChatIndex {
    file: Option<(PathBuf, SystemTime, u64)>,
    chats: Vec<RecentChat>,
    error: Option<String>,
}

impl ChatIndex {
    /// Keeps the last chats that were read when the index cannot be read now.
    fn load(&mut self, path: Option<&Path>) {
        let Some(path) = path else {
            *self = Self::default();
            return;
        };
        self.error = self
            .refresh(path)
            .err()
            .map(|error| format!("Could not read recent Codex chats: {error}"));
    }

    fn refresh(&mut self, path: &Path) -> std::io::Result<()> {
        let info = match fs::metadata(path) {
            Ok(info) => info,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                self.file = None;
                self.chats.clear();
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if !info.is_file() {
            return Err(std::io::Error::other(format!(
                "{} is not a file",
                path.display()
            )));
        }
        let file = (path.to_owned(), info.modified()?, info.len());
        if self.file.as_ref() != Some(&file) {
            self.chats = parse_chats(BufReader::new(File::open(path)?))?;
            self.file = Some(file);
        }
        Ok(())
    }
}

fn parse_chats(reader: impl BufRead) -> std::io::Result<Vec<RecentChat>> {
    let mut chats = HashMap::new();
    for line in reader.split(b'\n') {
        if let Ok(chat) = serde_json::from_slice::<RecentChat>(&line?)
            && !chat.thread_name.trim().is_empty()
        {
            chats.insert(chat.id, chat);
        }
    }
    let mut chats: Vec<_> = chats.into_values().collect();
    chats.sort_unstable_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    Ok(chats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index_line(id: u128, name: &str, updated: &str) -> String {
        let chat = json!({
            "id": Uuid::from_u128(id).to_string(), "thread_name": name, "updated_at": updated,
        });
        format!("{chat}\n")
    }

    fn names(index: &ChatIndex) -> Vec<&str> {
        index
            .chats
            .iter()
            .map(|chat| chat.thread_name.as_str())
            .collect()
    }

    #[test]
    fn session_index_is_parsed_again_only_after_it_changes() -> Result<()> {
        let folder = tempfile::tempdir()?;
        let path = folder.path().join("session_index.jsonl");
        let mut index = ChatIndex::default();
        index.load(Some(&path));
        assert!(index.chats.is_empty() && index.error.is_none());

        fs::write(
            &path,
            index_line(1, "First", "2026-11-01") + &index_line(2, "Second", "2026-11-02"),
        )?;
        index.load(Some(&path));
        assert_eq!(names(&index), ["Second", "First"]);

        // Same length and modification time: the memoized chats stay.
        let modified = fs::metadata(&path)?.modified()?;
        fs::write(
            &path,
            index_line(1, "Third", "2026-11-01") + &index_line(2, "Second", "2026-11-02"),
        )?;
        File::options()
            .write(true)
            .open(&path)?
            .set_modified(modified)?;
        index.load(Some(&path));
        assert_eq!(names(&index), ["Second", "First"]);

        let mut file = File::options().append(true).open(&path)?;
        std::io::Write::write_all(&mut file, index_line(3, "Newest", "2026-11-03").as_bytes())?;
        drop(file);
        index.load(Some(&path));
        assert_eq!(names(&index), ["Newest", "Second", "Third"]);
        assert!(index.error.is_none());
        Ok(())
    }

    #[test]
    fn unreadable_session_index_reports_an_error_and_keeps_chats() -> Result<()> {
        let folder = tempfile::tempdir()?;
        let path = folder.path().join("session_index.jsonl");
        fs::write(&path, index_line(1, "Kept", "2026-11-01") + "not json\n")?;
        let mut index = ChatIndex::default();
        index.load(Some(&path));
        assert_eq!(names(&index), ["Kept"]);
        assert!(index.error.is_none());

        index.load(Some(folder.path()));
        assert_eq!(names(&index), ["Kept"]);
        assert!(
            index
                .error
                .as_deref()
                .is_some_and(|error| error.starts_with("Could not read recent Codex chats"))
        );

        index.load(Some(&folder.path().join("missing.jsonl")));
        assert!(index.chats.is_empty() && index.error.is_none());
        Ok(())
    }

    #[test]
    fn classic_tiles_are_solid_micro_colors_even_when_selected() {
        let chat = Agent {
            id: Uuid::nil(),
            title: "This title must stay off the LCD".into(),
        };
        for (status, rgb) in [
            (Status::Waiting, [0, 0, 0]),
            (Status::Idle, [255, 255, 255]),
            (Status::Working, [48, 79, 254]),
            (Status::Approval, [255, 109, 0]),
            (Status::Input, [255, 109, 0]),
            (Status::Unread, [0, 255, 76]),
            (Status::Error, [255, 0, 51]),
        ] {
            let frame = tile(Some(&chat), "", status, true, KeyStyle::Classic);
            assert!(frame.pixels().all(|pixel| pixel.0 == rgb));
        }
    }

    #[tokio::test]
    async fn chat_status_stream_v11_updates_the_agent_light() -> Result<()> {
        assert_chat_status_stream(11).await
    }

    #[tokio::test]
    async fn chat_status_stream_v13_updates_the_agent_light() -> Result<()> {
        assert_chat_status_stream(13).await
    }

    #[tokio::test]
    async fn a_new_stream_version_can_reuse_the_status_format() -> Result<()> {
        assert_chat_status_stream(99).await
    }

    fn status_frame(id: Uuid, version: u32, change: Value) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(&json!({
            "type": "broadcast", "method": STREAM, "sourceClientId": "owner", "version": version,
            "params": {"hostId": LOCAL, "conversationId": id.to_string(), "change": change},
        }))?)
    }

    async fn assert_chat_status_stream(version: u32) -> Result<()> {
        let id = Uuid::new_v4();
        let followed = BTreeSet::from([id]);
        let mut chats = HashMap::new();
        let mut writer = io::sink();
        let push = |change| status_frame(id, version, change);
        let snapshot = push(json!({
            "type": "snapshot", "revision": 1,
            "conversationState": {
                "threadRuntimeStatus": {"type": "active", "activeFlags": []},
                "extension": {"counter": 0}
            },
        }))?;
        on_frame(&snapshot, "deckx", &followed, &mut chats, &mut writer).await?;
        assert_eq!(chats[&id].status(), Status::Working);

        let approval = push(json!({
            "type": "patches", "baseRevision": 1, "revision": 2, "patches": [
                {"op": "add", "path": ["requests", 0],
                 "value": {"method": "item/commandExecution/requestApproval", "params": {"ignored": true}}},
                {"op": "replace", "path": ["extension", "counter"], "value": 1}
            ],
        }))?;
        on_frame(&approval, "deckx", &followed, &mut chats, &mut writer).await?;
        assert_eq!(chats[&id].status(), Status::Approval);

        let complete = push(json!({
            "type": "patches", "baseRevision": 2, "revision": 3, "patches": [
                {"op": "remove", "path": ["requests", 0]},
                {"op": "replace", "path": ["threadRuntimeStatus", "type"], "value": "idle"},
                {"op": "replace", "path": ["hasUnreadTurn"], "value": true}
            ],
        }))?;
        on_frame(&complete, "deckx", &followed, &mut chats, &mut writer).await?;
        assert_eq!(chats[&id].status(), Status::Unread);

        let gap =
            push(json!({"type": "patches", "baseRevision": 4, "revision": 5, "patches": []}))?;
        on_frame(&gap, "deckx", &followed, &mut chats, &mut writer).await?;
        assert!(!chats.contains_key(&id));
        Ok(())
    }

    #[tokio::test]
    async fn unreadable_status_updates_only_invalidate_their_chat() -> Result<()> {
        let id = Uuid::new_v4();
        let other = Uuid::new_v4();
        let followed = BTreeSet::from([id, other]);
        let mut chats = HashMap::new();
        let mut writer = Vec::new();
        let statuses = Statuses::default();
        let error = MonitorError::default();
        let ctx = egui::Context::default();
        let working = json!({"type": "snapshot", "revision": 1, "conversationState": {
            "threadRuntimeStatus": {"type": "active"}, "hasUnreadTurn": false
        }});
        for id in [id, other] {
            let frame = status_frame(id, 99, working.clone())?;
            on_frame(&frame, "deckx", &followed, &mut chats, &mut writer).await?;
        }
        publish_chats(&statuses, &error, &ctx, &chats);
        assert!(error.lock().unwrap().is_none());

        let broken_patch = status_frame(
            id,
            99,
            json!({
                "type": "patches", "baseRevision": 1, "revision": 2, "patches": [
                    {"op": "replace", "path": ["hasUnreadTurn"]}
                ]
            }),
        )?;
        on_frame(&broken_patch, "deckx", &followed, &mut chats, &mut writer).await?;
        publish_chats(&statuses, &error, &ctx, &chats);
        assert_eq!(statuses.lock().unwrap()[&id], Status::Waiting);
        assert_eq!(statuses.lock().unwrap()[&other], Status::Working);
        assert_eq!(error.lock().unwrap().as_deref(), Some(STATUS_FORMAT_ERROR));
        let resubscribe = writer.len();
        assert!(resubscribe > 0);
        on_frame(&broken_patch, "deckx", &followed, &mut chats, &mut writer).await?;
        assert_eq!(writer.len(), resubscribe);

        for state in [
            json!({"newStatusField": "active"}),
            json!({"threadRuntimeStatus": {"type": "newState"}, "hasUnreadTurn": false}),
        ] {
            let snapshot = status_frame(
                id,
                99,
                json!({"type": "snapshot", "revision": 3, "conversationState": state}),
            )?;
            on_frame(&snapshot, "deckx", &followed, &mut chats, &mut writer).await?;
            assert_eq!(chats[&id].status(), Status::Waiting);
        }

        let recovered = status_frame(
            id,
            99,
            json!({
                "type": "patches", "baseRevision": 3, "revision": 4, "patches": [
                    {"op": "replace", "path": ["threadRuntimeStatus"], "value": {"type": "idle"}},
                    {"op": "replace", "path": ["hasUnreadTurn"], "value": null}
                ]
            }),
        )?;
        on_frame(&recovered, "deckx", &followed, &mut chats, &mut writer).await?;
        publish_chats(&statuses, &error, &ctx, &chats);
        assert_eq!(statuses.lock().unwrap()[&id], Status::Idle);
        assert_eq!(statuses.lock().unwrap()[&other], Status::Working);
        assert!(error.lock().unwrap().is_none());
        Ok(())
    }
}
