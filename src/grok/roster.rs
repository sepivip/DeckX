use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use super::{Bot, Catalog, automation, window};
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::Value;
use uiautomation::types::{ControlType, ElementMode, TreeScope, UIProperty};

type FileId = (PathBuf, SystemTime, u64);

#[derive(Default)]
pub(super) struct Roster {
    loaded: Option<FileId>,
    /// The roster file that the sidebar was last checked against.
    probed: Option<FileId>,
    bots: Arc<[Bot]>,
    main_bots: Option<BTreeMap<String, String>>,
}

const SCHEMA_VERSION: u32 = 4;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RosterFile {
    schema_version: u32,
    #[serde(default)]
    value: Value,
}

/// Reads the bots in Grok's roster cache. A row that DeckX cannot read is
/// skipped so one unexpected bot does not hide the others.
fn parse_roster(bytes: &[u8]) -> Result<Vec<Bot>> {
    let mut roster: RosterFile =
        serde_json::from_slice(bytes).context("Could not read the Grok Bot roster cache")?;
    ensure!(
        roster.schema_version <= SCHEMA_VERSION,
        "This Grok Bot version is newer than DeckX supports. Update DeckX to load your bots"
    );
    ensure!(
        roster.schema_version == SCHEMA_VERSION,
        "This Grok Bot version is older than DeckX supports. Update Grok Bot to load your bots"
    );
    let Some(Value::Array(rows)) = roster.value.get_mut("rows").map(Value::take) else {
        bail!("Could not read the Grok Bot roster cache");
    };
    let total = rows.len();
    let bots: Vec<Bot> = rows
        .into_iter()
        .filter_map(|row| serde_json::from_value::<Bot>(row).ok())
        .filter(|bot| !bot.id.is_empty())
        .map(|mut bot| {
            if bot.name.trim().is_empty() {
                bot.name.clone_from(if bot.title.trim().is_empty() {
                    &bot.id
                } else {
                    &bot.title
                });
            }
            bot
        })
        .collect();
    ensure!(
        total == 0 || !bots.is_empty(),
        "Could not read any bots from the Grok Bot roster cache"
    );
    Ok(bots
        .into_iter()
        .filter(|bot| !bot.is_hidden_from_sidebar && !bot.is_removed_from_sidebar)
        .collect())
}

pub(super) const ROSTER_SUFFIX: &str = ".roster.last-roster";

pub(super) fn cache_dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("Grok Bot/sand-client-persistence"))
}

pub(super) fn cache_key(path: &Path) -> Option<String> {
    if path.extension()?.to_str()? != "blob" {
        return None;
    }
    let stem = path.file_stem()?.to_str()?.to_ascii_uppercase();
    let bytes = data_encoding::BASE32_NOPAD.decode(stem.as_bytes()).ok()?;
    let key = String::from_utf8(bytes).ok()?;
    key.starts_with("sand.client.slice.").then_some(key)
}

pub(super) fn is_roster_file(path: &Path) -> bool {
    cache_key(path).is_some_and(|key| key.ends_with(ROSTER_SUFFIX))
}

pub(super) fn latest_file(dir: &Path) -> Option<FileId> {
    fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if !is_roster_file(&path) {
                return None;
            }
            let info = entry.metadata().ok()?;
            info.is_file()
                .then_some((path, info.modified().ok()?, info.len()))
        })
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)))
}

impl Roster {
    pub fn refresh(&mut self) -> Result<Catalog> {
        let dir = cache_dir().context("APPDATA is unavailable")?;
        let latest = latest_file(&dir).context("Open Grok Bot to load your bot list")?;
        let account = latest
            .0
            .file_stem()
            .and_then(|stem| stem.to_str())
            .context("Invalid Grok roster filename")?
            .to_owned();
        if self.loaded.as_ref() != Some(&latest) {
            ensure!(
                latest.2 <= 8 * 1024 * 1024,
                "Grok's roster cache is too large"
            );
            self.bots = parse_roster(&fs::read(&latest.0)?)?.into();
            self.loaded = Some(latest);
        }

        let cache_path = crate::profile::path()
            .ok()
            .map(|path| path.with_file_name("grok-main-bots.json"));
        let main_bots = self.main_bots.get_or_insert_with(|| {
            cache_path
                .as_ref()
                .and_then(|path| fs::read(path).ok())
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default()
        });
        // Reading the sidebar is slow, so check it only when the main bot is
        // unknown or the roster changed since the last check.
        let probe =
            !main_bots.contains_key(&account) || self.probed.as_ref() != self.loaded.as_ref();
        if probe && !self.bots.is_empty() {
            self.probed.clone_from(&self.loaded);
            if let Ok(Some(label)) = main_bot_label()
                && let Some(id) = unique_bot_id(&self.bots, &label)
                && main_bots.get(&account).is_none_or(|cached| cached != id)
            {
                main_bots.insert(account.clone(), id.to_owned());
                if let Some(path) = cache_path {
                    let _ = crate::profile::save_json(&path, main_bots);
                }
            }
        }
        Ok(Catalog {
            bots: Arc::clone(&self.bots),
            main_id: main_bots.get(&account).cloned(),
        })
    }
}

