//! Concentrated UI copy — the startup header's strings, in one table per
//! locale.
//!
//! Martty keeps its UI strings in `src/locale.rs` (`Locale`, `tr(en, zh)`,
//! `command_desc`) rather than pulling a i18n framework into the binary; this
//! module is the same shape for the one surface that needs it today: the
//! built-in startup header. The table is deliberately flat `&'static str`
//! pairs, so a new hint is one row and adding a locale is one variant — no
//! runtime lookup, no dependency.
//!
//! The hint *chords* are not baked in: a row names a keybinding id and the
//! renderer resolves it against the live table, so a `keybindings.json`
//! override is reflected in the header (upstream's `keyHint` /
//! `keyText`, `interactive-mode.ts:910-944`).

/// The copy table the built-in startup header reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Locale {
    /// English (the default).
    #[default]
    En,
    /// Simplified Chinese.
    Zh,
}

impl Locale {
    /// Parse a `--lang` / `PI_LANG` style code (`en`, `en-US`, `zh`, `zh-CN`).
    /// Anything else — including an empty string — is `None`, so the caller
    /// keeps its current locale instead of silently switching to English.
    pub fn parse(value: &str) -> Option<Self> {
        let base = value
            .trim()
            .split(['-', '_'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        match base.as_str() {
            "en" => Some(Locale::En),
            "zh" => Some(Locale::Zh),
            _ => None,
        }
    }

    /// The BCP-47-ish code for this locale.
    pub fn code(self) -> &'static str {
        match self {
            Locale::En => "en",
            Locale::Zh => "zh",
        }
    }

    /// Pick the copy for this locale (Martty's `tr(en, zh)`).
    pub fn tr<'a>(self, en: &'a str, zh: &'a str) -> &'a str {
        match self {
            Locale::En => en,
            Locale::Zh => zh,
        }
    }
}

/// The key half of a startup-header row.
///
/// A [`HeaderKey::Chord`] names a keybinding id and resolves to the *effective*
/// chords at render time; the other variants are literal key text from
/// upstream's `rawKeyHint` calls (`!`, `!!`, `/`, `drop files`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderKey {
    /// The effective chord(s) of one keybinding id.
    Chord(&'static str),
    /// The effective chord of one id, with upstream's `"… twice"` suffix
    /// (`Ctrl+C twice`).
    ChordTwice(&'static str),
    /// Two ids joined with `/` (`Ctrl+P/Shift+Ctrl+P`).
    ChordPair(&'static str, &'static str),
    /// Literal key text that is not a keybinding id.
    Literal(&'static str),
}

/// One startup-header hint: a key, plus the English and Chinese copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderHint {
    /// The key half of the row.
    pub key: HeaderKey,
    /// English description.
    pub en: &'static str,
    /// Simplified-Chinese description.
    pub zh: &'static str,
}

impl HeaderHint {
    /// The description for `locale`.
    pub fn description(&self, locale: Locale) -> &'static str {
        locale.tr(self.en, self.zh)
    }

    /// True when every `app.*` id this row names has a real consumer.
    ///
    /// A row is dropped when its action is bound but unimplemented, so the
    /// header can never advertise a key that does nothing (LUM-1240). The
    /// [`HeaderKey::Literal`] rows are raw key text, not action ids, and a
    /// [`HeaderKey::ChordPair`] stays while either half works — the pair's
    /// label already drops the dead half.
    pub fn is_wired(&self) -> bool {
        use crate::keybindings::app_action_is_consumed;
        match self.key {
            HeaderKey::Chord(id) | HeaderKey::ChordTwice(id) => app_action_is_consumed(id),
            HeaderKey::ChordPair(first, second) => {
                app_action_is_consumed(first) || app_action_is_consumed(second)
            }
            HeaderKey::Literal(_) => true,
        }
    }
}

/// The startup-header hint rows, in upstream order
/// (`interactive-mode.ts:918-937`, `expandedInstructions`).
///
/// `app.header` has no upstream counterpart (upstream drives the header's
/// expansion from `setToolsExpanded`) and is listed right after
/// `app.tools.expand` so the chord that folds this screen is discoverable on
/// the screen itself.
pub const STARTUP_HINTS: &[HeaderHint] = &[
    HeaderHint {
        key: HeaderKey::Chord("app.interrupt"),
        en: "to interrupt",
        zh: "中断当前回合",
    },
    HeaderHint {
        key: HeaderKey::Chord("app.clear"),
        en: "to clear",
        zh: "清空输入",
    },
    HeaderHint {
        key: HeaderKey::ChordTwice("app.clear"),
        en: "to exit",
        zh: "退出",
    },
    HeaderHint {
        key: HeaderKey::Chord("app.exit"),
        en: "to exit (empty)",
        zh: "退出（输入为空时）",
    },
    HeaderHint {
        key: HeaderKey::Chord("app.suspend"),
        en: "to suspend",
        zh: "挂起到后台",
    },
    HeaderHint {
        key: HeaderKey::Chord("tui.editor.deleteToLineEnd"),
        en: "to delete to end",
        zh: "删除到行尾",
    },
    HeaderHint {
        key: HeaderKey::Chord("tui.editor.historySearch"),
        en: "to search history",
        zh: "反查历史",
    },
    HeaderHint {
        key: HeaderKey::Chord("app.thinking.cycle"),
        en: "to cycle thinking level",
        zh: "切换思考等级",
    },
    HeaderHint {
        key: HeaderKey::ChordPair("app.model.cycleForward", "app.model.cycleBackward"),
        en: "to cycle models",
        zh: "切换模型",
    },
    HeaderHint {
        key: HeaderKey::Chord("app.model.select"),
        en: "to select model",
        zh: "选择模型",
    },
    HeaderHint {
        key: HeaderKey::Chord("app.tools.expand"),
        en: "to expand tools",
        zh: "展开工具输出",
    },
    HeaderHint {
        key: HeaderKey::Chord("app.header"),
        en: "to hide this header",
        zh: "收起本页提示",
    },
    HeaderHint {
        key: HeaderKey::Chord("app.thinking.toggle"),
        en: "to expand thinking",
        zh: "展开思考块",
    },
    HeaderHint {
        key: HeaderKey::Chord("app.editor.external"),
        en: "for external editor",
        zh: "打开外部编辑器",
    },
    HeaderHint {
        key: HeaderKey::Literal("/"),
        en: "for commands",
        zh: "斜杠命令",
    },
    HeaderHint {
        key: HeaderKey::Literal("!"),
        en: "to run bash",
        zh: "执行 bash",
    },
    HeaderHint {
        key: HeaderKey::Literal("!!"),
        en: "to run bash (no context)",
        zh: "执行 bash（不进上下文）",
    },
    HeaderHint {
        key: HeaderKey::Chord("app.message.followUp"),
        en: "to queue follow-up",
        zh: "排队后续消息",
    },
    HeaderHint {
        key: HeaderKey::Chord("app.message.dequeue"),
        en: "to edit all queued messages",
        zh: "取回全部排队消息",
    },
    HeaderHint {
        key: HeaderKey::Chord("app.clipboard.pasteImage"),
        en: "to paste image (with text fallback)",
        zh: "粘贴图片（无图则粘贴文本）",
    },
    HeaderHint {
        key: HeaderKey::Literal("drop files"),
        en: "to attach",
        zh: "拖入文件作为附件",
    },
];

/// The header's product line, minus the version (the renderer appends
/// `v<VERSION>`), upstream's `logo` (`interactive-mode.ts:913`).
pub const HEADER_TITLE: &str = "pi";

/// Upstream's `onboarding` line (`interactive-mode.ts:952`).
pub const HEADER_ONBOARDING_EN: &str =
    "Pi can explain its own features and look up its docs. Ask it how to use or extend Pi.";
/// Chinese rendering of [`HEADER_ONBOARDING_EN`].
pub const HEADER_ONBOARDING_ZH: &str =
    "Pi 可以讲解自身功能并检索文档。直接问它「怎么用」或「怎么扩展」。";

/// Startup-header row shown when the terminal is too short for the full hint
/// list and the header folded itself for this frame (LUM-1266).
///
/// `keys` is the resolved `app.header` chord — the action that expands the
/// header again — so the row stays honest when the binding is overridden.
pub fn header_folded_line(locale: Locale, keys: &str) -> String {
    match locale {
        Locale::En => format!("hints hidden on a short terminal — {keys} shows them"),
        Locale::Zh => format!("终端太矮，键位提示已折叠 — {keys} 展开"),
    }
}

/// Startup-header copy for `--no-extensions`.
///
/// Kept in the `extensions: none` shape the audit pins, with the flag that
/// caused it in parentheses: the user who typed `--no-extensions` wanted
/// exactly that, and a silent header would read as "the flag did nothing".
pub const EXTENSIONS_DISABLED_EN: &str = "extensions: none (--no-extensions)";
/// Chinese rendering of [`EXTENSIONS_DISABLED_EN`].
pub const EXTENSIONS_DISABLED_ZH: &str = "扩展: 无（--no-extensions）";

/// The startup header's extension summary: `N extension(s): a.mjs, b.mjs`.
///
/// The caller (the App's built-in header) guarantees `names` is non-empty,
/// so the row never renders a count with nothing behind it.
pub fn extensions_summary_line(locale: Locale, count: usize, names: &[String]) -> String {
    let list = names.join(", ");
    match locale {
        Locale::En => format!("{count} extension(s): {list}"),
        Locale::Zh => format!("{count} 个扩展: {list}"),
    }
}

/// Render a key id as the reader sees it: `ctrl+o` → `Ctrl+O`, `alt+enter` →
/// `Alt+Enter`, `escape` → `Esc`.
///
/// Same mapping as the `/hotkeys` table in `pi-coding-agent`, which this
/// duplicates on purpose: `pi-tui` cannot depend on `pi-coding-agent`, and
/// the header is rendered by the App.
pub fn format_chord(chord: &str) -> String {
    chord
        .split('+')
        .map(|part| match part {
            "ctrl" => "Ctrl".to_string(),
            "alt" => "Alt".to_string(),
            "shift" => "Shift".to_string(),
            "super" | "meta" => "Cmd".to_string(),
            "escape" => "Esc".to_string(),
            "pageUp" => "PgUp".to_string(),
            "pageDown" => "PgDn".to_string(),
            "home" => "Home".to_string(),
            "end" => "End".to_string(),
            "tab" => "Tab".to_string(),
            "enter" => "Enter".to_string(),
            "space" => "Space".to_string(),
            "up" => "Up".to_string(),
            "down" => "Down".to_string(),
            "left" => "Left".to_string(),
            "right" => "Right".to_string(),
            other => {
                // Single letters are title-cased so `ctrl+p` reads `Ctrl+P`.
                if other.chars().count() == 1 {
                    other.to_uppercase()
                } else {
                    other.to_string()
                }
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

impl HeaderKey {
    /// Resolve this key to display text, or `None` when a keybinding id has
    /// no chord in `resolve` (an unbound action is not a hint — same rule as
    /// `/hotkeys`).
    ///
    /// Chords are formatted for the reader ([`format_chord`]), so
    /// `ctrl+p/shift+ctrl+p` reads `Ctrl+P/Shift+Ctrl+P`.
    pub fn label(&self, resolve: impl Fn(&str) -> Vec<String>) -> Option<String> {
        let chords = |id: &str| -> Option<String> {
            let keys = resolve(id);
            if keys.is_empty() {
                None
            } else {
                Some(
                    keys.iter()
                        .map(|chord| format_chord(chord))
                        .collect::<Vec<_>>()
                        .join("/"),
                )
            }
        };
        match self {
            HeaderKey::Chord(id) => chords(id),
            HeaderKey::ChordTwice(id) => chords(id).map(|keys| format!("{keys} twice")),
            HeaderKey::ChordPair(a, b) => match (chords(a), chords(b)) {
                (Some(a), Some(b)) => Some(format!("{a}/{b}")),
                // One half unbound: fall back to the other rather than
                // dropping a chord the user can actually press.
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            },
            HeaderKey::Literal(text) => Some((*text).to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(id: &str) -> Vec<String> {
        match id {
            "app.interrupt" => vec!["escape".to_string()],
            "app.clear" => vec!["ctrl+c".to_string()],
            "app.model.cycleForward" => vec!["ctrl+p".to_string()],
            "app.model.cycleBackward" => vec!["shift+ctrl+p".to_string()],
            "app.session.tree" => Vec::new(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn extension_summary_line_joins_the_names() {
        let names = vec!["./a.mjs".to_string(), "~/b.mjs".to_string()];
        assert_eq!(
            extensions_summary_line(Locale::En, 2, &names),
            "2 extension(s): ./a.mjs, ~/b.mjs"
        );
        assert_eq!(
            extensions_summary_line(Locale::Zh, 2, &names),
            "2 个扩展: ./a.mjs, ~/b.mjs"
        );
        // The `--no-extensions` copy keeps the `extensions: none` shape so
        // the audit's acceptance wording holds.
        assert!(EXTENSIONS_DISABLED_EN.starts_with("extensions: none"));
        // Not just non-empty: the Chinese copy has to carry the same two facts
        // (`--no-extensions` and "none"). `!is_empty()` on a `&str` const was
        // a vacuous assertion (`clippy::const_is_empty`), and LUM-1308 tightened
        // it to the content instead of silencing the lint.
        assert!(EXTENSIONS_DISABLED_ZH.contains("扩展: 无"));
        assert!(EXTENSIONS_DISABLED_ZH.contains("--no-extensions"));
    }

    #[test]
    fn parses_locale_codes_with_a_region_suffix() {
        assert_eq!(Locale::parse("en"), Some(Locale::En));
        assert_eq!(Locale::parse("en-US"), Some(Locale::En));
        assert_eq!(Locale::parse("zh_CN"), Some(Locale::Zh));
        assert_eq!(Locale::parse("ZH"), Some(Locale::Zh));
        assert_eq!(Locale::parse("fr"), None);
        assert_eq!(Locale::parse(""), None);
    }

    #[test]
    fn tr_picks_the_locale_column() {
        assert_eq!(Locale::En.tr("en", "zh"), "en");
        assert_eq!(Locale::Zh.tr("en", "zh"), "zh");
        assert_eq!(Locale::default(), Locale::En);
        assert_eq!(Locale::Zh.code(), "zh");
    }

    #[test]
    fn format_chord_title_cases_each_part() {
        assert_eq!(format_chord("ctrl+o"), "Ctrl+O");
        assert_eq!(format_chord("alt+enter"), "Alt+Enter");
        assert_eq!(format_chord("escape"), "Esc");
        assert_eq!(format_chord("shift+tab"), "Shift+Tab");
    }

    #[test]
    fn header_keys_resolve_through_the_live_chord_table() {
        assert_eq!(
            HeaderKey::Chord("app.interrupt").label(resolve),
            Some("Esc".to_string())
        );
        assert_eq!(
            HeaderKey::ChordTwice("app.clear").label(resolve),
            Some("Ctrl+C twice".to_string())
        );
        assert_eq!(
            HeaderKey::ChordPair("app.model.cycleForward", "app.model.cycleBackward")
                .label(resolve),
            Some("Ctrl+P/Shift+Ctrl+P".to_string())
        );
        // A pair with one half unbound keeps the half that works.
        assert_eq!(
            HeaderKey::ChordPair("app.model.cycleForward", "app.model.select").label(resolve),
            Some("Ctrl+P".to_string())
        );
        // A wholly unbound id is not a hint.
        assert_eq!(HeaderKey::Chord("app.session.tree").label(resolve), None);
        assert_eq!(
            HeaderKey::Literal("drop files").label(resolve),
            Some("drop files".to_string())
        );
    }

    #[test]
    fn every_row_has_copy_in_both_locales() {
        for hint in STARTUP_HINTS {
            assert!(!hint.en.is_empty(), "{hint:?}");
            assert!(!hint.zh.is_empty(), "{hint:?}");
            assert_ne!(hint.description(Locale::En), hint.description(Locale::Zh));
        }
    }

    #[test]
    fn the_table_covers_the_upstream_hints_plus_the_header_toggle() {
        let ids: Vec<&str> = STARTUP_HINTS
            .iter()
            .filter_map(|hint| match hint.key {
                HeaderKey::Chord(id) | HeaderKey::ChordTwice(id) => Some(id),
                HeaderKey::ChordPair(a, _) => Some(a),
                HeaderKey::Literal(_) => None,
            })
            .collect();
        for required in [
            "app.interrupt",
            "app.clear",
            "app.exit",
            "app.suspend",
            "app.thinking.cycle",
            "app.model.cycleForward",
            "app.model.select",
            "app.tools.expand",
            "app.header",
            "app.thinking.toggle",
            "app.editor.external",
            "app.message.followUp",
            "app.message.dequeue",
            "app.clipboard.pasteImage",
            "tui.editor.deleteToLineEnd",
        ] {
            assert!(ids.contains(&required), "{required} missing from {ids:?}");
        }
    }
}
