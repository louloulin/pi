//! Integration tests for the Stage 11 `pi packages` ecosystem.
//!
//! The tests never touch the network: remote sources go through a mock
//! [`PackageFetcher`], and the CLI is exercised against a temporary
//! `PI_HOME` / `--dir` root. The `version` / `list-models` / `install` /
//! `remove` / `list` commands are spawned as real processes so a
//! regression back into interactive mode shows up as a timeout.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use pi_coding_agent::packages::{installer, InstallError, PackageFetcher, PackageSpec, Registry};
use pi_extensions::ExtensionSearchPaths;
use tempfile::TempDir;

/// Hard ceiling for a spawned `pi` command. Reaching it means the binary
/// fell back into interactive mode.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

struct RunResult {
    status: ExitStatus,
    stdout: String,
    stderr: String,
    elapsed: Duration,
}

fn pi_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pi"))
}

/// Spawn `pi <args>` with a null stdin and a bounded wall-clock budget.
fn run_pi(args: &[&str], cwd: &Path, home: &Path, pi_home: Option<&Path>) -> RunResult {
    let mut cmd = Command::new(pi_bin());
    cmd.args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match pi_home {
        Some(root) => {
            cmd.env("PI_HOME", root);
        }
        None => {
            cmd.env_remove("PI_HOME");
        }
    }

    let mut child = cmd.spawn().expect("failed to spawn pi");
    let start = Instant::now();
    loop {
        match child.try_wait().expect("try_wait") {
            Some(_) => break,
            None => {
                if start.elapsed() > COMMAND_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "`pi {}` hung for {:?} (interactive regression)",
                        args.join(" "),
                        COMMAND_TIMEOUT
                    );
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
    let output = child.wait_with_output().expect("wait_with_output");
    RunResult {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        elapsed: start.elapsed(),
    }
}

/// Write a minimal `pi-package` fixture with one extension.
fn write_fixture(dir: &Path, name: &str) -> PathBuf {
    let package = dir.join(name);
    fs::create_dir_all(package.join("extensions")).expect("fixture extensions dir");
    fs::write(
        package.join("package.json"),
        format!(r#"{{"name":"{name}","keywords":["pi-package"]}}"#),
    )
    .expect("fixture package.json");
    fs::write(package.join("extensions/hello.js"), "export default {};\n")
        .expect("fixture extension");
    package
}

fn temp() -> TempDir {
    TempDir::with_prefix(format!("pi-packages-{}-", std::process::id())).expect("tempdir")
}

// ---------------------------------------------------------------------------
// CLI: install / list / registry / loader discovery
// ---------------------------------------------------------------------------

#[test]
fn install_file_package_is_listed_and_discoverable() {
    let temp = temp();
    let root = temp.path().join("pi-root");
    let cwd = temp.path();
    write_fixture(cwd, "hello-pkg");

    let install = run_pi(
        &["install", "./hello-pkg", "--dir", root.to_str().unwrap()],
        cwd,
        temp.path(),
        None,
    );
    assert_eq!(
        install.status.code(),
        Some(0),
        "install failed: stdout={:?} stderr={:?}",
        install.stdout,
        install.stderr
    );
    assert!(
        install.stdout.contains("Installed ./hello-pkg"),
        "{}",
        install.stdout
    );

    // `pi list` sees the package.
    let list = run_pi(
        &["list", "--dir", root.to_str().unwrap()],
        cwd,
        temp.path(),
        None,
    );
    assert_eq!(list.status.code(), Some(0), "stderr={:?}", list.stderr);
    assert!(
        list.stdout.contains("hello-pkg"),
        "list output: {}",
        list.stdout
    );

    // The registry is valid JSON with the documented schema.
    let registry_path = root.join("packages.json");
    let raw = fs::read_to_string(&registry_path).expect("registry file");
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("registry parses");
    let entry = &parsed["packages"][0];
    assert_eq!(entry["name"], "hello-pkg");
    assert_eq!(entry["spec"], "./hello-pkg");
    assert!(
        entry["installedAt"].is_string(),
        "installedAt missing: {raw}"
    );
    assert!(entry["resolved"].is_string());
    assert!(Path::new(entry["resolved"].as_str().unwrap()).is_dir());

    // The library re-parses the same registry.
    let registry = Registry::load(&root).expect("load registry");
    assert_eq!(registry.packages().len(), 1);
    assert_eq!(registry.packages()[0].name, "hello-pkg");

    // `pi-extensions` discovers the installed extension.
    let search = ExtensionSearchPaths {
        global: Some(root.join(installer::EXTENSIONS_DIR)),
        project: None,
    };
    let candidates = search.candidates();
    assert!(
        candidates
            .iter()
            .any(|path| path.file_name().and_then(|s| s.to_str()) == Some("hello.js")),
        "loader did not find the extension: {candidates:?}"
    );

    // Idempotent re-install does not append a second entry.
    let reinstall = run_pi(
        &["install", "./hello-pkg", "--dir", root.to_str().unwrap()],
        cwd,
        temp.path(),
        None,
    );
    assert_eq!(
        reinstall.status.code(),
        Some(0),
        "stderr={:?}",
        reinstall.stderr
    );
    let registry = Registry::load(&root).expect("reload registry");
    assert_eq!(
        registry.packages().len(),
        1,
        "re-install appended a duplicate"
    );
}

#[test]
fn remove_deletes_package_and_empties_registry() {
    let temp = temp();
    let root = temp.path().join("pi-root");
    let cwd = temp.path();
    write_fixture(cwd, "hello-pkg");

    let install = run_pi(
        &["install", "./hello-pkg", "--dir", root.to_str().unwrap()],
        cwd,
        temp.path(),
        None,
    );
    assert_eq!(
        install.status.code(),
        Some(0),
        "stderr={:?}",
        install.stderr
    );

    let remove = run_pi(
        &["remove", "./hello-pkg", "--dir", root.to_str().unwrap()],
        cwd,
        temp.path(),
        None,
    );
    assert_eq!(remove.status.code(), Some(0), "stderr={:?}", remove.stderr);
    assert!(remove.stdout.contains("Removed"), "{}", remove.stdout);

    assert!(
        !root.join("packages/hello-pkg").exists(),
        "install dir not deleted"
    );
    assert!(
        !root.join("agent/extensions/hello-pkg").exists(),
        "extension dir not deleted"
    );

    let list = run_pi(
        &["list", "--dir", root.to_str().unwrap()],
        cwd,
        temp.path(),
        None,
    );
    assert_eq!(list.status.code(), Some(0), "stderr={:?}", list.stderr);
    assert!(
        list.stdout.trim().is_empty(),
        "list not empty: {}",
        list.stdout
    );

    // Removing a missing package is a non-zero, non-hanging error.
    let missing = run_pi(
        &["remove", "npm:nope", "--dir", root.to_str().unwrap()],
        cwd,
        temp.path(),
        None,
    );
    assert_ne!(missing.status.code(), Some(0));
    assert!(!missing.stderr.is_empty(), "expected an error message");
}

#[test]
fn pi_home_overrides_the_root() {
    let temp = temp();
    let root = temp.path().join("pi-home");
    let cwd = temp.path();
    write_fixture(cwd, "hello-pkg");

    let install = run_pi(&["install", "./hello-pkg"], cwd, temp.path(), Some(&root));
    assert_eq!(
        install.status.code(),
        Some(0),
        "stderr={:?}",
        install.stderr
    );
    assert!(
        root.join("packages.json").is_file(),
        "PI_HOME root not used"
    );

    let list = run_pi(&["list"], cwd, temp.path(), Some(&root));
    assert_eq!(list.status.code(), Some(0), "stderr={:?}", list.stderr);
    assert!(list.stdout.contains("hello-pkg"), "{}", list.stdout);
}

#[test]
fn invalid_spec_exits_with_ex_usage() {
    let temp = temp();
    let root = temp.path().join("pi-root");
    for spec in [
        "some-bare-name",
        "npm:",
        "not-a-spec",
        "ftp://example.com/pkg",
    ] {
        let result = run_pi(
            &["install", spec, "--dir", root.to_str().unwrap()],
            temp.path(),
            temp.path(),
            None,
        );
        assert_eq!(
            result.status.code(),
            Some(64),
            "spec {spec:?} should exit 64, got {:?} (stderr={:?})",
            result.status.code(),
            result.stderr
        );
        assert!(!result.stderr.is_empty(), "spec {spec:?} produced no error");
    }

    // A syntactically valid local path that does not exist is a runtime
    // error, not a usage error.
    let missing = run_pi(
        &[
            "install",
            "./does-not-exist",
            "--dir",
            root.to_str().unwrap(),
        ],
        temp.path(),
        temp.path(),
        None,
    );
    assert_eq!(
        missing.status.code(),
        Some(66),
        "stderr={:?}",
        missing.stderr
    );
    assert!(!missing.stderr.is_empty());
}

// ---------------------------------------------------------------------------
// Regression: these commands used to fall through to interactive mode
// ---------------------------------------------------------------------------

#[test]
fn version_list_and_models_do_not_enter_interactive_mode() {
    let temp = temp();
    let cwd = temp.path();

    let version = run_pi(&["version"], cwd, temp.path(), None);
    assert_eq!(
        version.status.code(),
        Some(0),
        "stderr={:?}",
        version.stderr
    );
    assert!(version.stdout.contains("pi "), "{}", version.stdout);
    assert!(version.stdout.contains("faux"), "{}", version.stdout);
    assert!(version.elapsed < COMMAND_TIMEOUT);

    let models = run_pi(&["list-models"], cwd, temp.path(), None);
    assert_eq!(models.status.code(), Some(0), "stderr={:?}", models.stderr);
    assert!(
        !models.stdout.trim().is_empty(),
        "list-models printed nothing"
    );

    let models_json = run_pi(&["list-models", "--output", "json"], cwd, temp.path(), None);
    assert_eq!(
        models_json.status.code(),
        Some(0),
        "stderr={:?}",
        models_json.stderr
    );
    let parsed: serde_json::Value =
        serde_json::from_str(models_json.stdout.trim()).expect("json output");
    assert!(parsed["models"].as_array().is_some_and(|m| !m.is_empty()));

    let update = run_pi(&["update-models"], cwd, temp.path(), None);
    assert_eq!(update.status.code(), Some(0), "stderr={:?}", update.stderr);
    assert!(
        update.stdout.contains("Model catalogs refreshed"),
        "{}",
        update.stdout
    );
}

// ---------------------------------------------------------------------------
// Remote source path — exercised with an in-memory fetcher (no network)
// ---------------------------------------------------------------------------

struct MockFetcher;

impl PackageFetcher for MockFetcher {
    fn fetch(&self, spec: &PackageSpec, dest: &Path) -> Result<PathBuf, InstallError> {
        match spec {
            PackageSpec::Npm { name, .. } => {
                fs::create_dir_all(dest.join("extensions")).expect("mock extensions");
                fs::write(
                    dest.join("package.json"),
                    format!(r#"{{"name":"{name}","keywords":["pi-package"]}}"#),
                )
                .expect("mock package.json");
                fs::write(dest.join("extensions/mock.js"), "export default {};\n")
                    .expect("mock extension");
                Ok(dest.to_path_buf())
            }
            other => Err(InstallError::Fetch(format!(
                "mock fetcher does not handle {other:?}"
            ))),
        }
    }
}

#[test]
fn npm_install_uses_fetcher_and_stays_idempotent() {
    let temp = temp();
    let root = temp.path().join("pi-root");
    let cwd = temp.path();

    let first =
        installer::install(&root, cwd, "npm:@foo/bar@1.0.0", &MockFetcher).expect("install");
    assert_eq!(first.name, "@foo/bar");
    assert!(Path::new(&first.resolved)
        .join("extensions/mock.js")
        .is_file());
    assert_eq!(first.extensions.len(), 1);

    installer::install(&root, cwd, "npm:@foo/bar@1.0.0", &MockFetcher).expect("reinstall");
    let registry = Registry::load(&root).expect("registry");
    assert_eq!(registry.packages().len(), 1, "duplicate npm entry");

    let removed = installer::remove(&root, "npm:@foo/bar@1.0.0").expect("remove");
    assert!(removed);
    assert!(!root.join("packages/@foo-bar").exists());
    assert!(Registry::load(&root).expect("registry").is_empty());
}

#[test]
fn spec_parsing_covers_all_source_kinds() {
    assert!(matches!(
        PackageSpec::parse("npm:pkg"),
        Ok(PackageSpec::Npm { .. })
    ));
    assert!(matches!(
        PackageSpec::parse("git:host/user/repo@v1"),
        Ok(PackageSpec::Git { .. })
    ));
    assert!(matches!(
        PackageSpec::parse("https://host/user/repo"),
        Ok(PackageSpec::Https { .. })
    ));
    assert!(matches!(
        PackageSpec::parse("/tmp/pkg"),
        Ok(PackageSpec::File { .. })
    ));
    assert!(PackageSpec::parse("").is_err());
}
