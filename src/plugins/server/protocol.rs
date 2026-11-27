//! Registration and the events plugins and property inspectors send.

use std::time::{Duration, Instant};

use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{DEVICE_ID, Hub, Identity, Instance, Peer, appearance::Change, device_info};

const OPEN_URL_INTERVAL: Duration = Duration::from_secs(1);

/// An event from a plugin or inspector. Unknown events and events with missing or mistyped
/// fields are ignored.
#[derive(Deserialize)]
#[serde(tag = "event", rename_all = "camelCase")]
enum Incoming {
    GetGlobalSettings {
        #[serde(default, deserialize_with = "present")]
        id: Option<Value>,
    },
    SetGlobalSettings {
        #[serde(default)]
        payload: Value,
    },
    OpenUrl {
        #[serde(default)]
        payload: Value,
    },
    GetSettings(Binding),
    SetSettings(Binding),
    SendToPlugin(Binding),
    SendToPropertyInspector(Binding),
    GetResources(Binding),
    SetTitle(Binding),
    SetImage(Binding),
    SetState(Binding),
    ShowOk(Binding),
    ShowAlert(Binding),
    #[serde(other)]
    Other,
}

/// The fields of an event addressed to one binding.
#[derive(Deserialize)]
struct Binding {
    context: String,
    #[serde(default, deserialize_with = "present")]
    id: Option<Value>,
    #[serde(default)]
    payload: Value,
}

impl Incoming {
    fn binding(&self) -> Option<&Binding> {
        match self {
            Self::GetSettings(binding)
            | Self::SetSettings(binding)
            | Self::SendToPlugin(binding)
            | Self::SendToPropertyInspector(binding)
            | Self::GetResources(binding)
            | Self::SetTitle(binding)
            | Self::SetImage(binding)
            | Self::SetState(binding)
            | Self::ShowOk(binding)
            | Self::ShowAlert(binding) => Some(binding),
            Self::GetGlobalSettings { .. }
            | Self::SetGlobalSettings { .. }
            | Self::OpenUrl { .. }
            | Self::Other => None,
        }
    }
}

/// Keeps a request `id` that is present, including `null`, so replies echo it.
fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

fn reply(mut response: Value, id: &Option<Value>) -> Value {
    if let Some(id) = id {
        response["id"] = id.clone();
    }
    response
}

impl Hub {
    pub(in crate::plugins) fn authenticate(
        &mut self,
        uuid: &str,
        event: &str,
        tx: mpsc::Sender<Value>,
    ) -> bool {
        let identity = self
            .tokens
            .get(uuid)
            .map(|owner| Identity::Plugin(owner.clone()))
            .or_else(|| self.panels.get(uuid).map(|panel| panel.identity.clone()));
        let Some(identity) = identity else {
            return false;
        };
        if identity.event() != event || self.peers.contains_key(uuid) {
            return false;
        }
        self.peers.insert(
            uuid.to_owned(),
            Peer {
                identity: identity.clone(),
                tx: tx.clone(),
            },
        );
        match &identity {
            Identity::Plugin(owner) => {
                if self.connected {
                    let _ = tx.try_send(json!({"event":"deviceDidConnect","device":DEVICE_ID,"deviceInfo":device_info()}));
                }
                for instance in self.visible.values().filter(|instance| {
                    self.lookup(&instance.action)
                        .is_some_and(|(id, _)| id == owner)
                }) {
                    let _ = tx.try_send(self.packet(instance, "willAppear", json!({})));
                }
                for peer in self.peers.values() {
                    if let Identity::Inspector { plugin, instance } = &peer.identity
                        && plugin == owner
                    {
                        let _ = tx.try_send(self.packet(
                            instance,
                            "propertyInspectorDidAppear",
                            json!({}),
                        ));
                    }
                }
            }
            Identity::Inspector { instance, .. } => {
                self.broadcast_instance(instance, "propertyInspectorDidAppear", json!({}));
            }
        }
        true
    }

