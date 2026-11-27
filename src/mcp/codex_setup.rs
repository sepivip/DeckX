use std::{
    env,
    ffi::OsStr,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

const NAME: &str = "deckx";
const ARGUMENT: &str = "--mcp";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// The longest part of the CLI's error output that DeckX shows.
const DETAIL_LIMIT: usize = 400;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Registration {
    Missing,
    Configured,
    Disabled,
    Different,
}

#[derive(Deserialize)]
struct Server {
    name: String,
    enabled: bool,
    transport: Transport,
}

#[derive(Deserialize)]
struct Transport {
    #[serde(rename = "type")]
    kind: String,
    command: Option<PathBuf>,
    #[serde(default)]
    args: Vec<String>,
}

/// The Codex CLI. npm installs only `codex.cmd` and `codex.ps1` shims.
#[derive(Debug, PartialEq, Eq)]
enum Cli {
    Exe(PathBuf),
    Cmd(PathBuf),
    PowerShell(PathBuf),
}

fn codex_cli() -> Result<Cli> {
    crate::codex::find_in_desktop(|root| {
        let path = root.join("app/resources/codex.exe");
        path.is_file().then_some(Cli::Exe(path))
    })
    .or_else(|| find_on_path(&env::var_os("PATH")?, Path::is_file))
    .context("Codex could not be found. Use the manual setup below.")
}

/// Searches PATH in order like a shell. Within a folder an executable wins
/// over the npm shims.
fn find_on_path(path: &OsStr, is_file: impl Fn(&Path) -> bool) -> Option<Cli> {
    let path = env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .find_map(|dir| {
            ["codex.exe", "codex.cmd", "codex.ps1"]
                .into_iter()
                .map(|name| dir.join(name))
                .find(|path| is_file(path))
        })?;
    Some(match path.extension().and_then(OsStr::to_str) {
        Some("cmd") => Cli::Cmd(path),
        Some("ps1") => Cli::PowerShell(path),
        _ => Cli::Exe(path),
    })
}

fn command(cli: &Cli) -> Command {
    let mut command = match cli {
        // For a .cmd path, Rust runs cmd.exe /c and escapes each argument for it.
        Cli::Exe(path) | Cli::Cmd(path) => Command::new(path),
        Cli::PowerShell(script) => {
            let shell = env::var_os("SystemRoot")
                .map(|root| {
                    PathBuf::from(root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
                })
                .filter(|path| path.is_file())
                .unwrap_or_else(|| "powershell.exe".into());
            let mut command = Command::new(shell);
            command
                .args(["-NoProfile", "-NonInteractive"])
                .args(["-ExecutionPolicy", "Bypass", "-File"])
                .arg(script);
            command
        }
    };
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

pub(crate) fn cli_command() -> Result<Command> {
    Ok(command(&codex_cli()?))
}

/// Runs the CLI and returns its output, or an error that includes the end of
/// what it printed to stderr.
fn run(mut command: Command, failure: &str) -> Result<Vec<u8>> {
    let output = command.output().context("Could not start Codex")?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    Err(match detail(&output.stderr) {
        Some(detail) => anyhow!("{failure}. Codex reported: {detail}"),
        None => anyhow!("{failure}."),
    })
}

fn detail(stderr: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim();
    let skip = text.chars().count().saturating_sub(DETAIL_LIMIT);
    // CLIs usually print the cause last, so keep the end.
    let detail: String = text.chars().skip(skip).collect();
    (!detail.is_empty()).then(|| {
        if skip > 0 {
            format!("…{}", detail.trim_start())
        } else {
            detail
        }
    })
}

pub fn inspect() -> Result<Registration> {
    registration(&codex_cli()?, &env::current_exe()?)
}

fn registration(cli: &Cli, exe: &Path) -> Result<Registration> {
    let mut list = command(cli);
    list.args(["mcp", "list", "--json"]);
    let output = run(list, "Codex could not read its MCP settings")?;
    let servers: Vec<Server> = serde_json::from_slice(&output)?;
    let Some(server) = servers.into_iter().find(|server| server.name == NAME) else {
        return Ok(Registration::Missing);
    };
    let matches = server.transport.kind == "stdio"
        && server.transport.args == [ARGUMENT]
        && server.transport.command.is_some_and(|path| {
            let actual = path.canonicalize().unwrap_or(path);
            let expected = exe.canonicalize().unwrap_or_else(|_| exe.to_owned());
            actual
                .as_os_str()
                .eq_ignore_ascii_case(expected.as_os_str())
        });
    Ok(if !matches {
        Registration::Different
    } else if !server.enabled {
        Registration::Disabled
    } else {
        Registration::Configured
    })
}

pub fn add() -> Result<Registration> {
    let cli = codex_cli()?;
    let exe = env::current_exe()?;
    let previous = registration(&cli, &exe)?;
    if previous != Registration::Missing {
        return Ok(previous);
    }
    let mut add = command(&cli);
    add.args(["mcp", "add", NAME])
        .arg("--env")
        .arg(local_data_env()?)
        .arg("--")
        .arg(&exe)
        .arg(ARGUMENT);
    run(add, "Codex could not add DeckX. Try the manual setup below")?;
    registration(&cli, &exe)
}

// Codex starts MCP servers with a reduced environment. `deckx --mcp` needs
// LOCALAPPDATA to find the running app's endpoint.
fn local_data_env() -> Result<String> {
    Ok(format!("LOCALAPPDATA={}", env::var("LOCALAPPDATA")?))
}

pub fn powershell_command() -> Result<String> {
    let quote = |text: &str| format!("'{}'", text.replace('\'', "''"));
    Ok(format!(
        "codex mcp add {NAME} --env {} -- {} {ARGUMENT}",
        quote(&local_data_env()?),
        quote(&env::current_exe()?.to_string_lossy()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn search(dirs: &[&str], files: &[&str]) -> Option<Cli> {
        let files: HashSet<PathBuf> = files.iter().map(PathBuf::from).collect();
        let path = env::join_paths(dirs).unwrap();
        find_on_path(&path, |candidate| files.contains(candidate))
    }

    #[test]
    fn path_search_finds_npm_shims_and_prefers_executables() {
        let npm = r"C:\Users\me\AppData\Roaming\npm";
        assert_eq!(
            search(
                &[r"C:\Tools", npm],
                &[
                    r"C:\Users\me\AppData\Roaming\npm\codex.cmd",
                    r"C:\Users\me\AppData\Roaming\npm\codex.ps1"
                ],
            ),
            Some(Cli::Cmd(PathBuf::from(npm).join("codex.cmd")))
        );
        assert_eq!(
            search(&[npm], &[r"C:\Users\me\AppData\Roaming\npm\codex.ps1"]),
            Some(Cli::PowerShell(PathBuf::from(npm).join("codex.ps1")))
        );
        assert_eq!(
            search(
                &[r"C:\Tools"],
                &[r"C:\Tools\codex.cmd", r"C:\Tools\codex.exe"],
            ),
            Some(Cli::Exe(PathBuf::from(r"C:\Tools\codex.exe")))
        );
        // The first folder on PATH wins, as it does in a shell.
        assert_eq!(
            search(
                &[npm, r"C:\Tools"],
                &[
                    r"C:\Users\me\AppData\Roaming\npm\codex.cmd",
                    r"C:\Tools\codex.exe"
                ],
            ),
            Some(Cli::Cmd(PathBuf::from(npm).join("codex.cmd")))
        );
        assert_eq!(search(&["bin", r"C:\Tools"], &[r"bin\codex.exe"]), None);
        assert_eq!(search(&[r"C:\Tools"], &[]), None);
    }

    #[test]
    fn cli_errors_keep_the_end_of_stderr() {
        assert_eq!(detail(b" \r\n"), None);
        assert_eq!(
            detail(b"warning: old config\nError: server deckx already exists\r\n").as_deref(),
            Some("warning: old config\nError: server deckx already exists")
        );
        let long = format!("{}Error: denied", "x".repeat(1000));
        let trimmed = detail(long.as_bytes()).unwrap();
        assert!(trimmed.starts_with('…') && trimmed.ends_with("Error: denied"));
        assert_eq!(trimmed.chars().count(), DETAIL_LIMIT + 1);
    }
}