// Grok obtains the main agent separately from its persisted roster. Its sidebar
// exposes the same main-agent state through the bot row's accessible label.
fn main_bot_label() -> Result<Option<String>> {
    automation::run(|automation| {
        let window = window(automation)?;
        let list_name = automation
            .create_property_condition(UIProperty::Name, "Bot list".into(), None)
            .context("Could not create the bot list search")?;
        let list = window
            .find_first(TreeScope::Descendants, &list_name)
            .context("Grok Bot sidebar is unavailable")?;
        let button = automation
            .create_property_condition(
                UIProperty::ControlType,
                (ControlType::Button as i32).into(),
                None,
            )
            .context("Could not create the bot row search")?;
        // Fetch every row name in the same cross-process call as the search.
        let names = automation
            .create_cache_request()
            .context("Could not create the bot name request")?;
        names.add_property(UIProperty::Name)?;
        names.set_element_mode(ElementMode::None)?;
        let mut labels = list
            .find_all_build_cache(TreeScope::Descendants, &button, &names)
            .context("Could not read the accessible bot rows")?
            .into_iter()
            .filter_map(|row| {
                let label = row.get_cached_name().ok()?;
                let (name, suffix) = label.rsplit_once(", Main Bot")?;
                (suffix.is_empty() || suffix.starts_with(", ")).then(|| name.to_owned())
            });
        Ok(labels.next().filter(|_| labels.next().is_none()))
    })
}

fn unique_bot_id<'a>(bots: &'a [Bot], label: &str) -> Option<&'a str> {
    let normalize = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let label = normalize(label);
    let mut matches = bots
        .iter()
        .filter(|bot| normalize(bot.label()) == label || normalize(&bot.name) == label);
    let bot = matches.next()?;
    matches.next().is_none().then_some(bot.id.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster(version: u32, rows: Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schemaVersion": version,
            "value": {"rows": rows, "cursor": null},
        }))
        .unwrap()
    }

    #[test]
    fn unreadable_rows_are_skipped_and_names_fall_back() {
        let bots = parse_roster(&roster(
            SCHEMA_VERSION,
            serde_json::json!([
                {"id": "named", "name": "Named", "title": "Shown title"},
                {"id": "titled", "title": "Only a title"},
                {"id": "bare"},
                {"name": "No ID"},
                {"id": 7, "name": "Numeric ID"},
                {"id": "dated", "name": "Bad date", "createdAt": "yesterday"},
                {"id": "hidden", "name": "Hidden", "isHiddenFromSidebar": true},
                {"id": "", "name": "Empty ID"},
                "not a bot",
            ]),
        ))
        .unwrap();
        let read: Vec<_> = bots
            .iter()
            .map(|bot| (bot.id.as_str(), bot.name.as_str(), bot.label()))
            .collect();
        assert_eq!(
            read,
            [
                ("named", "Named", "Shown title"),
                ("titled", "Only a title", "Only a title"),
                ("bare", "bare", "bare"),
            ]
        );
    }

    #[test]
    fn other_roster_versions_name_the_out_of_date_app() {
        let newer = parse_roster(&roster(SCHEMA_VERSION + 1, serde_json::json!([]))).unwrap_err();
        assert!(newer.to_string().contains("newer than DeckX supports"));
        let older = parse_roster(&roster(SCHEMA_VERSION - 1, serde_json::json!([]))).unwrap_err();
        assert!(older.to_string().contains("older than DeckX supports"));
        // A newer cache can change shape entirely and still report its version.
        let reshaped = serde_json::json!({"schemaVersion": SCHEMA_VERSION + 1, "value": "v5"});
        let error = parse_roster(&serde_json::to_vec(&reshaped).unwrap()).unwrap_err();
        assert!(error.to_string().contains("newer than DeckX supports"));
    }

    #[test]
    fn a_roster_without_readable_bots_is_an_error() {
        assert!(
            parse_roster(&roster(SCHEMA_VERSION, serde_json::json!([])))
                .is_ok_and(|bots| bots.is_empty())
        );
        assert!(
            parse_roster(&roster(SCHEMA_VERSION, serde_json::json!([{"title": "x"}]))).is_err()
        );
    }

    #[test]
    #[ignore = "requires Grok Bot's sidebar and a locally cached roster"]
    fn installed_grok_main_bot_maps_to_the_first_key() {
        let label = main_bot_label()
            .unwrap()
            .expect("Grok's sidebar did not expose a main bot");
        let mut roster = Roster::default();
        let catalog = roster.refresh().unwrap();
        let layout = super::super::Layout::resolve(&catalog, &super::super::Bindings::default());
        let expected = unique_bot_id(&roster.bots, &label).expect("Could not match the main bot");
        assert_eq!(layout.bot(0).map(|bot| bot.id.as_str()), Some(expected));
    }
}
