use std::{num::NonZeroU64, path::Path};

use serde::{Deserialize, Deserializer};

use super::roster;
use crate::usage::{Reading, Window, read_json};

const SUFFIX: &str = ".weekly-usage.cache";
const MAX_CACHE: u64 = 64 * 1024;
const MAX_AGE: u64 = 24 * 60 * 60 * 1000;
const WEEK: u32 = 7 * 24 * 60;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Cache {
    schema_version: u32,
    value: Snapshot,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    kind: String,
    expires_at_ms: u64,
    selected_team_id: Team,
    reading: Option<Sample>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Sample {
    read_at_ms: u64,
    usage: Usage,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Usage {
    percent_used: f64,
    next_reset_ms: Option<u64>,
    has_non_zero_included_limit: bool,
    #[serde(default, deserialize_with = "reported_team")]
    read_for_team_id: Option<Team>,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(untagged)]
enum Team {
    Selected(NonZeroU64),
    Personal(()),
}

// A missing field inherits the snapshot's scope. An explicit null names the personal scope.
fn reported_team<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Team>, D::Error> {
    Team::deserialize(deserializer).map(Some)
}

pub(crate) fn read(now: u64) -> Option<Reading> {
    read_in(&roster::cache_dir()?, now)
}

fn read_in(dir: &Path, now: u64) -> Option<Reading> {
    let (latest, _, _) = roster::latest_file(dir)?;
    let key = roster::cache_key(&latest)?;
    let scope = key.strip_suffix(roster::ROSTER_SUFFIX)?;
    let file_team = match scope.rsplit_once(".team.") {
        Some((_, id)) => Some(Team::Selected(id.parse().ok()?)),
        None => None,
    };
    let usage_key = format!("{scope}{SUFFIX}");
    let stem = data_encoding::BASE32_NOPAD
        .encode(usage_key.as_bytes())
        .to_ascii_lowercase();
    let path = dir.join(format!("{stem}.blob"));
    let cache: Cache = read_json(&path, MAX_CACHE)?;
    let value = cache.value;
    let reading = value.reading?;
    let usage = reading.usage;
    if cache.schema_version != 3
        || value.kind != "present"
        || !usage.has_non_zero_included_limit
        || file_team.is_some_and(|team| team != value.selected_team_id)
        || usage
            .read_for_team_id
            .is_some_and(|team| team != value.selected_team_id)
    {
        return None;
    }
    let window = Window::from_used(usage.percent_used, Some(WEEK), usage.next_reset_ms)?;
    Reading {
        windows: [Some(window), None],
        observed_at: reading.read_at_ms,
        expires_at: value
            .expires_at_ms
            .min(reading.read_at_ms.saturating_add(MAX_AGE)),
    }
    .current(now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_weekly_cache_follows_the_roster_scope_and_expires_at_reset() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let path = |key: &str| {
            folder.path().join(format!(
                "{}.blob",
                data_encoding::BASE32_NOPAD.encode(key.as_bytes())
            ))
        };
        std::fs::write(
            path("sand.client.slice.account.one.roster.last-roster"),
            "{}",
        )?;
        let cache = r#"{"schemaVersion":3,"value":{"kind":"present","selectedTeamId":null,"expiresAtMs":5000,"reading":{"readAtMs":1000,"usage":{"percentUsed":12.5,"nextResetMs":4500,"hasNonZeroIncludedLimit":true}}}}"#;
        std::fs::write(
            path("sand.client.slice.account.two.weekly-usage.cache"),
            cache,
        )?;
        assert!(read_in(folder.path(), 2000).is_none());
        std::fs::write(
            path("sand.client.slice.account.one.weekly-usage.cache"),
            cache,
        )?;
        let reading = read_in(folder.path(), 2000).unwrap();
        assert_eq!(reading.windows[0].unwrap().remaining, 87);
        assert!(read_in(folder.path(), 4500).is_none());
        Ok(())
    }

    #[test]
    fn matching_team_usage_loads_and_a_mismatched_window_is_rejected() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        for scope in [
            "sand.client.slice.account.one",
            "sand.client.slice.account.one.team.42",
        ] {
            let dir = folder.path().join(scope);
            std::fs::create_dir(&dir)?;
            let path = |suffix: &str| {
                let key = format!("{scope}.{suffix}");
                let stem = data_encoding::BASE32_NOPAD
                    .encode(key.as_bytes())
                    .to_ascii_lowercase();
                dir.join(format!("{stem}.blob"))
            };
            std::fs::write(path("roster.last-roster"), "{}")?;
            let usage_path = path("weekly-usage.cache");
            let mut cache = serde_json::json!({
                "schemaVersion": 3,
                "value": {
                    "kind": "present",
                    "selectedTeamId": 42,
                    "expiresAtMs": 5000,
                    "reading": {"readAtMs": 1000, "usage": {
                        "percentUsed": 29.0,
                        "nextResetMs": 4500,
                        "hasNonZeroIncludedLimit": true,
                        "readForTeamId": 42
                    }}
                }
            });
            std::fs::write(&usage_path, serde_json::to_vec(&cache)?)?;
            let reading = read_in(&dir, 2000).unwrap();
            assert_eq!(reading.windows[0].unwrap().remaining, 71);

            for team in [serde_json::json!(43), serde_json::Value::Null] {
                cache["value"]["reading"]["usage"]["readForTeamId"] = team;
                std::fs::write(&usage_path, serde_json::to_vec(&cache)?)?;
                assert!(read_in(&dir, 2000).is_none());
            }
        }
        Ok(())
    }
}