    pub(super) fn disconnected(&mut self, uuid: &str) {
        if let Some(Peer {
            identity: Identity::Inspector { instance, .. },
            ..
        }) = self.peers.remove(uuid)
        {
            self.broadcast_instance(&instance, "propertyInspectorDidDisappear", json!({}));
        }
    }

    /// Handles one message from a registered peer and returns a URL for the caller to open.
    pub(in crate::plugins) fn handle(&mut self, key: &str, message: &Value) -> Option<String> {
        let peer = self.peers.get(key)?;
        let identity = peer.identity.clone();
        let tx = peer.tx.clone();
        let owner = identity.plugin();
        let incoming = Incoming::deserialize(message).ok()?;
        match &incoming {
            Incoming::GetGlobalSettings { id } => {
                let response = json!({"event":"didReceiveGlobalSettings","payload":{"settings":self.global(owner)}});
                let _ = tx.try_send(reply(response, id));
                return None;
            }
            Incoming::SetGlobalSettings { payload } => {
                self.set_global(key, owner, payload);
                return None;
            }
            Incoming::OpenUrl { payload } => return self.open_url(owner, payload),
            _ => {}
        }
        let Binding {
            context,
            id,
            payload,
        } = incoming.binding()?;
        let instance = self.addressable(&identity, context)?;
        let (actual_owner, action) = self.lookup(&instance.action)?;
        if actual_owner != owner {
            return None;
        }
        let states = action.states.len();
        let plugin = matches!(identity, Identity::Plugin(_));
        let change = match &incoming {
            Incoming::GetSettings(_) => {
                let response = self.packet(&instance, "didReceiveSettings", json!({}));
                let _ = tx.try_send(reply(response, id));
                return None;
            }
            Incoming::SetSettings(_) => {
                if self.set_settings(owner, context, payload) {
                    let packet = self.packet(&instance, "didReceiveSettings", json!({}));
                    if plugin {
                        self.to_inspector(owner, context, &packet);
                    } else {
                        self.to_plugin(owner, &packet);
                    }
                }
                return None;
            }
            Incoming::SendToPlugin(_) if !plugin => {
                self.to_plugin(owner, &json!({"event":"sendToPlugin","action":instance.action,"context":context,"device":DEVICE_ID,"payload":payload}));
                return None;
            }
            Incoming::SendToPropertyInspector(_) if plugin => {
                self.to_inspector(owner, context, &json!({"event":"sendToPropertyInspector","action":instance.action,"context":context,"payload":payload}));
                return None;
            }
            Incoming::GetResources(_) => {
                let packet = self.packet(&instance, "didReceiveResources", json!({}));
                let _ = tx.try_send(reply(packet, id));
                return None;
            }
            Incoming::SetTitle(_) if plugin => Change::Title,
            Incoming::SetImage(_) if plugin => Change::Image,
            Incoming::SetState(_) if plugin => Change::State,
            Incoming::ShowOk(_) if plugin => Change::Ok,
            Incoming::ShowAlert(_) if plugin => Change::Alert,
            _ => return None,
        };
        self.change(context, change, payload, states);
        None
    }

    /// The binding a peer may address: an inspector's own binding, or a visible or inspected
    /// binding for a plugin.
    fn addressable(&self, identity: &Identity, context: &str) -> Option<Instance> {
        match identity {
            Identity::Inspector { instance, .. } => {
                (instance.context == context).then(|| instance.clone())
            }
            Identity::Plugin(owner) => self.visible.get(context).cloned().or_else(|| {
                self.panels
                    .values()
                    .find_map(|panel| match &panel.identity {
                        Identity::Inspector { plugin, instance }
                            if plugin == owner && instance.context == context =>
                        {
                            Some(instance.clone())
                        }
                        _ => None,
                    })
            }),
        }
    }

    fn open_url(&mut self, owner: &str, payload: &Value) -> Option<String> {
        let url = payload["url"].as_str()?;
        let now = Instant::now();
        if self
            .opened
            .get(owner)
            .is_some_and(|at| now.duration_since(*at) < OPEN_URL_INTERVAL)
        {
            return None;
        }
        self.opened.insert(owner.to_owned(), now);
        Some(url.to_owned())
    }
}
