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
    /// Japanese.
    Ja,
    /// Korean.
    Ko,
    /// Spanish.
    Es,
    /// French.
    Fr,
    /// German.
    De,
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
            "ja" | "jp" => Some(Locale::Ja),
            "ko" | "kr" => Some(Locale::Ko),
            "es" => Some(Locale::Es),
            "fr" => Some(Locale::Fr),
            "de" => Some(Locale::De),
            _ => None,
        }
    }

    /// The BCP-47-ish code for this locale.
    pub fn code(self) -> &'static str {
        match self {
            Locale::En => "en",
            Locale::Zh => "zh",
            Locale::Ja => "ja",
            Locale::Ko => "ko",
            Locale::Es => "es",
            Locale::Fr => "fr",
            Locale::De => "de",
        }
    }

    /// Pick the copy for this locale (Martty's `tr(en, zh)`).
    ///
    /// Locales that haven't been translated yet fall back to the English
    /// column so adding a locale never produces a blank header — the row
    /// stays informative in the reader's own language or English, never
    /// empty.
    pub fn tr<'a>(self, en: &'a str, zh: &'a str) -> &'a str {
        match self {
            Locale::En => en,
            Locale::Zh => zh,
            // Fallback chain for the new locales. Rows that have a real
            // translation can call [`Locale::tr7`] with all seven columns
            // instead.
            Locale::Ja | Locale::Ko | Locale::Es | Locale::Fr | Locale::De => en,
        }
    }

    /// Pick the copy from a full seven-column row.
    ///
    /// `en` is the universal fallback: a locale that hasn't been
    /// translated yet reads the English copy. The 7-arg signature matches
    /// the 7 supported locales so a translation review can scan each row
    /// left-to-right in language order.
    pub fn tr7<'a>(
        self,
        en: &'a str,
        zh: &'a str,
        ja: &'a str,
        ko: &'a str,
        es: &'a str,
        fr: &'a str,
        de: &'a str,
    ) -> &'a str {
        match self {
            Locale::En => en,
            Locale::Zh => zh,
            Locale::Ja => ja,
            Locale::Ko => ko,
            Locale::Es => es,
            Locale::Fr => fr,
            Locale::De => de,
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
        use crate::components::keybindings::app_action_is_consumed;
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
        // One row for both reverse-search chords, like the model-cycle row:
        // the header's hint list is a fixed-height block above the transcript,
        // and a second row would cost the transcript one (LUM-1319 §3.4).
        key: HeaderKey::ChordPair("tui.editor.historySearch", "tui.editor.historySearchNext"),
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
/// Japanese rendering of [`HEADER_ONBOARDING_EN`].
pub const HEADER_ONBOARDING_JA: &str =
    "Pi は自身の機能を説明し、ドキュメントを参照できます。利用方法または拡張方法を尋ねてください。";
/// Korean rendering of [`HEADER_ONBOARDING_EN`].
pub const HEADER_ONBOARDING_KO: &str =
    "Pi는 자체 기능을 설명하고 문서를 검색할 수 있습니다. 사용법이나 확장 방법을 물어보세요.";
/// Spanish rendering of [`HEADER_ONBOARDING_EN`].
pub const HEADER_ONBOARDING_ES: &str =
    "Pi puede explicar sus propias funciones y consultar su documentación. Pregúntale cómo usarlo o extenderlo.";
/// French rendering of [`HEADER_ONBOARDING_EN`].
pub const HEADER_ONBOARDING_FR: &str =
    "Pi peut expliquer ses propres fonctionnalités et consulter sa documentation. Demandez-lui comment l'utiliser ou l'étendre.";
/// German rendering of [`HEADER_ONBOARDING_EN`].
pub const HEADER_ONBOARDING_DE: &str =
    "Pi kann seine eigenen Funktionen erklären und seine Dokumentation nachschlagen. Frag es, wie du es nutzen oder erweitern kannst.";

/// Pick the onboarding line for `locale`.
pub fn header_onboarding_line(locale: Locale) -> &'static str {
    locale.tr7(
        HEADER_ONBOARDING_EN,
        HEADER_ONBOARDING_ZH,
        HEADER_ONBOARDING_JA,
        HEADER_ONBOARDING_KO,
        HEADER_ONBOARDING_ES,
        HEADER_ONBOARDING_FR,
        HEADER_ONBOARDING_DE,
    )
}

