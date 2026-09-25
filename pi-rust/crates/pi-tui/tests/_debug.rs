use std::sync::Arc;
use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::autocomplete::{AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions, CompletionResult};
use pi_tui::input::{InputEvent, KeyCode, KeyModifiers, MouseButton, MouseGesture, MouseGestureKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const WIDTH: u16 = 40;
const HEIGHT: u16 = 14;

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

fn app() -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    App::new(
        &agent,
        AppConfig {
            session_id: "dbg".into(),
            ..AppConfig::default()
        },
    )
}

#[derive(Debug)]
struct FixedProvider {
    items: Vec<AutocompleteItem>,
}
impl FixedProvider {
    fn new(labels: &[&str]) -> Self {
        Self { items: labels.iter().map(|l| AutocompleteItem::new(*l, *l)).collect() }
    }
}
impl AutocompleteProvider for FixedProvider {
    fn get_suggestions(&self, _: &[String], _: usize, _: usize, _: bool) -> Option<AutocompleteSuggestions> {
        Some(AutocompleteSuggestions { items: self.items.clone(), prefix: String::new() })
    }
    fn apply_completion(&self, _: &[String], _: usize, _: usize, item: &AutocompleteItem, _: &str) -> CompletionResult {
        CompletionResult {
            lines: vec![item.value.clone()],
            cursor_line: 0,
            cursor_col: item.value.len(),
        }
    }
}

#[test]
fn debug_run() {
    let mut app = app();
    for i in 0..30 {
        app.info(format!("line {i}"));
    }
    let _ = app.render_snapshot(WIDTH, HEIGHT);
    app.prompt_mut().editor_mut().set_autocomplete_provider(Arc::new(FixedProvider::new(&["alpha", "beta", "gamma", "delta", "epsilon"])));
    for ch in "/".chars() {
        app.step(InputEvent::key(KeyCode::Char(ch), KeyModifiers::NONE));
    }

    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);

    for y in 0..HEIGHT {
        let row: String = (0..WIDTH).map(|x| {
            buf.cell((x, y))
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .unwrap_or(' ')
        }).collect();
        eprintln!("{y:2}: |{row}|");
    }
    eprintln!("after type '/', selected={}", app.prompt().editor().autocomplete_selected());
    eprintln!("autocomplete items: {:?}", app.prompt().editor().autocomplete_items().iter().map(|i| &i.value).collect::<Vec<_>>());

    for y in 0..HEIGHT {
        let row: String = (0..WIDTH).map(|x| {
            buf.cell((x, y))
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .unwrap_or(' ')
        }).collect();
        if let Some(x) = row.find("epsilon") {
            eprintln!("epsilon at ({}, {})", x, y);
            app.step(InputEvent::gesture(MouseGesture::new(MouseGestureKind::Press(MouseButton::Left), x as u16, y, false)));
            eprintln!("after press, selected={}", app.prompt().editor().autocomplete_selected());
            break;
        }
    }
}
