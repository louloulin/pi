//! `pi-tui` — Terminal UI primitives for the Pi Rust port.
//!
//! Stage 4 ports `packages/tui` (editor, message view, prompt, selector,
//! status bar) onto `ratatui` + `crossterm`, plus an [`App`] struct that
//! drives the interactive event loop and pipes [`AgentEvent`]s from
//! `pi-agent-core` into the rendered message view.
//!
//! See `docs/ARCHITECTURE.md` for how the crate fits into the workspace.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod app;
pub mod dialog;
pub mod editor;
pub mod fuzzy;
pub mod input;
pub mod kill_ring;
pub mod markdown;
pub mod message;
pub mod prompt;
pub mod selector;
pub mod status;
pub mod styled;
pub mod styles;
pub mod theme;
pub mod undo_stack;
pub mod word_navigation;

pub use app::{App, AppConfig, RenderSnapshot};
pub use dialog::{Dialog, DialogAction, DialogKind};
pub use editor::{Editor, EditorAction, JumpDirection};
pub use fuzzy::{fuzzy_filter, fuzzy_match, fuzzy_match_all, fuzzy_rank, FuzzyMatch};
pub use input::{InputEvent, Key, KeyModifiers};
pub use kill_ring::{KillDirection, KillRing};
pub use markdown::{render_markdown, render_markdown_with_theme};
pub use message::{MessageItem, MessageView};
pub use prompt::{Prompt, PromptAction};
pub use selector::{Selector, SelectorAction, SelectorItem, SelectorLayout};
pub use status::{StatusBar, StatusData};
pub use styled::{SpanStyle, StyledLine, StyledSpan};
pub use styles::SelectListStyles;
pub use theme::{
    available_themes, builtin_theme, builtin_theme_names, default_custom_themes_dir,
    default_theme_name, is_light_theme, load_theme, load_theme_from_path, parse_auto_theme_setting,
    resolve_theme_setting, ColorMode, ColorValue, TerminalTheme, Theme, ThemeBg, ThemeColor,
    ThemeController, ThemeError, ThemeJson,
};
pub use undo_stack::UndoStack;
pub use word_navigation::{find_word_backward, find_word_forward};

/// Re-export of the underlying terminal backend so binaries can pin a
/// single version of `crossterm`.
pub use crossterm as backend;
/// Re-export of the underlying widget library.
pub use ratatui as widgets;

/// Build info that the binary prints at startup.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
