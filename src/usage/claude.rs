use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{MINUTE, Reading, WEEK, Window, read_json};

const MAX_HISTORY: u64 = 4 * 1024 * 1024;
// Claude Desktop retains thirty days of history. The tile identifies older samples as last seen.
const MAX_AGE: u64 = 30 * 24 * 60 * MINUTE;

#[derive(Deserialize)]
struct History {
    version: u8,
    samples: Vec<Sample>,
}

#[derive(Deserialize)]
struct Sample {
    t: u64,
    u: Option<Limits>,
    #[serde(flatten)]
    legacy: Limits,
}

#[derive(Deserialize)]
struct Limits {
    fh: Option<f64>,
    sd: Option<f64>,
}

pub(super) fn read(now: u64) -> Option<Reading> {
    let dir = PathBuf::from(std::env::var_os("APPDATA")?);
    read_history(&dir.join("Claude/plan-usage-history.json"), now)
}

fn read_history(path: &Path, now: u64) -> Option<Reading> {
    let history: History = read_json(path, MAX_HISTORY)?;
    let newest = history.samples.into_iter().max_by_key(|sample| sample.t)?;
    let limits = match history.version {
        1 => newest.legacy,
        2 => newest.u?,
        _ => return None,
    };
    let windows = [
        limits
            .fh
            .and_then(|used| Window::from_used(used, Some(300), None)),
        limits
            .sd
            .and_then(|used| Window::from_used(used, Some(WEEK), None)),
    ];
    Reading {
        windows,
        observed_at: newest.t,
        expires_at: newest.t.saturating_add(MAX_AGE),
    }
    .current(now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_uses_the_latest_sample_and_discards_expired_data() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let path = folder.path().join("plan-usage-history.json");
        std::fs::write(
            &path,
            br#"{"version":2,"samples":[{"t":2000,"org":"two","u":{"sd":64}},{"t":1000,"org":"one","u":{"fh":30,"sd":47}}]}"#,
        )?;
        let reading = read_history(&path, 3000).unwrap();
        assert_eq!(reading.observed_at, 2000);
        assert_eq!(reading.windows[0], None);
        assert_eq!(reading.windows[1].unwrap().remaining, 36);
        let older = read_history(&path, 2000 + 61 * MINUTE).unwrap();
        assert_eq!(older, reading);
        assert!(older.is_cached(2000 + 61 * MINUTE));
        assert!(read_history(&path, 2000 + MAX_AGE).is_none());
        Ok(())
    }
}
