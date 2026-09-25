use std::sync::Arc;
use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("faux"),
        id: "faux-model".into(),
        api: Api::Faux,
        label: Some("Faux".into()),
        context_window: 1024,
        max_output_tokens: 256,
    }
}

fn dump(label: &str, app: &mut App, w: u16, h: u16) {
    let mut buf = Buffer::empty(Rect { x: 0, y: 0, width: w, height: h });
    app.render_to_buffer(Rect { x: 0, y: 0, width: w, height: h }, &mut buf);
    println!("=== {label} ({w}x{h}) ===");
    for y in 0..h {
        let mut row = String::new();
        for x in 0..w {
            if let Some(cell) = buf.cell((x, y)) {
                row.push_str(cell.symbol());
            }
        }
        println!("{:02}|{row}|", y);
    }
}

fn main() {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(&agent, AppConfig::default());

    app.messages_mut().push_info("theme → not applied: Theme not found: upup-dark");
    app.messages_mut().push_info("你是谁");
    app.messages_mut().push_info("你能做啥");
    app.messages_mut().push_info("Session exported to: session-2026-09-25T02-43-56-136Z.html");
    app.messages_mut().push_info("sdfsdf");

    dump("after 5 user messages", &mut app, 100, 30);
}