/// Upstream's `compactOnboarding` line (`interactive-mode.ts:948`).
///
/// TS pi-tui prints this single row when the user has folded it
/// (`header_expanded = false`) so the surface still points at the chord that
/// brings the full startup help back. The Rust port reuses the same string
/// for both the user-collapsed case and the short-terminal fold — they are
/// the same state from the user's perspective: "the expanded help is not on
/// screen right now, here's how to get it".
pub const HEADER_COMPACT_ONBOARDING_EN_TEMPLATE: &str =
    "Press {keys} to show full startup help and loaded resources.";
/// Chinese rendering of [`HEADER_COMPACT_ONBOARDING_EN_TEMPLATE`].
pub const HEADER_COMPACT_ONBOARDING_ZH_TEMPLATE: &str = "按 {keys} 显示完整启动帮助与已加载资源";
/// Japanese rendering of [`HEADER_COMPACT_ONBOARDING_EN_TEMPLATE`].
pub const HEADER_COMPACT_ONBOARDING_JA_TEMPLATE: &str =
    "{keys} を押すと完全な起動ヘルプと読み込まれたリソースを表示します";
/// Korean rendering of [`HEADER_COMPACT_ONBOARDING_EN_TEMPLATE`].
pub const HEADER_COMPACT_ONBOARDING_KO_TEMPLATE: &str =
    "{keys} 를 눌러 전체 시작 도움말과 로드된 리소스를 표시하세요";
/// Spanish rendering of [`HEADER_COMPACT_ONBOARDING_EN_TEMPLATE`].
pub const HEADER_COMPACT_ONBOARDING_ES_TEMPLATE: &str =
    "Pulsa {keys} para mostrar la ayuda de inicio completa y los recursos cargados";
/// French rendering of [`HEADER_COMPACT_ONBOARDING_EN_TEMPLATE`].
pub const HEADER_COMPACT_ONBOARDING_FR_TEMPLATE: &str =
    "Appuyez sur {keys} pour afficher l'aide complète au démarrage et les ressources chargées";
/// German rendering of [`HEADER_COMPACT_ONBOARDING_EN_TEMPLATE`].
pub const HEADER_COMPACT_ONBOARDING_DE_TEMPLATE: &str =
    "Drücke {keys}, um die vollständige Starthilfe und geladene Ressourcen anzuzeigen";

/// Title of the `?` shortcut overlay (LUM-1464).
///
/// The overlay lists the same chords the startup header advertises, so the
/// copy names the surface the reader already knows rather than inventing a
/// second vocabulary; codex calls the surface `ShortcutOverlay`
/// (`bottom_pane/footer.rs`).
pub const SHORTCUT_OVERLAY_TITLE_EN: &str = "Keyboard shortcuts";
/// Chinese rendering of [`SHORTCUT_OVERLAY_TITLE_EN`].
pub const SHORTCUT_OVERLAY_TITLE_ZH: &str = "键盘快捷键";
/// Japanese rendering of [`SHORTCUT_OVERLAY_TITLE_EN`].
pub const SHORTCUT_OVERLAY_TITLE_JA: &str = "キーボードショートカット";
/// Korean rendering of [`SHORTCUT_OVERLAY_TITLE_EN`].
pub const SHORTCUT_OVERLAY_TITLE_KO: &str = "키보드 단축키";
/// Spanish rendering of [`SHORTCUT_OVERLAY_TITLE_EN`].
pub const SHORTCUT_OVERLAY_TITLE_ES: &str = "Atajos de teclado";
/// French rendering of [`SHORTCUT_OVERLAY_TITLE_EN`].
pub const SHORTCUT_OVERLAY_TITLE_FR: &str = "Raccourcis clavier";
/// German rendering of [`SHORTCUT_OVERLAY_TITLE_EN`].
pub const SHORTCUT_OVERLAY_TITLE_DE: &str = "Tastenkürzel";

/// Pick the shortcut-overlay title for `locale`.
pub fn shortcut_overlay_title(locale: Locale) -> &'static str {
    locale.tr7(
        SHORTCUT_OVERLAY_TITLE_EN,
        SHORTCUT_OVERLAY_TITLE_ZH,
        SHORTCUT_OVERLAY_TITLE_JA,
        SHORTCUT_OVERLAY_TITLE_KO,
        SHORTCUT_OVERLAY_TITLE_ES,
        SHORTCUT_OVERLAY_TITLE_FR,
        SHORTCUT_OVERLAY_TITLE_DE,
    )
}

