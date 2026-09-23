//! `pi --clear-history` — the explicit cleanup entry point for the
//! cross-session prompt history (LUM-1319).
//!
//! The composer writes `$HOME/.pi/agent/history.jsonl` as the user submits
//! prompts (see `docs/LUM1319_HISTORY_PERSISTENCE.md` §3), and the TUI never
//! deletes it: `/clear` keeps the file on purpose, matching upstream. Clearing
//! it is therefore a deliberate parameter, and this suite spawns the real
//! binary to prove it deletes exactly that file — and nothing else in the
//! agent directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// Hard ceiling for a spawned `pi` command. Reaching it means the binary fell
/// back into interactive mode instead of handling the parameter.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

fn pi_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pi"))
}

/// `$HOME/.pi/agent/history.jsonl` for `home`.
fn history_path(home: &Path) -> PathBuf {
    home.join(".pi").join("agent").join("history.jsonl")
}

struct RunResult {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
    elapsed: Duration,
}

fn run_pi(args: &[&str], home: &Path) -> RunResult {
    let started = Instant::now();
    let output = Command::new(pi_bin())
        .args(args)
        .current_dir(home)
        .env("HOME", home)
        .env_remove("PI_HOME")
        .stdin(Stdio::null())
        .output()
        .expect("spawn pi");
    RunResult {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        elapsed: started.elapsed(),
    }
}

fn write_history(home: &Path) -> PathBuf {
    let path = history_path(home);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        "{\"text\":\"first prompt\",\"ts\":1}\n{\"text\":\"second prompt\",\"ts\":2}\n",
    )
    .unwrap();
    path
}

#[test]
fn clear_history_deletes_the_file_and_reports_the_path() {
    let temp = TempDir::new().unwrap();
    let path = write_history(temp.path());
    // A neighbouring agent-dir file must survive the cleanup.
    let settings = path.parent().unwrap().join("settings.json");
    fs::write(&settings, "{}").unwrap();

    let result = run_pi(&["--clear-history"], temp.path());
    assert!(
        result.elapsed < COMMAND_TIMEOUT,
        "pi --clear-history did not exit promptly: {:?}",
        result.elapsed
    );
    assert!(
        result.status.success(),
        "exit {:?}\nstdout: {}\nstderr: {}",
        result.status,
        result.stdout,
        result.stderr
    );
    assert!(
        result.stdout.contains(&path.display().to_string()),
        "the cleared path is reported:\n{}",
        result.stdout
    );
    assert!(!path.exists(), "history.jsonl is gone");
    assert!(settings.exists(), "only the history file is touched");
}

#[test]
fn clear_history_succeeds_when_there_is_nothing_to_clear() {
    let temp = TempDir::new().unwrap();
    let result = run_pi(&["--clear-history"], temp.path());
    assert!(
        result.status.success(),
        "a missing history file is not an error: {}",
        result.stderr
    );
    assert!(!history_path(temp.path()).exists());
}
