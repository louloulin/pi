//! The config layer's bridge into the `pi-tui` registry: installing the merged
//! table at TUI startup, and re-installing it after a config reload.
//!
//! Own integration binary: `set_keybindings` mutates process state, so the
//! test must not share a process with the other coding-agent suites.

use pi_coding_agent::{install_keybindings_from, reload_keybindings};
use pi_tui::keybindings::{get_keybindings, reset_keybindings};

#[test]
fn installing_and_reloading_publishes_the_merged_table() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("keybindings.json");
    std::fs::write(&path, r#"{"cursorUp":["ctrl+p"]}"#).expect("write config fixture");

    let mut manager = install_keybindings_from(dir.path());

    // The merged table — the `pi-tui` defaults plus the `app.*` ids plus the
    // user override — is now the process global the components resolve
    // against.
    let installed = get_keybindings();
    assert!(
        !installed.get_keys("app.exit").is_empty(),
        "the coding-agent table defines app.* ids"
    );
    assert_eq!(
        installed.get_keys("tui.editor.cursorUp"),
        vec!["ctrl+p".to_string()]
    );

    // Editing the file and reloading republishes the table: the registry
    // holds a clone, so a `reload()` without the second install would go
    // unseen by the TUI.
    std::fs::write(&path, r#"{"cursorUp":["ctrl+n"]}"#).expect("rewrite config fixture");
    reload_keybindings(&mut manager);
    assert_eq!(
        get_keybindings().get_keys("tui.editor.cursorUp"),
        vec!["ctrl+n".to_string()]
    );

    reset_keybindings();
}
