//! Manual cross-check between the navigation tools and shell equivalents.
//!
//! Build with: `cargo run -p pi-coding-agent --example manual_check -- /tmp/manual_check`
//!
//! Drives `find`, `grep`, and `ls` over a real directory and prints the
//! output side-by-side with the equivalent shell command. The expectation
//! is that the two outputs match (modulo ordering / format details).

use std::env;
use std::path::PathBuf;

use pi_coding_agent::tools::{AbortLike, AgentTool, FindTool, GrepTool, LsTool};
use serde_json::json;

fn first_text(out: &pi_coding_agent::tools::ToolOutput) -> String {
    use pi_protocol::Content;
    match out.content.first().expect("content block") {
        Content::Text(t) => t.text.clone(),
        other => panic!("expected text content, got {:?}", other),
    }
}

#[tokio::main]
async fn main() {
    let mut args = env::args().skip(1);
    let path = args.next().expect("usage: manual_check <dir>");
    let dir: PathBuf = path.into();
    // The tools sandbox relative paths; chdir so we can pass `.` as
    // the search root.
    std::env::set_current_dir(&dir).expect("chdir to target");

    println!("=== directory: {} ===\n", dir.display());

    // ---- find **/*.rs ----
    println!("--- find **/*.rs ---");
    let out = FindTool
        .execute(
            json!({ "pattern": "**/*.rs", "path": "." }),
            AbortLike::none(),
        )
        .await
        .expect("find should succeed");
    println!("[tool]:\n{}\n", first_text(&out));
    println!("[shell]:\n");
    let shell = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "find {} -type f -name '*.rs' -not -path '*/.git/*' -not -path '*/node_modules/*' -not -path '*/target/*' -not -path '*/.pi/*' | sort",
            dir.display()
        ))
        .output()
        .expect("spawn shell");
    print!("{}", String::from_utf8_lossy(&shell.stdout));
    println!();

    // ---- grep ^name over Cargo.toml ----
    println!("--- grep '^name' Cargo.toml ---");
    let out = GrepTool
        .execute(
            json!({
                "pattern": "^name",
                "path": "Cargo.toml",
            }),
            AbortLike::none(),
        )
        .await
        .expect("grep should succeed");
    println!("[tool]:\n{}\n", first_text(&out));
    println!("[shell]:\n");
    let shell = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!("grep -n '^name' {}", dir.join("Cargo.toml").display()))
        .output()
        .expect("spawn shell");
    print!("{}", String::from_utf8_lossy(&shell.stdout));
    println!();

    // ---- ls -l ----
    println!("--- ls (detail: true) ---");
    let out = LsTool
        .execute(
            json!({ "path": ".", "detail": true }),
            AbortLike::none(),
        )
        .await
        .expect("ls should succeed");
    println!("[tool]:\n{}\n", first_text(&out));
    println!("[shell]:\n");
    let shell = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!("ls -l --time-style=+'%%Y-%%m-%%d %%H:%%M:%%S' {}", dir.display()))
        .output()
        .expect("spawn shell");
    print!("{}", String::from_utf8_lossy(&shell.stdout));
    println!();

    // ---- ls (all: true) ----
    println!("--- ls (all: true) ---");
    let out = LsTool
        .execute(
            json!({ "path": ".", "all": true }),
            AbortLike::none(),
        )
        .await
        .expect("ls should succeed");
    println!("[tool]:\n{}\n", first_text(&out));
    println!("[shell]:\n");
    let shell = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!("ls -A {}", dir.display()))
        .output()
        .expect("spawn shell");
    print!("{}", String::from_utf8_lossy(&shell.stdout));
    println!();
}