/// How the `?` overlay is dismissed, printed on its title row.
///
/// Both chords are literal because both are hardcoded handlers (the `?`
/// toggle and `Escape`), not registry entries a rebind could move — the same
/// rule `/hotkeys` follows for its own literals.
pub const SHORTCUT_OVERLAY_CLOSE_EN: &str = "? / Esc to close";
/// Chinese rendering of [`SHORTCUT_OVERLAY_CLOSE_EN`].
pub const SHORTCUT_OVERLAY_CLOSE_ZH: &str = "? / Esc 关闭";
/// Japanese rendering of [`SHORTCUT_OVERLAY_CLOSE_EN`].
pub const SHORTCUT_OVERLAY_CLOSE_JA: &str = "? / Esc で閉じる";
/// Korean rendering of [`SHORTCUT_OVERLAY_CLOSE_EN`].
pub const SHORTCUT_OVERLAY_CLOSE_KO: &str = "? / Esc 로 닫기";
/// Spanish rendering of [`SHORTCUT_OVERLAY_CLOSE_EN`].
pub const SHORTCUT_OVERLAY_CLOSE_ES: &str = "? / Esc para cerrar";
/// French rendering of [`SHORTCUT_OVERLAY_CLOSE_EN`].
pub const SHORTCUT_OVERLAY_CLOSE_FR: &str = "? / Échap pour fermer";
/// German rendering of [`SHORTCUT_OVERLAY_CLOSE_EN`].
pub const SHORTCUT_OVERLAY_CLOSE_DE: &str = "? / Esc zum Schließen";

/// Pick the shortcut-overlay close hint for `locale`.
pub fn shortcut_overlay_close(locale: Locale) -> &'static str {
    locale.tr7(
        SHORTCUT_OVERLAY_CLOSE_EN,
        SHORTCUT_OVERLAY_CLOSE_ZH,
        SHORTCUT_OVERLAY_CLOSE_JA,
        SHORTCUT_OVERLAY_CLOSE_KO,
        SHORTCUT_OVERLAY_CLOSE_ES,
        SHORTCUT_OVERLAY_CLOSE_FR,
        SHORTCUT_OVERLAY_CLOSE_DE,
    )
}

/// Startup-header row shown when the header is folded: either because the
/// terminal is too short for the full hint list or because the user pressed
/// `app.header` to collapse it. Upstream uses the same single line for both
/// cases — its `compactOnboarding` row (`interactive-mode.ts:948`).
///
/// `keys` is the resolved `app.header` chord — the action that expands the
/// header again — so the row stays honest when the binding is overridden.
pub fn header_folded_line(locale: Locale, keys: &str) -> String {
    let template = locale.tr7(
        HEADER_COMPACT_ONBOARDING_EN_TEMPLATE,
        HEADER_COMPACT_ONBOARDING_ZH_TEMPLATE,
        HEADER_COMPACT_ONBOARDING_JA_TEMPLATE,
        HEADER_COMPACT_ONBOARDING_KO_TEMPLATE,
        HEADER_COMPACT_ONBOARDING_ES_TEMPLATE,
        HEADER_COMPACT_ONBOARDING_FR_TEMPLATE,
        HEADER_COMPACT_ONBOARDING_DE_TEMPLATE,
    );
    template.replace("{keys}", keys)
}

/// Startup-header copy for `--no-extensions`.
///
/// Kept in the `extensions: none` shape the audit pins, with the flag that
/// caused it in parentheses: the user who typed `--no-extensions` wanted
/// exactly that, and a silent header would read as "the flag did nothing".
pub const EXTENSIONS_DISABLED_EN: &str = "extensions: none (--no-extensions)";
/// Chinese rendering of [`EXTENSIONS_DISABLED_EN`].
pub const EXTENSIONS_DISABLED_ZH: &str = "扩展: 无（--no-extensions）";
/// Japanese rendering of [`EXTENSIONS_DISABLED_EN`].
pub const EXTENSIONS_DISABLED_JA: &str = "拡張: なし（--no-extensions）";
/// Korean rendering of [`EXTENSIONS_DISABLED_EN`].
pub const EXTENSIONS_DISABLED_KO: &str = "확장: 없음（--no-extensions）";
/// Spanish rendering of [`EXTENSIONS_DISABLED_EN`].
pub const EXTENSIONS_DISABLED_ES: &str = "extensiones: ninguna (--no-extensions)";
/// French rendering of [`EXTENSIONS_DISABLED_EN`].
pub const EXTENSIONS_DISABLED_FR: &str = "extensions : aucune (--no-extensions)";
/// German rendering of [`EXTENSIONS_DISABLED_EN`].
pub const EXTENSIONS_DISABLED_DE: &str = "Erweiterungen: keine (--no-extensions)";

/// Pick the `--no-extensions` line for `locale`.
pub fn extensions_disabled_line(locale: Locale) -> &'static str {
    locale.tr7(
        EXTENSIONS_DISABLED_EN,
        EXTENSIONS_DISABLED_ZH,
        EXTENSIONS_DISABLED_JA,
        EXTENSIONS_DISABLED_KO,
        EXTENSIONS_DISABLED_ES,
        EXTENSIONS_DISABLED_FR,
        EXTENSIONS_DISABLED_DE,
    )
}

