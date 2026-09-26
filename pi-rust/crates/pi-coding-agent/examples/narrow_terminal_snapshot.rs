//! P29 (G1) — manual snapshot for the three narrow-terminal width bands.
//!
//! Build with: `cargo run -p pi-coding-agent --example narrow_terminal_snapshot`
//!
//! Renders the same App state at three widths — wide (120), narrow (29),
//! extreme-narrow (15) — and writes the trimmed lines to
//! `target/snapshot/narrow-{wide,narrow,extreme}.txt`. The harness exists
//! to confirm that:
//!
//!   * the narrow band drops the secondary status segments (the footer
//!     collapses the cache/cost/hint segments);
//!   * the extreme-narrow band caps the composer to a single row and
//!     freezes the spinner;
//!   * the App's [`App::render_snapshot`] path does not panic at any
//!     of the three widths.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::message::MessageItem;
use pi_tui::narrow_terminal::{is_extreme_narrow, is_narrow, narrow_options};

const WIDTHS: [(u16, &str); 3] = [
    (120, "wide"),
    (29, "narrow"),
    (15, "extreme"),
];

fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("faux"),
        api: Api::Faux,
        id: "faux-model".into(),
        label: Some("Faux".into()),
        context_window: 1024,
        max_output_tokens: 256,
    }
}

fn fresh_app() -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(&agent, AppConfig::default());
    app.messages_mut().push(MessageItem::user("hi"));
    app
}

fn main() {
    let mut out_dir = PathBuf::from("target/snapshot");
    fs::create_dir_all(&out_dir).expect("create target/snapshot");

    for (width, label) in WIDTHS {
        // Sanity print the NarrowOptions before rendering so the snapshot
        // file's header documents what band the harness is exercising.
        let opts = narrow_options(width);
        eprintln!(
            "width={} ({}) narrow={} extreme={} max_rows={} spinner={}",
            width,
            label,
            opts.narrow,
            opts.extreme_narrow,
            opts.max_composer_rows,
            opts.spinner_animated,
        );
        assert_eq!(opts.narrow, is_narrow(width));
        assert_eq!(opts.extreme_narrow, is_extreme_narrow(width));

        let app = fresh_app();
        let snap = app.render_snapshot(width, 24);
        let path = out_dir.join(format!("narrow-{label}.txt"));
        fs::write(&path, snap.lines.join("\n")).expect("write snapshot");
        eprintln!("wrote {} ({} lines)", path.display(), snap.lines.len());
    }
}