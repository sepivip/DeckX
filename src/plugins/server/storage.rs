//! Plugin settings and their debounced writer.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::{Hub, Instance, Shared, lock};

/// Settings changes are written together after this pause.
const SAVE_DELAY: Duration = Duration::from_millis(400);

#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct Data {
    global: HashMap<String, Value>,
    settings: HashMap<String, HashMap<String, Value>>,
}

pub(super) async fn write_settings((hub, written): (Shared, Arc<Mutex<u64>>), saves: Arc<Notify>) {
    loop {
        saves.notified().await;
        tokio::time::sleep(SAVE_DELAY).await;
        let (hub, written) = (hub.clone(), written.clone());
        let _ = tokio::task::spawn_blocking(move || flush(&hub, &written)).await;
    }
}

/// Saves the newest settings outside the hub lock. Writers take `written` first, so a slower
/// write never replaces a newer one.
pub(super) fn flush(hub: &Shared, written: &Mutex<u64>) -> Result<()> {
    let mut written = written.lock().unwrap_or_else(PoisonError::into_inner);
    let (revision, data, path) = {
        let hub = lock(hub);
        if hub.revision == *written {
            return Ok(());
        }
        (hub.revision, hub.data.clone(), hub.data_path.clone())
    };
    let result = crate::profile::save_json(&path, &*data).context("Could not save plugin settings");
    let mut hub = lock(hub);
    match &result {
        Ok(()) => {
            *written = revision;
            hub.save_error = None;
        }
        Err(error) => hub.save_error = Some(format!("{error:#}")),
    }
    result
}

impl Hub {
    /// Drops an uninstalled plugin's settings.
    pub(in crate::plugins) fn forget(&mut self, plugin: &str) {
        if self.data.global.contains_key(plugin) || self.data.settings.contains_key(plugin) {
            let data = Arc::make_mut(&mut self.data);
            data.global.remove(plugin);
            data.settings.remove(plugin);
            self.changed();
        }
    }

    /// Copies a binding's settings, whether or not its plugin is running.
    pub fn duplicate_settings(&mut self, action: &str, from: &str, to: &str) {
        let Some(owner) = self.owners.get(action).cloned() else {
            return;
        };
        let Some(value) = self
            .data
            .settings
            .get(&owner)
            .and_then(|settings| settings.get(from))
            .cloned()
        else {
            return;
        };
        Arc::make_mut(&mut self.data)
            .settings
            .entry(owner)
            .or_default()
            .insert(to.to_owned(), value);
        self.changed();
    }

    pub(super) fn settings(&self, owner: &str, context: &str) -> Value {
        self.data
            .settings
            .get(owner)
            .and_then(|settings| settings.get(context))
            .cloned()
            .unwrap_or_else(|| json!({}))
    }

    pub(super) fn global(&self, owner: &str) -> Value {
        self.data
            .global
            .get(owner)
            .cloned()
            .unwrap_or_else(|| json!({}))
    }

    /// Reads or replaces settings. The plugin host checks that `instance` belongs to `owner`
    /// first, so this is not reachable through `PluginHost::hub`.
    pub(in crate::plugins) fn configure(
        &mut self,
        owner: &str,
        instance: Option<&Instance>,
        settings: Option<Value>,
    ) -> Result<Value> {
        if let Some(value) = settings {
            ensure!(
                value.is_object() && small_json(&value),
                "Plugin settings must be a JSON object up to 256 KiB"
            );
            let data = Arc::make_mut(&mut self.data);
            if let Some(instance) = instance {
                data.settings
                    .entry(owner.to_owned())
                    .or_default()
                    .insert(instance.context.clone(), value.clone());
            } else {
                data.global.insert(owner.to_owned(), value.clone());
            }
            self.changed();
            if let Some(instance) = instance {
                let packet = self.packet(
                    instance,
                    "didReceiveSettings",
                    json!({"settings":self.settings(owner, &instance.context)}),
                );
                self.to_plugin(owner, &packet);
                self.to_inspector(owner, &instance.context, &packet);
            } else {
                let packet =
                    json!({"event":"didReceiveGlobalSettings","payload":{"settings":value}});
                for peer in self
                    .peers
                    .values()
                    .filter(|peer| peer.identity.plugin() == owner)
                {
                    let _ = peer.tx.try_send(packet.clone());
                }
            }
        }
        Ok(instance.map_or_else(
            || self.global(owner),
            |instance| self.settings(owner, &instance.context),
        ))
    }

    /// Stores a plugin's global settings from `sender` and passes them to its other peers.
    pub(super) fn set_global(&mut self, sender: &str, owner: &str, payload: &Value) {
        if !(payload.is_object() && small_json(payload)) {
            return;
        }
        Arc::make_mut(&mut self.data)
            .global
            .insert(owner.to_owned(), payload.clone());
        self.changed();
        let packet = json!({"event":"didReceiveGlobalSettings","payload":{"settings":payload}});
        for (other, peer) in &self.peers {
            if other != sender && peer.identity.plugin() == owner {
                let _ = peer.tx.try_send(packet.clone());
            }
        }
    }

    /// Stores a binding's settings unless they are not a small JSON object.
    pub(super) fn set_settings(&mut self, owner: &str, context: &str, payload: &Value) -> bool {
        if !(payload.is_object() && small_json(payload)) {
            return false;
        }
        Arc::make_mut(&mut self.data)
            .settings
            .entry(owner.to_owned())
            .or_default()
            .insert(context.to_owned(), payload.clone());
        self.changed();
        true
    }

    /// Records a settings change for the writer task.
    fn changed(&mut self) {
        self.revision += 1;
        self.saves.notify_one();
    }
}

fn small_json(value: &Value) -> bool {
    serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= 256 * 1024)
}