/// The startup header's extension summary: `N extension(s): a.mjs, b.mjs`.
///
/// The caller (the App's built-in header) guarantees `names` is non-empty,
/// so the row never renders a count with nothing behind it.
pub fn extensions_summary_line(locale: Locale, count: usize, names: &[String]) -> String {
    let list = names.join(", ");
    let template = locale.tr7(
        "{count} extension(s): {list}",
        "{count} 个扩展: {list}",
        "拡張 {count} 個: {list}",
        "확장 {count} 개: {list}",
        "{count} extensión(es): {list}",
        "{count} extension(s) : {list}",
        "{count} Erweiterung(en): {list}",
    );
    template
        .replace("{count}", &count.to_string())
        .replace("{list}", &list)
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
        // The five new locales accept both the canonical two-letter code
        // and the older three-letter where one exists (Japanese `jp`,
        // Korean `kr` — common aliases in JVM locales).
        assert_eq!(Locale::parse("fr"), Some(Locale::Fr));
        assert_eq!(Locale::parse("de"), Some(Locale::De));
        assert_eq!(Locale::parse("es"), Some(Locale::Es));
        assert_eq!(Locale::parse("ja"), Some(Locale::Ja));
        assert_eq!(Locale::parse("ja-JP"), Some(Locale::Ja));
        assert_eq!(Locale::parse("jp"), Some(Locale::Ja));
        assert_eq!(Locale::parse("ko"), Some(Locale::Ko));
        assert_eq!(Locale::parse("kr"), Some(Locale::Ko));
        assert_eq!(Locale::parse(""), None);
        // Truly unsupported languages still return None.
        assert_eq!(Locale::parse("ru"), None);
        assert_eq!(Locale::parse("ar"), None);
    }

    #[test]
    fn code_returns_the_bcp47_subtag_for_every_locale() {
        // Round-trip every variant through `code()` and confirm `parse`
        // can read it back.
        for locale in [
            Locale::En,
            Locale::Zh,
            Locale::Ja,
            Locale::Ko,
            Locale::Es,
            Locale::Fr,
            Locale::De,
        ] {
            assert_eq!(Locale::parse(locale.code()), Some(locale));
        }
    }

    #[test]
    fn tr_picks_the_locale_column() {
        assert_eq!(Locale::En.tr("en", "zh"), "en");
        assert_eq!(Locale::Zh.tr("en", "zh"), "zh");
        assert_eq!(Locale::default(), Locale::En);
        assert_eq!(Locale::Zh.code(), "zh");
        // New locales fall back to English through the 2-arg `tr` (so
        // existing callers don't have to change) — the 7-arg `tr7`
        // returns the per-locale column instead.
        assert_eq!(Locale::Ja.tr("en", "zh"), "en");
        assert_eq!(Locale::Ko.tr("en", "zh"), "en");
        assert_eq!(Locale::Es.tr("en", "zh"), "en");
        assert_eq!(Locale::Fr.tr("en", "zh"), "en");
        assert_eq!(Locale::De.tr("en", "zh"), "en");
    }

    #[test]
    fn tr7_picks_every_locale_column() {
        // Every column gets its own copy; the 7-arg signature is the
        // explicit one a translation review reads top-to-bottom.
        let row = Locale::En.tr7("E", "Z", "J", "K", "S", "F", "G");
        assert_eq!(row, "E");
        assert_eq!(Locale::Zh.tr7("E", "Z", "J", "K", "S", "F", "G"), "Z");
        assert_eq!(Locale::Ja.tr7("E", "Z", "J", "K", "S", "F", "G"), "J");
        assert_eq!(Locale::Ko.tr7("E", "Z", "J", "K", "S", "F", "G"), "K");
        assert_eq!(Locale::Es.tr7("E", "Z", "J", "K", "S", "F", "G"), "S");
        assert_eq!(Locale::Fr.tr7("E", "Z", "J", "K", "S", "F", "G"), "F");
        assert_eq!(Locale::De.tr7("E", "Z", "J", "K", "S", "F", "G"), "G");
    }

    #[test]
    fn header_onboarding_line_localises_for_every_locale() {
        // Every locale must have a non-empty onboarding string. The audit
        // pins "Pi can explain" as the English substring; we check that
        // the other locales don't accidentally come back as the same
        // English text (that would mean a translation got overwritten by
        // the fallback chain).
        assert!(header_onboarding_line(Locale::En).contains("Pi can explain"));
        assert!(header_onboarding_line(Locale::Zh).contains("Pi 可以"));
        assert!(header_onboarding_line(Locale::Ja).contains("Pi は"));
        assert!(header_onboarding_line(Locale::Ko).contains("Pi는"));
        assert!(header_onboarding_line(Locale::Es).contains("Pi puede"));
        assert!(header_onboarding_line(Locale::Fr).contains("Pi peut"));
        assert!(header_onboarding_line(Locale::De).contains("Pi kann"));
    }

    #[test]
    fn header_folded_line_substitutes_keys_in_every_locale() {
        // The chord placeholder is a single `{keys}` token in every
        // template — a translator who adds extra braces (or strips the
        // placeholder entirely) surfaces here.
        for locale in [
            Locale::En,
            Locale::Zh,
            Locale::Ja,
            Locale::Ko,
            Locale::Es,
            Locale::Fr,
            Locale::De,
        ] {
            let line = header_folded_line(locale, "Ctrl+O");
            assert!(line.contains("Ctrl+O"), "{locale:?} missing substitution: {line:?}");
            assert!(!line.contains("{keys}"), "{locale:?} still has placeholder: {line:?}");
        }
    }

    #[test]
    fn shortcut_overlay_localises_title_and_close_in_every_locale() {
        // Title and close hint both differ across locales — passing the
        // same string in two languages means the table is missing one
        // row.
        for locale in [
            Locale::En,
            Locale::Zh,
            Locale::Ja,
            Locale::Ko,
            Locale::Es,
            Locale::Fr,
            Locale::De,
        ] {
            assert!(!shortcut_overlay_title(locale).is_empty());
            assert!(!shortcut_overlay_close(locale).is_empty());
        }
        assert_ne!(
            shortcut_overlay_title(Locale::En),
            shortcut_overlay_title(Locale::Zh),
        );
        assert_ne!(
            shortcut_overlay_title(Locale::En),
            shortcut_overlay_title(Locale::De),
        );
        assert_ne!(
            shortcut_overlay_close(Locale::En),
            shortcut_overlay_close(Locale::Ja),
        );
    }

    #[test]
    fn extensions_disabled_carries_the_flag_in_every_locale() {
        // The audit pins that `extensions: none (--no-extensions)` carries
        // both facts. Every locale has to surface the flag somewhere in
        // its line so the user understands what they did.
        for locale in [
            Locale::En,
            Locale::Zh,
            Locale::Ja,
            Locale::Ko,
            Locale::Es,
            Locale::Fr,
            Locale::De,
        ] {
            let line = extensions_disabled_line(locale);
            assert!(line.contains("--no-extensions"), "{locale:?} missing flag: {line:?}");
        }
    }

    #[test]
    fn extensions_summary_line_substitutes_count_and_list_for_every_locale() {
        // Same placeholder convention as `header_folded_line`: `{count}`
        // and `{list}` both have to land, and nothing should remain.
        let names = vec!["a.mjs".to_string(), "b.mjs".to_string()];
        for locale in [
            Locale::En,
            Locale::Zh,
            Locale::Ja,
            Locale::Ko,
            Locale::Es,
            Locale::Fr,
            Locale::De,
        ] {
            let line = extensions_summary_line(locale, 2, &names);
            assert!(line.contains("2"), "{locale:?} missing count: {line:?}");
            assert!(line.contains("a.mjs"), "{locale:?} missing list: {line:?}");
            assert!(line.contains("b.mjs"), "{locale:?} missing list: {line:?}");
            assert!(!line.contains("{count}"), "{locale:?} still has placeholder: {line:?}");
            assert!(!line.contains("{list}"), "{locale:?} still has placeholder: {line:?}");
        }
    }

    #[test]
    fn header_hint_description_falls_back_to_english_for_untranslated_locales() {
        // `HeaderHint` carries only the English and Chinese columns; the
        // five new locales see the English copy rather than an empty
        // string, so the header never disappears for a non-Zh reader.
        let hint = HeaderHint {
            key: HeaderKey::Literal("/"),
            en: "for commands",
            zh: "斜杠命令",
        };
        assert_eq!(hint.description(Locale::En), "for commands");
        assert_eq!(hint.description(Locale::Zh), "斜杠命令");
        assert_eq!(hint.description(Locale::Ja), "for commands");
        assert_eq!(hint.description(Locale::Ko), "for commands");
        assert_eq!(hint.description(Locale::Es), "for commands");
        assert_eq!(hint.description(Locale::Fr), "for commands");
        assert_eq!(hint.description(Locale::De), "for commands");
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
