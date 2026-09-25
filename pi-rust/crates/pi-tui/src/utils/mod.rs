//! Pure-helper layer.
//!
//! Everything in here is dependency-light: ratatui / crossterm may not be
//! imported. Modules split by concern:
//!
//! | Module | Concern |
//! |--------|---------|
//! | [`width`] | Display cell counts (CJK-aware) and column arithmetic |
//! | [`styled`] | `SpanStyle` / `StyledSpan` / `StyledLine` value types |
//! | [`util`] | Generic helpers (ANSI strip, OSC 8 link lookup, wrap with ANSI) |
//! | [`fuzzy`] | Fuzzy match used by selector / settings / autocomplete |
//! | [`highlight`] | Tree-sitter-backed syntax highlight tokens |
//! | [`hyperlink`] | OSC 8 hyperlink emission / parsing |
//! | [`word_navigation`] | Word boundary detection for Ctrl+Left/Right |
//! | [`visual_text`] | Visual line / cursor model the editor sits on top of |
//! | [`render_helpers`] | Paint-time helpers shared across widgets |

pub mod fuzzy;
pub mod highlight;
pub mod hyperlink;
pub mod render_helpers;
pub mod styled;
pub mod util;
pub mod visual_text;
pub mod width;
pub mod word_navigation;

pub use styled::{SpanStyle, StyledLine, StyledSpan};
pub use util::{get_osc8_link_at_column, slice_by_column, strip_terminal_sequences, wrap_text_with_ansi, visible_width};
pub use width::{char_columns, columns, prefix_columns, truncate_columns};
pub use word_navigation::{find_word_backward, find_word_forward};