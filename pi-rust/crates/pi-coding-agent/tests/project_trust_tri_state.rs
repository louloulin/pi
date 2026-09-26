//! Integration tests for the project-trust tri-state surface.
//!
//! Port of the *user-facing* slice of `packages/coding-agent/test/...`
//! related to `getProjectTrustOptions` + the `project_trust` extension
//! event shape:
//!
//! * `ProjectTrustDecision = Option<bool>` already encodes the tri-state
//!   (`Some(true)` = yes, `Some(false)` = no, `None` = undecided).
//! * [`ProjectTrustOption`] lists every row the user-facing prompt shows,
//!   including the `session_only` variants that never write to disk.
//! * The `project_trust` extension event returns
//!   `{ trusted: "yes" | "no" | "undecided", remember?: boolean }` and
//!   the wiring collapses `undecided` (and any unrecognised value) into
//!   "fall through to the UI prompt".
//!
//! The Rust unit tests in [`crate::trust`] cover the same ground without
//! touching the extension host. This file exists to pin the public API
//! shape so a downstream user can rely on `get_project_trust_options`
//! directly.

#![cfg(not(target_arch = "wasm32"))]

use pi_coding_agent::trust::{
    get_project_trust_options, get_project_trust_parent_path, ProjectTrustDecision,
    ProjectTrustOption, ProjectTrustStore, ProjectTrustUpdate,
};

#[test]
fn decision_type_carries_the_tri_state() {
    // Yes / No / Undecided are the three observable states.
    let yes: ProjectTrustDecision = Some(true);
    let no: ProjectTrustDecision = Some(false);
    let undecided: ProjectTrustDecision = None;
    assert_eq!(yes, Some(true));
    assert_eq!(no, Some(false));
    assert_eq!(undecided, None);
    assert_ne!(yes, no);
    assert_ne!(yes, undecided);
    assert_ne!(no, undecided);
}

#[test]
fn options_start_with_trust_then_parent_then_session_then_deny() {
    let temp = tempfile::tempdir().expect("tempdir");
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).expect("mkdir");

    let options = get_project_trust_options(&project, true);
    let labels: Vec<&str> = options.iter().map(|opt| opt.label.as_str()).collect();
    // Upstream order: Trust → Trust parent → Trust (session) → Do not trust → Do not trust (session).
    assert_eq!(labels[0], "Trust");
    assert!(labels[1].starts_with("Trust parent folder ("));
    assert_eq!(labels[2], "Trust (this session only)");
    assert_eq!(labels[3], "Do not trust");
    assert_eq!(labels[4], "Do not trust (this session only)");
}

#[test]
fn trust_option_round_trips_through_the_store() {
    let temp = tempfile::tempdir().expect("tempdir");
    let agent_dir = temp.path().join("agent");
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).expect("mkdir");

    let store = ProjectTrustStore::new(&agent_dir);
    let options = get_project_trust_options(&project, false);

    // Pick the first non-session row and apply it; the store should now
    // report the matching decision for `project`.
    let chosen: ProjectTrustOption = options.into_iter().next().expect("at least one");
    assert!(chosen.trusted);
    store.set_many(&chosen.updates).expect("set_many");
    assert_eq!(store.get(&project).expect("read"), Some(true));
}

#[test]
fn trust_parent_folder_clear_is_visible_via_an_update() {
    let temp = tempfile::tempdir().expect("tempdir");
    let project = temp.path().join("nested").join("project");
    std::fs::create_dir_all(&project).expect("mkdir");

    let options = get_project_trust_options(&project, false);
    let parent_row = options
        .iter()
        .find(|opt| opt.label.starts_with("Trust parent folder ("))
        .expect("parent row");
    // Exactly two writes: parent=true, project cleared.
    let writes: &Vec<ProjectTrustUpdate> = &parent_row.updates;
    assert_eq!(writes.len(), 2);
    assert_eq!(writes[0].decision, Some(true));
    assert_eq!(writes[1].decision, None);
}

#[test]
fn parent_path_helper_matches_the_label() {
    let temp = tempfile::tempdir().expect("tempdir");
    let parent = temp.path().join("trusted-parent");
    let project = parent.join("project");
    std::fs::create_dir_all(&project).expect("mkdir");

    let parent_path = get_project_trust_parent_path(&project).expect("parent exists");
    let options = get_project_trust_options(&project, false);
    let parent_row = options
        .iter()
        .find(|opt| opt.label.starts_with("Trust parent folder ("))
        .expect("parent row");
    let label_tail = parent_row
        .label
        .trim_start_matches("Trust parent folder (")
        .trim_end_matches(')');
    // The label tail must be the parent path's canonical form, so a UI
    // selector matching on the label can also resolve it back to a path.
    assert_eq!(
        label_tail,
        parent_path.canonicalize().unwrap().to_string_lossy()
    );
}

#[test]
fn session_only_rows_carry_no_updates() {
    let temp = tempfile::tempdir().expect("tempdir");
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).expect("mkdir");

    let options = get_project_trust_options(&project, true);
    let session_only: Vec<&ProjectTrustOption> =
        options.iter().filter(|opt| opt.session_only).collect();
    assert_eq!(session_only.len(), 2);
    for opt in &session_only {
        assert!(opt.updates.is_empty());
    }
}