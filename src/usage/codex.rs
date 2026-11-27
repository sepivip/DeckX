use std::{process::Stdio, time::Duration};

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
    time::timeout,
};

use super::{MINUTE, Reading, Window, now_ms};

const DEADLINE: Duration = Duration::from_secs(8);
const MAX_REPLY: u64 = 256 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountLimits {
    rate_limits: Limits,
}

#[derive(Deserialize)]
struct Limits {
    primary: Option<Limit>,
    secondary: Option<Limit>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Limit {
    used_percent: f64,
    window_duration_mins: Option<u32>,
    resets_at: Option<u64>,
}

pub(super) async fn read() -> Option<Reading> {
    let command = crate::mcp::codex_setup::cli_command().ok()?;
    let mut child = Command::from(command)
        .arg("app-server")
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let limits = timeout(DEADLINE, query(&mut child)).await.ok().flatten();
    let _ = child.kill().await;
    snapshot(limits?, now_ms())
}

async fn query(child: &mut Child) -> Option<AccountLimits> {
    let mut input = child.stdin.take()?;
    let mut output = BufReader::new(child.stdout.take()?);
    let hello = json!({
        "id": 1,
        "method": "initialize",
        "params": {"clientInfo": {
            "name": "deckx", "title": "DeckX", "version": crate::APP_VERSION
        }},
    });
    input
        .write_all(format!("{hello}\n").as_bytes())
        .await
        .ok()?;
    input.flush().await.ok()?;
    result(&mut output, 1).await?;
    input
        .write_all(
            b"{\"method\":\"initialized\"}\n{\"id\":2,\"method\":\"account/rateLimits/read\"}\n",
        )
        .await
        .ok()?;
    input.flush().await.ok()?;
    serde_json::from_value(result(&mut output, 2).await?).ok()
}

async fn result(output: &mut BufReader<tokio::process::ChildStdout>, id: u64) -> Option<Value> {
    let mut line = Vec::new();
    loop {
        line.clear();
        let count = (&mut *output)
            .take(MAX_REPLY + 1)
            .read_until(b'\n', &mut line)
            .await
            .ok()?;
        if count == 0 || count as u64 > MAX_REPLY {
            return None;
        }
        if let Ok(mut reply) = serde_json::from_slice::<Value>(&line)
            && reply.get("id").and_then(Value::as_u64) == Some(id)
        {
            return reply.get_mut("result").map(Value::take);
        }
    }
}

fn snapshot(limits: AccountLimits, now: u64) -> Option<Reading> {
    let windows = [limits.rate_limits.primary, limits.rate_limits.secondary].map(|window| {
        let window = window?;
        Window::from_used(
            window.used_percent,
            window.window_duration_mins,
            window.resets_at.and_then(|time| time.checked_mul(1000)),
        )
    });
    Reading {
        windows,
        observed_at: now,
        expires_at: now.saturating_add(3 * MINUTE),
    }
    .current(now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_account_snapshot_keeps_a_weekly_only_plan_and_drops_a_reset_window() {
        let reply = json!({
            "rateLimits": {
                "primary": {"usedPercent": 27, "windowDurationMins": 10080, "resetsAt": 1200},
                "secondary": {"usedPercent": 80, "windowDurationMins": 300, "resetsAt": 999}
            }
        });
        let reading = snapshot(serde_json::from_value(reply).unwrap(), 1_000_000).unwrap();
        assert_eq!(reading.windows[0].unwrap().remaining, 73);
        assert_eq!(reading.windows[0].unwrap().minutes, Some(10080));
        assert_eq!(reading.windows[1], None);
    }
}
