//! Dependency-free syntax highlighting for fenced code blocks.
//!
//! Upstream renders markdown code through `highlight.js`
//! (`packages/coding-agent/src/utils/syntax-highlight.ts` +
//! `theme.ts::buildCliHighlightTheme`) and forwards the resulting HTML classes
//! into the `ThemeColor::Syntax*` slots. This crate cannot depend on
//! `highlight.js`, so this module re-implements a documented subset of that
//! pipeline:
//!
//! * a small lexer per language family (C-like, shell, Python, YAML-ish,
//!   markup, diff, …) that classifies runs of source into [`TokenKind`]s,
//! * the same class → [`ThemeColor`] mapping upstream builds
//!   (`keyword` → `syntaxKeyword`, `built_in` → `syntaxType`, `literal` →
//!   `syntaxNumber`, `comment` → `syntaxComment`, `meta` → `muted`,
//!   `addition`/`deletion` → `toolDiffAdded`/`toolDiffRemoved`, …),
//! * the same language set as the eager registration list upstream uses
//!   (`python`, `rust`, `typescript`, `bash`, …) plus the extensions
//!   `getLanguageFromPath` can produce.
//!
//! Deliberate deviations from `highlight.js` (documented rather than silently
//! assumed): nested sub-languages are not entered (```` ```html<script> ````,
//! template-literal interpolation, CSS inside `<style>`), and languages outside
//! the table below fall back to the un-highlighted code-block color, matching
//! upstream's behaviour for `lang` values `supportsLanguage` rejects.

use crate::styled::{SpanStyle, StyledLine, StyledSpan};
use crate::theme::ThemeColor;

/// Lexical class of a run of source text.
///
/// Each class maps onto exactly one [`ThemeColor`] slot, mirroring the class →
/// theme map upstream builds from the `highlight.js` output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// Unclassified text: plain identifiers, whitespace and stray characters.
    Plain,
    /// Language keyword (`fn`, `let`, `class`, `select`, …).
    Keyword,
    /// Built-in / primitive type name (`Vec`, `int`, `String`, …).
    Type,
    /// Literal constant (`true`, `null`, `nil`, `None`, …).
    Literal,
    /// Numeric literal, including hex and float forms.
    Number,
    /// String, template or character literal, delimiters included.
    String,
    /// Line or block comment, delimiters included.
    Comment,
    /// Identifier immediately followed by `(`.
    Function,
    /// Variable reference (`$HOME`, `@ivar`, `-flag`).
    Variable,
    /// Operator run (`->`, `::`, `&&`, `==`).
    Operator,
    /// Structural punctuation (`(`, `)`, `{`, `}`, `,`, `;`).
    Punctuation,
    /// Preprocessor directive, attribute, decorator or markup doctype.
    Meta,
    /// Added line inside a `diff`.
    Addition,
    /// Removed line inside a `diff`.
    Deletion,
}

impl TokenKind {
    /// Theme slot this class maps onto, or `None` to inherit the base style.
    fn slot(self) -> Option<ThemeColor> {
        match self {
            TokenKind::Plain => None,
            TokenKind::Keyword => Some(ThemeColor::SyntaxKeyword),
            TokenKind::Type => Some(ThemeColor::SyntaxType),
            TokenKind::Literal | TokenKind::Number => Some(ThemeColor::SyntaxNumber),
            TokenKind::String => Some(ThemeColor::SyntaxString),
            TokenKind::Comment => Some(ThemeColor::SyntaxComment),
            TokenKind::Function => Some(ThemeColor::SyntaxFunction),
            TokenKind::Variable => Some(ThemeColor::SyntaxVariable),
            TokenKind::Operator => Some(ThemeColor::SyntaxOperator),
            TokenKind::Punctuation => Some(ThemeColor::SyntaxPunctuation),
            TokenKind::Meta => Some(ThemeColor::Muted),
            TokenKind::Addition => Some(ThemeColor::ToolDiffAdded),
            TokenKind::Deletion => Some(ThemeColor::ToolDiffRemoved),
        }
    }
}

/// A classified run of source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// Classification of [`Token::text`].
    pub kind: TokenKind,
    /// Verbatim source text, newlines included.
    pub text: String,
}

/// Whether `language` is one of the languages this module can highlight.
///
/// Case-insensitive, and aware of the same short aliases upstream resolves
/// through `highlight.js` (`rs`, `py`, `ts`, `yml`, `sh`, …). Empty or unknown
/// names return `false`, which callers use to keep plain code-block styling.
pub fn supports_language(language: &str) -> bool {
    lookup(language).is_some()
}

/// Split `code` into classified [`Token`]s.
///
/// Returns `None` for languages this module does not support, so the caller can
/// fall back to plain rendering. The concatenation of every `Token::text` is
/// exactly `code`.
pub fn tokenize(code: &str, language: &str) -> Option<Vec<Token>> {
    lookup(language).map(|spec| scan(spec, code))
}

/// Highlight `code` into styled lines, ready to be pushed into a code block.
///
/// `language` is the fence info string, `base` the style every token inherits
/// (the markdown renderer passes the code-block color). Every token replaces
/// only the foreground of `base`, so callers keep their background/attributes.
///
/// The number of returned lines always equals `code.split('\n').count()`, even
/// when the language is unknown, so line numbering stays aligned with the
/// source the user wrote.
pub fn highlight_code(code: &str, language: Option<&str>, base: SpanStyle) -> Vec<StyledLine> {
    let tokens = match language.and_then(lookup) {
        Some(spec) => scan(spec, code),
        None => vec![Token {
            kind: TokenKind::Plain,
            text: code.to_string(),
        }],
    };

    let mut lines: Vec<StyledLine> = Vec::new();
    let mut current: StyledLine = Vec::new();
    for token in tokens {
        let style = match token.kind.slot() {
            Some(fg) => SpanStyle {
                fg: Some(fg),
                ..base
            },
            None => base,
        };
        for (index, part) in token.text.split('\n').enumerate() {
            if index > 0 {
                lines.push(std::mem::take(&mut current));
            }
            push_span(&mut current, part, style);
        }
    }
    lines.push(current);
    lines
}

/// Append `text` to `out`, merging into the previous span when the style
/// matches (empty runs are dropped, mirroring the markdown renderer).
fn push_span(out: &mut StyledLine, text: &str, style: SpanStyle) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut() {
        if last.style == style {
            last.text.push_str(text);
            return;
        }
    }
    out.push(StyledSpan::new(text.to_string(), style));
}

/// Append a classified run to `tokens`, merging with the previous same-class
/// run so the token stream stays compact.
fn push_token(out: &mut Vec<Token>, kind: TokenKind, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut() {
        if last.kind == kind {
            last.text.push_str(text);
            return;
        }
    }
    out.push(Token {
        kind,
        text: text.to_string(),
    });
}

fn collect(chars: &[char]) -> String {
    chars.iter().collect()
}

fn starts_with(chars: &[char], at: usize, needle: &str) -> bool {
    let mut actual = chars.iter().skip(at).copied();
    needle
        .chars()
        .all(|expected| actual.next() == Some(expected))
}

fn line_end(chars: &[char], start: usize) -> usize {
    let mut index = start;
    while index < chars.len() && chars[index] != '\n' {
        index += 1;
    }
    index
}

fn find_close(chars: &[char], from: usize, close: &str) -> Option<usize> {
    let close_len = close.chars().count();
    let mut index = from;
    while index < chars.len() {
        if starts_with(chars, index, close) {
            return Some(index + close_len);
        }
        index += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// Language table
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Quote {
    open: &'static str,
    close: &'static str,
    multiline: bool,
    escapes: bool,
}

const DOUBLE_QUOTE: Quote = Quote {
    open: "\"",
    close: "\"",
    multiline: false,
    escapes: true,
};

const SINGLE_QUOTE: Quote = Quote {
    open: "'",
    close: "'",
    multiline: false,
    escapes: true,
};

const SHELL_SINGLE_QUOTE: Quote = Quote {
    open: "'",
    close: "'",
    multiline: false,
    escapes: false,
};

const BACKTICK: Quote = Quote {
    open: "`",
    close: "`",
    multiline: true,
    escapes: true,
};

const TRIPLE_DOUBLE: Quote = Quote {
    open: "\"\"\"",
    close: "\"\"\"",
    multiline: true,
    escapes: true,
};

const TRIPLE_SINGLE: Quote = Quote {
    open: "'''",
    close: "'''",
    multiline: true,
    escapes: true,
};

const C_QUOTES: &[Quote] = &[DOUBLE_QUOTE, SINGLE_QUOTE];
const JS_QUOTES: &[Quote] = &[DOUBLE_QUOTE, SINGLE_QUOTE, BACKTICK];
const PY_QUOTES: &[Quote] = &[TRIPLE_DOUBLE, TRIPLE_SINGLE, DOUBLE_QUOTE, SINGLE_QUOTE];
const SHELL_QUOTES: &[Quote] = &[SHELL_SINGLE_QUOTE, DOUBLE_QUOTE, BACKTICK];

/// Static description of a language's lexical rules.
struct LanguageSpec {
    id: &'static str,
    aliases: &'static [&'static str],
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    quotes: &'static [Quote],
    /// Single-letter prefixes that may precede a quote (`f"…"`, `b'…'`).
    string_prefixes: &'static [char],
    /// Rust `r"…"` / `r#"…"#` raw strings.
    raw_strings: bool,
    /// Single quotes are character literals (and `'a` a Rust lifetime).
    char_literals: bool,
    keywords: &'static [&'static str],
    /// Keywords match case-insensitively (`SQL`).
    case_insensitive_keywords: bool,
    types: &'static [&'static str],
    literals: &'static [&'static str],
    variable_prefixes: &'static [char],
    /// `-x` / `--long` are variable references (shell CLIs).
    flag_args: bool,
    /// `#include`-style directives.
    preprocessor: bool,
    /// Rust `#[attr]` / `#![attr]`.
    attributes: bool,
    /// `@decorator` before an identifier.
    decorator: bool,
    /// Line-leading `key:` (YAML) or `key =` (TOML) separators.
    key_value: Option<char>,
    /// `[section]` headers (TOML/INI).
    ini_sections: bool,
    /// `<tag attr="value">` markup scanning.
    markup: bool,
    /// Unified-diff scanning.
    diff: bool,
    /// `#!/usr/bin/env …` first line is a comment.
    shebang: bool,
    /// `#aabbcc` color literals.
    hex_colors: bool,
}

const EMPTY: LanguageSpec = LanguageSpec {
    id: "",
    aliases: &[],
    line_comments: &[],
    block_comment: None,
    quotes: &[],
    string_prefixes: &[],
    raw_strings: false,
    char_literals: false,
    keywords: &[],
    case_insensitive_keywords: false,
    types: &[],
    literals: &[],
    variable_prefixes: &[],
    flag_args: false,
    preprocessor: false,
    attributes: false,
    decorator: false,
    key_value: None,
    ini_sections: false,
    markup: false,
    diff: false,
    shebang: false,
    hex_colors: false,
};

static LANGUAGES: &[LanguageSpec] = &[
    LanguageSpec {
        id: "rust",
        aliases: &["rs"],
        line_comments: &["//"],
        block_comment: Some(("/*", "*/")),
        quotes: &[DOUBLE_QUOTE],
        string_prefixes: &['b'],
        raw_strings: true,
        char_literals: true,
        keywords: &[
            "as", "async", "await", "box", "break", "const", "continue", "crate", "dyn", "else",
            "enum", "extern", "fn", "for", "if", "impl", "in", "let", "loop", "macro", "match",
            "mod", "move", "mut", "pub", "ref", "return", "self", "static", "struct", "super",
            "trait", "try", "type", "union", "unsafe", "use", "where", "while", "yield",
        ],
        types: &[
            "Self", "String", "Vec", "Option", "Result", "Box", "Rc", "Arc", "RefCell", "Cell",
            "Cow", "HashMap", "HashSet", "BTreeMap", "VecDeque", "Path", "PathBuf", "OsStr",
            "OsString", "bool", "char", "str", "f32", "f64", "i8", "i16", "i32", "i64", "i128",
            "isize", "u8", "u16", "u32", "u64", "u128", "usize",
        ],
        literals: &["true", "false"],
        attributes: true,
        ..EMPTY
    },
    LanguageSpec {
        id: "typescript",
        aliases: &["ts", "tsx", "javascript", "js", "jsx", "mjs", "cjs"],
        line_comments: &["//"],
        block_comment: Some(("/*", "*/")),
        quotes: JS_QUOTES,
        keywords: &[
            "abstract",
            "accessor",
            "any",
            "as",
            "assert",
            "async",
            "await",
            "bigint",
            "boolean",
            "break",
            "case",
            "catch",
            "class",
            "const",
            "constructor",
            "continue",
            "debugger",
            "declare",
            "default",
            "delete",
            "do",
            "else",
            "enum",
            "export",
            "extends",
            "false",
            "finally",
            "for",
            "from",
            "function",
            "get",
            "if",
            "implements",
            "import",
            "in",
            "infer",
            "instanceof",
            "interface",
            "is",
            "keyof",
            "let",
            "module",
            "namespace",
            "never",
            "new",
            "null",
            "number",
            "object",
            "of",
            "override",
            "package",
            "private",
            "protected",
            "public",
            "readonly",
            "require",
            "return",
            "satisfies",
            "set",
            "static",
            "string",
            "super",
            "switch",
            "symbol",
            "this",
            "throw",
            "true",
            "try",
            "type",
            "typeof",
            "undefined",
            "unknown",
            "var",
            "void",
            "while",
            "with",
            "yield",
        ],
        types: &[
            "Array", "Promise", "Record", "Map", "Set", "Date", "Error", "RegExp", "Partial",
            "Readonly", "Function", "Object", "Symbol", "BigInt", "Boolean", "Number", "String",
            "JSON", "Math", "console",
        ],
        literals: &["NaN", "Infinity"],
        decorator: true,
        ..EMPTY
    },
    LanguageSpec {
        id: "python",
        aliases: &["py"],
        line_comments: &["#"],
        quotes: PY_QUOTES,
        string_prefixes: &['r', 'R', 'b', 'B', 'f', 'F', 'u', 'U'],
        keywords: &[
            "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
            "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in",
            "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
            "with", "yield", "match", "case", "self", "cls",
        ],
        types: &[
            "Exception",
            "IndexError",
            "KeyError",
            "RuntimeError",
            "TypeError",
            "ValueError",
            "bytearray",
            "bytes",
            "complex",
            "dict",
            "enumerate",
            "float",
            "frozenset",
            "int",
            "len",
            "list",
            "object",
            "print",
            "range",
            "set",
            "str",
            "super",
            "tuple",
            "type",
            "zip",
            "bool",
        ],
        literals: &["True", "False", "None"],
        decorator: true,
        shebang: true,
        ..EMPTY
    },
    LanguageSpec {
        id: "go",
        aliases: &["golang"],
        line_comments: &["//"],
        block_comment: Some(("/*", "*/")),
        quotes: &[DOUBLE_QUOTE, BACKTICK],
        char_literals: true,
        keywords: &[
            "break",
            "case",
            "chan",
            "const",
            "continue",
            "default",
            "defer",
            "else",
            "fallthrough",
            "for",
            "func",
            "go",
            "goto",
            "if",
            "import",
            "interface",
            "map",
            "package",
            "range",
            "return",
            "select",
            "struct",
            "switch",
            "type",
            "var",
        ],
        types: &[
            "any",
            "append",
            "bool",
            "byte",
            "cap",
            "complex64",
            "complex128",
            "error",
            "float32",
            "float64",
            "int",
            "int8",
            "int16",
            "int32",
            "int64",
            "len",
            "make",
            "new",
            "panic",
            "print",
            "println",
            "recover",
            "rune",
            "string",
            "uint",
            "uint8",
            "uint16",
            "uint32",
            "uint64",
            "uintptr",
        ],
        literals: &["true", "false", "nil", "iota"],
        ..EMPTY
    },
    LanguageSpec {
        id: "c",
        aliases: &["h"],
        line_comments: &["//"],
        block_comment: Some(("/*", "*/")),
        quotes: C_QUOTES,
        string_prefixes: &['L', 'u', 'U'],
        char_literals: true,
        keywords: &[
            "auto", "break", "case", "const", "continue", "default", "do", "else", "enum",
            "extern", "for", "goto", "if", "inline", "register", "restrict", "return", "sizeof",
            "static", "struct", "switch", "typedef", "union", "volatile", "while",
        ],
        types: &[
            "_Bool", "bool", "char", "double", "float", "int", "int8_t", "int16_t", "int32_t",
            "int64_t", "long", "short", "signed", "size_t", "ssize_t", "uint8_t", "uint16_t",
            "uint32_t", "uint64_t", "unsigned", "void", "FILE",
        ],
        literals: &["NULL", "true", "false"],
        preprocessor: true,
        ..EMPTY
    },
    LanguageSpec {
        id: "cpp",
        aliases: &["cc", "c++", "cxx", "hpp", "hxx"],
        line_comments: &["//"],
        block_comment: Some(("/*", "*/")),
        quotes: C_QUOTES,
        string_prefixes: &['L', 'u', 'U'],
        char_literals: true,
        keywords: &[
            "alignas",
            "alignof",
            "auto",
            "break",
            "case",
            "catch",
            "class",
            "co_await",
            "co_return",
            "co_yield",
            "concept",
            "const",
            "constexpr",
            "const_cast",
            "continue",
            "decltype",
            "default",
            "delete",
            "do",
            "dynamic_cast",
            "else",
            "enum",
            "explicit",
            "export",
            "extern",
            "final",
            "for",
            "friend",
            "goto",
            "if",
            "inline",
            "mutable",
            "namespace",
            "new",
            "noexcept",
            "operator",
            "override",
            "private",
            "protected",
            "public",
            "register",
            "reinterpret_cast",
            "requires",
            "return",
            "sizeof",
            "static",
            "static_assert",
            "static_cast",
            "struct",
            "switch",
            "template",
            "this",
            "thread_local",
            "throw",
            "try",
            "typedef",
            "typeid",
            "typename",
            "union",
            "using",
            "virtual",
            "volatile",
            "while",
        ],
        types: &[
            "bool",
            "char",
            "double",
            "float",
            "int",
            "long",
            "short",
            "signed",
            "size_t",
            "string",
            "unique_ptr",
            "shared_ptr",
            "unsigned",
            "vector",
            "void",
            "wchar_t",
        ],
        literals: &["NULL", "nullptr", "true", "false"],
        preprocessor: true,
        ..EMPTY
    },
    LanguageSpec {
        id: "java",
        aliases: &[],
        line_comments: &["//"],
        block_comment: Some(("/*", "*/")),
        quotes: C_QUOTES,
        char_literals: true,
        keywords: &[
            "abstract",
            "assert",
            "break",
            "case",
            "catch",
            "class",
            "const",
            "continue",
            "default",
            "do",
            "else",
            "enum",
            "extends",
            "final",
            "finally",
            "for",
            "goto",
            "if",
            "implements",
            "import",
            "instanceof",
            "interface",
            "native",
            "new",
            "package",
            "permits",
            "private",
            "protected",
            "public",
            "record",
            "return",
            "sealed",
            "static",
            "strictfp",
            "super",
            "switch",
            "synchronized",
            "this",
            "throw",
            "throws",
            "transient",
            "try",
            "var",
            "volatile",
            "while",
            "yield",
        ],
        types: &[
            "ArrayList",
            "Boolean",
            "Double",
            "Exception",
            "Float",
            "HashMap",
            "Integer",
            "List",
            "Long",
            "Map",
            "Object",
            "Optional",
            "RuntimeException",
            "Set",
            "String",
            "boolean",
            "byte",
            "char",
            "double",
            "float",
            "int",
            "long",
            "short",
            "void",
        ],
        literals: &["true", "false", "null"],
        decorator: true,
        ..EMPTY
    },
    LanguageSpec {
        id: "csharp",
        aliases: &["cs", "c#"],
        line_comments: &["//"],
        block_comment: Some(("/*", "*/")),
        quotes: C_QUOTES,
        string_prefixes: &['$', '@'],
        char_literals: true,
        keywords: &[
            "abstract",
            "as",
            "async",
            "await",
            "base",
            "break",
            "case",
            "catch",
            "checked",
            "class",
            "const",
            "continue",
            "default",
            "delegate",
            "do",
            "else",
            "enum",
            "event",
            "explicit",
            "extern",
            "finally",
            "fixed",
            "for",
            "foreach",
            "goto",
            "if",
            "implicit",
            "in",
            "interface",
            "internal",
            "is",
            "lock",
            "namespace",
            "new",
            "operator",
            "out",
            "override",
            "params",
            "private",
            "protected",
            "public",
            "readonly",
            "ref",
            "return",
            "sealed",
            "sizeof",
            "stackalloc",
            "static",
            "struct",
            "switch",
            "this",
            "throw",
            "try",
            "typeof",
            "unchecked",
            "unsafe",
            "using",
            "var",
            "virtual",
            "volatile",
            "when",
            "where",
            "while",
            "yield",
        ],
        types: &[
            "Action",
            "Boolean",
            "Console",
            "Dictionary",
            "Double",
            "Func",
            "IEnumerable",
            "Int32",
            "List",
            "Object",
            "String",
            "Task",
            "bool",
            "byte",
            "char",
            "decimal",
            "double",
            "dynamic",
            "float",
            "int",
            "long",
            "object",
            "sbyte",
            "short",
            "string",
            "uint",
            "ulong",
            "ushort",
            "void",
        ],
        literals: &["true", "false", "null"],
        ..EMPTY
    },
    LanguageSpec {
        id: "kotlin",
        aliases: &["kt", "kts"],
        line_comments: &["//"],
        block_comment: Some(("/*", "*/")),
        quotes: &[TRIPLE_DOUBLE, DOUBLE_QUOTE, SINGLE_QUOTE],
        char_literals: true,
        keywords: &[
            "abstract",
            "actual",
            "as",
            "break",
            "by",
            "catch",
            "class",
            "companion",
            "const",
            "constructor",
            "continue",
            "crossinline",
            "data",
            "do",
            "else",
            "enum",
            "expect",
            "external",
            "final",
            "finally",
            "for",
            "fun",
            "get",
            "if",
            "in",
            "infix",
            "init",
            "inline",
            "interface",
            "internal",
            "is",
            "lateinit",
            "noinline",
            "object",
            "open",
            "operator",
            "out",
            "override",
            "package",
            "private",
            "protected",
            "public",
            "reified",
            "return",
            "sealed",
            "set",
            "super",
            "suspend",
            "tailrec",
            "this",
            "throw",
            "try",
            "typealias",
            "val",
            "var",
            "vararg",
            "when",
            "where",
            "while",
        ],
        types: &[
            "Any",
            "Array",
            "Boolean",
            "Byte",
            "Char",
            "Double",
            "Float",
            "Int",
            "List",
            "Long",
            "Map",
            "MutableList",
            "Nothing",
            "Set",
            "Short",
            "String",
            "Unit",
        ],
        literals: &["true", "false", "null"],
        ..EMPTY
    },
    LanguageSpec {
        id: "swift",
        aliases: &[],
        line_comments: &["//"],
        block_comment: Some(("/*", "*/")),
        quotes: &[TRIPLE_DOUBLE, DOUBLE_QUOTE],
        char_literals: true,
        keywords: &[
            "actor",
            "any",
            "as",
            "associatedtype",
            "async",
            "await",
            "break",
            "case",
            "catch",
            "class",
            "continue",
            "convenience",
            "default",
            "defer",
            "deinit",
            "do",
            "dynamic",
            "else",
            "enum",
            "extension",
            "fallthrough",
            "fileprivate",
            "final",
            "for",
            "func",
            "get",
            "guard",
            "if",
            "import",
            "in",
            "indirect",
            "init",
            "inout",
            "internal",
            "is",
            "lazy",
            "let",
            "mutating",
            "open",
            "operator",
            "optional",
            "override",
            "private",
            "protocol",
            "public",
            "repeat",
            "required",
            "return",
            "self",
            "set",
            "some",
            "static",
            "struct",
            "subscript",
            "super",
            "switch",
            "throw",
            "throws",
            "try",
            "typealias",
            "var",
            "weak",
            "where",
            "while",
            "willSet",
            "didSet",
        ],
        types: &[
            "Any",
            "AnyObject",
            "Array",
            "Bool",
            "Character",
            "ClosedRange",
            "Dictionary",
            "Double",
            "Error",
            "Float",
            "Int",
            "Int8",
            "Int16",
            "Int32",
            "Int64",
            "Optional",
            "Range",
            "Result",
            "Set",
            "String",
            "UInt",
            "Void",
        ],
        literals: &["true", "false", "nil"],
        ..EMPTY
    },
    LanguageSpec {
        id: "scala",
        aliases: &[],
        line_comments: &["//"],
        block_comment: Some(("/*", "*/")),
        quotes: &[TRIPLE_DOUBLE, DOUBLE_QUOTE, SINGLE_QUOTE],
        char_literals: true,
        keywords: &[
            "abstract",
            "case",
            "catch",
            "class",
            "def",
            "derives",
            "do",
            "else",
            "enum",
            "export",
            "extends",
            "extension",
            "final",
            "finally",
            "for",
            "forSome",
            "given",
            "if",
            "implicit",
            "import",
            "inline",
            "lazy",
            "match",
            "new",
            "object",
            "opaque",
            "override",
            "package",
            "private",
            "protected",
            "return",
            "sealed",
            "super",
            "then",
            "this",
            "throw",
            "trait",
            "transparent",
            "try",
            "type",
            "using",
            "val",
            "var",
            "while",
            "with",
            "yield",
        ],
        types: &[
            "Any", "AnyRef", "Boolean", "Byte", "Char", "Double", "Either", "Float", "Future",
            "Int", "List", "Long", "Map", "Nothing", "Option", "Seq", "Set", "Short", "String",
            "Unit",
        ],
        literals: &["true", "false", "null"],
        ..EMPTY
    },
    LanguageSpec {
        id: "ruby",
        aliases: &["rb"],
        line_comments: &["#"],
        quotes: &[DOUBLE_QUOTE, SINGLE_QUOTE],
        variable_prefixes: &['@', '$'],
        keywords: &[
            "alias",
            "and",
            "attr_accessor",
            "attr_reader",
            "attr_writer",
            "begin",
            "break",
            "case",
            "class",
            "def",
            "defined?",
            "do",
            "else",
            "elsif",
            "end",
            "ensure",
            "extend",
            "for",
            "if",
            "in",
            "include",
            "lambda",
            "module",
            "next",
            "not",
            "or",
            "prepend",
            "proc",
            "puts",
            "print",
            "raise",
            "redo",
            "require",
            "require_relative",
            "rescue",
            "retry",
            "return",
            "self",
            "super",
            "then",
            "undef",
            "unless",
            "until",
            "when",
            "while",
            "yield",
        ],
        types: &[
            "Array",
            "Exception",
            "FalseClass",
            "File",
            "Float",
            "Hash",
            "IO",
            "Integer",
            "Module",
            "NilClass",
            "Object",
            "String",
            "Struct",
            "Symbol",
            "Time",
            "TrueClass",
        ],
        literals: &["true", "false", "nil"],
        shebang: true,
        ..EMPTY
    },
    LanguageSpec {
        id: "php",
        aliases: &[],
        line_comments: &["//", "#"],
        block_comment: Some(("/*", "*/")),
        quotes: &[DOUBLE_QUOTE, SINGLE_QUOTE],
        variable_prefixes: &['$'],
        keywords: &[
            "abstract",
            "and",
            "array",
            "as",
            "break",
            "callable",
            "case",
            "catch",
            "class",
            "clone",
            "const",
            "continue",
            "declare",
            "default",
            "do",
            "echo",
            "else",
            "elseif",
            "empty",
            "endforeach",
            "endif",
            "enum",
            "extends",
            "final",
            "finally",
            "fn",
            "for",
            "foreach",
            "function",
            "global",
            "goto",
            "if",
            "implements",
            "include",
            "include_once",
            "instanceof",
            "insteadof",
            "interface",
            "isset",
            "list",
            "match",
            "namespace",
            "new",
            "or",
            "print",
            "private",
            "protected",
            "public",
            "readonly",
            "require",
            "require_once",
            "return",
            "static",
            "switch",
            "throw",
            "trait",
            "try",
            "unset",
            "use",
            "var",
            "while",
            "xor",
            "yield",
        ],
        types: &[
            "bool", "float", "int", "iterable", "mixed", "object", "string", "void",
        ],
        literals: &["true", "false", "null"],
        ..EMPTY
    },
    LanguageSpec {
        id: "lua",
        aliases: &[],
        line_comments: &["--"],
        quotes: &[DOUBLE_QUOTE, SINGLE_QUOTE],
        keywords: &[
            "and", "break", "do", "else", "elseif", "end", "for", "function", "goto", "if", "in",
            "local", "not", "or", "repeat", "return", "then", "until", "while",
        ],
        types: &[
            "assert",
            "coroutine",
            "debug",
            "error",
            "io",
            "ipairs",
            "math",
            "os",
            "pairs",
            "pcall",
            "print",
            "require",
            "self",
            "setmetatable",
            "string",
            "table",
            "tonumber",
            "tostring",
            "type",
        ],
        literals: &["true", "false", "nil"],
        ..EMPTY
    },
    LanguageSpec {
        id: "perl",
        aliases: &["pl", "pm"],
        line_comments: &["#"],
        quotes: &[DOUBLE_QUOTE, SINGLE_QUOTE],
        variable_prefixes: &['$', '@', '%'],
        shebang: true,
        keywords: &[
            "and",
            "bless",
            "chomp",
            "chop",
            "cmp",
            "defined",
            "delete",
            "die",
            "do",
            "each",
            "else",
            "elsif",
            "eq",
            "exists",
            "for",
            "foreach",
            "ge",
            "grep",
            "gt",
            "if",
            "join",
            "keys",
            "last",
            "le",
            "local",
            "lt",
            "map",
            "my",
            "ne",
            "next",
            "not",
            "or",
            "our",
            "package",
            "pop",
            "print",
            "push",
            "redo",
            "ref",
            "require",
            "return",
            "reverse",
            "say",
            "scalar",
            "shift",
            "sort",
            "splice",
            "split",
            "sub",
            "tr",
            "undef",
            "unless",
            "unshift",
            "until",
            "use",
            "values",
            "warn",
            "while",
            "wantarray",
        ],
        types: &[],
        literals: &["undef"],
        ..EMPTY
    },
    LanguageSpec {
        id: "bash",
        aliases: &["sh", "shell", "zsh", "console"],
        line_comments: &["#"],
        quotes: SHELL_QUOTES,
        variable_prefixes: &['$'],
        flag_args: true,
        shebang: true,
        keywords: &[
            "alias", "break", "case", "continue", "coproc", "declare", "do", "done", "elif",
            "else", "esac", "exit", "export", "fi", "for", "function", "if", "in", "local",
            "readonly", "return", "select", "shift", "source", "then", "time", "typeset",
            "unalias", "unset", "until", "while",
        ],
        types: &[
            "awk", "cat", "cd", "chmod", "chown", "cp", "curl", "echo", "eval", "exec", "false",
            "grep", "head", "kill", "ls", "mkdir", "mv", "printf", "pwd", "read", "rm", "sed",
            "seq", "set", "sleep", "sort", "tail", "test", "touch", "tr", "trap", "true", "uniq",
            "wait", "wc", "which", "xargs",
        ],
        literals: &[],
        ..EMPTY
    },
    LanguageSpec {
        id: "sql",
        aliases: &[],
        line_comments: &["--"],
        block_comment: Some(("/*", "*/")),
        quotes: &[SINGLE_QUOTE, DOUBLE_QUOTE],
        case_insensitive_keywords: true,
        keywords: &[
            "ADD",
            "ALL",
            "ALTER",
            "ANALYZE",
            "AND",
            "AS",
            "ASC",
            "BEGIN",
            "BETWEEN",
            "BY",
            "CASE",
            "CAST",
            "CHECK",
            "COALESCE",
            "COMMIT",
            "CONSTRAINT",
            "COUNT",
            "CREATE",
            "DEFAULT",
            "DELETE",
            "DESC",
            "DISTINCT",
            "DROP",
            "ELSE",
            "END",
            "EXISTS",
            "EXPLAIN",
            "FOREIGN",
            "FROM",
            "FULL",
            "GROUP",
            "HAVING",
            "IF",
            "IN",
            "INDEX",
            "INNER",
            "INSERT",
            "INTO",
            "IS",
            "JOIN",
            "KEY",
            "LEFT",
            "LIKE",
            "LIMIT",
            "MAX",
            "MIN",
            "NOT",
            "NULL",
            "OFFSET",
            "ON",
            "OR",
            "ORDER",
            "OUTER",
            "PRIMARY",
            "REFERENCES",
            "REPLACE",
            "RETURNING",
            "RIGHT",
            "ROLLBACK",
            "SELECT",
            "SET",
            "SUM",
            "TABLE",
            "THEN",
            "TRANSACTION",
            "TRUNCATE",
            "UNION",
            "UNIQUE",
            "UPDATE",
            "VALUES",
            "VIEW",
            "WHEN",
            "WHERE",
            "WITH",
            "AVG",
        ],
        types: &[
            "BIGINT",
            "BOOLEAN",
            "DATE",
            "INT",
            "INTEGER",
            "JSON",
            "JSONB",
            "NUMERIC",
            "SERIAL",
            "TEXT",
            "TIMESTAMP",
            "UUID",
            "VARCHAR",
        ],
        literals: &["NULL", "TRUE", "FALSE"],
        ..EMPTY
    },
    LanguageSpec {
        id: "json",
        aliases: &["jsonc"],
        line_comments: &[],
        quotes: &[DOUBLE_QUOTE],
        literals: &["true", "false", "null"],
        ..EMPTY
    },
    LanguageSpec {
        id: "yaml",
        aliases: &["yml"],
        line_comments: &["#"],
        quotes: &[DOUBLE_QUOTE, SINGLE_QUOTE],
        key_value: Some(':'),
        literals: &["true", "false", "null", "yes", "no", "on", "off", "~"],
        ..EMPTY
    },
    LanguageSpec {
        id: "toml",
        aliases: &["ini", "conf"],
        line_comments: &["#"],
        quotes: &[TRIPLE_DOUBLE, DOUBLE_QUOTE, SINGLE_QUOTE],
        key_value: Some('='),
        ini_sections: true,
        literals: &["true", "false"],
        ..EMPTY
    },
    LanguageSpec {
        id: "html",
        aliases: &["htm", "xml", "xhtml", "svg", "vue"],
        quotes: &[],
        markup: true,
        ..EMPTY
    },
    LanguageSpec {
        id: "css",
        aliases: &["scss", "sass", "less"],
        block_comment: Some(("/*", "*/")),
        quotes: &[DOUBLE_QUOTE, SINGLE_QUOTE],
        hex_colors: true,
        ..EMPTY
    },
    LanguageSpec {
        id: "markdown",
        aliases: &["md"],
        line_comments: &[],
        quotes: &[],
        ..EMPTY
    },
    LanguageSpec {
        id: "diff",
        aliases: &["patch"],
        diff: true,
        ..EMPTY
    },
];

fn lookup(language: &str) -> Option<&'static LanguageSpec> {
    let name = language.trim();
    if name.is_empty() {
        return None;
    }
    let lower = name.to_ascii_lowercase();
    LANGUAGES
        .iter()
        .find(|spec| spec.id == lower || spec.aliases.iter().any(|alias| *alias == lower))
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

fn scan(spec: &LanguageSpec, source: &str) -> Vec<Token> {
    let chars: Vec<char> = source.chars().collect();
    if spec.diff {
        return scan_diff(&chars);
    }
    if spec.markup {
        return scan_markup(&chars);
    }

    let mut out: Vec<Token> = Vec::new();
    let mut index = 0usize;
    let mut line_start = true;
    while index < chars.len() {
        let current = chars[index];

        if current == '\n' {
            push_token(&mut out, TokenKind::Plain, "\n");
            index += 1;
            line_start = true;
            continue;
        }
        if matches!(current, ' ' | '\t' | '\r') {
            let start = index;
            while index < chars.len() && matches!(chars[index], ' ' | '\t' | '\r') {
                index += 1;
            }
            push_token(&mut out, TokenKind::Plain, &collect(&chars[start..index]));
            continue;
        }

        if line_start {
            if spec.shebang && current == '#' && chars.get(index + 1) == Some(&'!') {
                let end = line_end(&chars, index);
                push_token(&mut out, TokenKind::Comment, &collect(&chars[index..end]));
                index = end;
                continue;
            }
            if spec.preprocessor && current == '#' {
                let start = index;
                let mut end = index + 1;
                while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
                    end += 1;
                }
                push_token(&mut out, TokenKind::Meta, &collect(&chars[start..end]));
                index = end;
                line_start = false;
                continue;
            }
            if spec.attributes
                && current == '#'
                && matches!(chars.get(index + 1), Some('[') | Some('!'))
            {
                let end = scan_attribute(&chars, index);
                push_token(&mut out, TokenKind::Meta, &collect(&chars[index..end]));
                index = end;
                line_start = false;
                continue;
            }
            if let Some(separator) = spec.key_value {
                if current == '-' && chars.get(index + 1) == Some(&' ') {
                    // YAML list marker: the key that follows is still line-leading.
                    push_token(&mut out, TokenKind::Punctuation, "-");
                    index += 1;
                    continue;
                }
                if let Some((end, kind)) = scan_key(&chars, index, separator, spec.ini_sections) {
                    push_token(&mut out, kind, &collect(&chars[index..end]));
                    index = end;
                    line_start = false;
                    continue;
                }
            }
        }
        line_start = false;

        if spec
            .line_comments
            .iter()
            .any(|prefix| starts_with(&chars, index, prefix))
        {
            // `--` in Lua is a line comment, but `--[[` opens a block.
            if spec.id == "lua" && starts_with(&chars, index, "--[[") {
                let end = find_close(&chars, index + 4, "]]").unwrap_or(chars.len());
                push_token(&mut out, TokenKind::Comment, &collect(&chars[index..end]));
                index = end;
                continue;
            }
            let end = line_end(&chars, index);
            push_token(&mut out, TokenKind::Comment, &collect(&chars[index..end]));
            index = end;
            continue;
        }
        if let Some((open, close)) = spec.block_comment {
            if starts_with(&chars, index, open) {
                let from = index + open.chars().count();
                let end = find_close(&chars, from, close).unwrap_or(chars.len());
                push_token(&mut out, TokenKind::Comment, &collect(&chars[index..end]));
                index = end;
                continue;
            }
        }
        if let Some(string) = match_string_start(spec, &chars, index) {
            let end = string.scan(&chars, index);
            push_token(&mut out, TokenKind::String, &collect(&chars[index..end]));
            index = end;
            continue;
        }
        if spec.variable_prefixes.contains(&current) {
            let start = index;
            let mut end = index + 1;
            while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
                end += 1;
            }
            if end == start + 1 {
                push_token(&mut out, TokenKind::Punctuation, &current.to_string());
                index += 1;
            } else {
                push_token(&mut out, TokenKind::Variable, &collect(&chars[start..end]));
                index = end;
            }
            continue;
        }
        if spec.decorator && current == '@' {
            let start = index;
            let mut end = index + 1;
            while end < chars.len()
                && (chars[end].is_alphanumeric() || matches!(chars[end], '_' | '.'))
            {
                end += 1;
            }
            push_token(&mut out, TokenKind::Meta, &collect(&chars[start..end]));
            index = end;
            continue;
        }
        if spec.hex_colors
            && current == '#'
            && chars.get(index + 1).is_some_and(|c| c.is_ascii_hexdigit())
        {
            let start = index;
            let mut end = index + 1;
            while end < chars.len() && chars[end].is_ascii_hexdigit() {
                end += 1;
            }
            push_token(&mut out, TokenKind::Number, &collect(&chars[start..end]));
            index = end;
            continue;
        }
        if current.is_ascii_digit() {
            let end = scan_number(&chars, index);
            push_token(&mut out, TokenKind::Number, &collect(&chars[index..end]));
            index = end;
            continue;
        }
        if spec.flag_args && current == '-' && is_flag_start(&chars, index) {
            let start = index;
            let mut end = index;
            while end < chars.len() && (chars[end] == '-' || chars[end].is_alphanumeric()) {
                end += 1;
            }
            push_token(&mut out, TokenKind::Variable, &collect(&chars[start..end]));
            index = end;
            continue;
        }
        if is_ident_start(current) {
            let start = index;
            let mut end = index;
            while end < chars.len() && is_ident_char(chars[end]) {
                end += 1;
            }
            let word = collect(&chars[start..end]);
            let kind = classify(spec, &word, &chars, end);
            push_token(&mut out, kind, &word);
            index = end;
            continue;
        }
        if is_operator(current) {
            let start = index;
            while index < chars.len() && is_operator(chars[index]) {
                index += 1;
            }
            push_token(
                &mut out,
                TokenKind::Operator,
                &collect(&chars[start..index]),
            );
            continue;
        }

        let kind = if is_punctuation(current) {
            TokenKind::Punctuation
        } else {
            TokenKind::Plain
        };
        push_token(&mut out, kind, &current.to_string());
        index += 1;
    }
    out
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '$'
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '$')
}

fn is_operator(c: char) -> bool {
    matches!(
        c,
        '+' | '-'
            | '*'
            | '/'
            | '%'
            | '='
            | '<'
            | '>'
            | '!'
            | '&'
            | '|'
            | '^'
            | '~'
            | '?'
            | ':'
            | '@'
            | '#'
    )
}

fn is_punctuation(c: char) -> bool {
    matches!(
        c,
        '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | '.' | '\\'
    )
}

fn is_flag_start(chars: &[char], index: usize) -> bool {
    match chars.get(index + 1) {
        Some(next) if next.is_alphabetic() => true,
        Some('-') => chars.get(index + 2).is_some_and(|c| c.is_alphabetic()),
        _ => false,
    }
}

fn scan_number(chars: &[char], start: usize) -> usize {
    let mut index = start;
    while index < chars.len() {
        let current = chars[index];
        if current == '.' {
            // Stop before `..`/`..=` so ranges keep their own tokens.
            if chars.get(index + 1) == Some(&'.') || (index > start && chars[index - 1] == '.') {
                break;
            }
            index += 1;
            continue;
        }
        if current.is_ascii_alphanumeric() || current == '_' {
            index += 1;
            continue;
        }
        if matches!(current, '+' | '-')
            && index > start
            && matches!(chars[index - 1], 'e' | 'E' | 'p' | 'P')
        {
            index += 1;
            continue;
        }
        break;
    }
    index
}

/// Rust `#[derive(Debug)]`: scan to the matching `]` so the whole attribute is
/// one `meta` token.
fn scan_attribute(chars: &[char], start: usize) -> usize {
    let mut depth = 0usize;
    let mut index = start;
    while index < chars.len() {
        match chars[index] {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return index + 1;
                }
            }
            '\n' if depth == 0 => return index,
            _ => {}
        }
        index += 1;
    }
    chars.len()
}

fn matches_word(list: &[&'static str], word: &str, case_insensitive: bool) -> bool {
    list.iter().any(|candidate| {
        if case_insensitive {
            candidate.eq_ignore_ascii_case(word)
        } else {
            *candidate == word
        }
    })
}

fn classify(spec: &LanguageSpec, word: &str, chars: &[char], after: usize) -> TokenKind {
    let case_insensitive = spec.case_insensitive_keywords;
    if matches_word(spec.keywords, word, case_insensitive) {
        return TokenKind::Keyword;
    }
    if matches_word(spec.literals, word, case_insensitive) {
        return TokenKind::Literal;
    }
    if matches_word(spec.types, word, case_insensitive) {
        return TokenKind::Type;
    }
    let mut index = after;
    while index < chars.len() && chars[index] == ' ' {
        index += 1;
    }
    if chars.get(index) == Some(&'(') {
        return TokenKind::Function;
    }
    TokenKind::Plain
}

struct StringStart {
    prefix_len: usize,
    open_len: usize,
    close: String,
    multiline: bool,
    escapes: bool,
}

impl StringStart {
    /// Index of the first character after the closing delimiter (`chars.len()`
    /// when the literal is unterminated).
    fn scan(&self, chars: &[char], start: usize) -> usize {
        let close_len = self.close.chars().count();
        let mut index = start + self.prefix_len + self.open_len;
        while index < chars.len() {
            let current = chars[index];
            if self.escapes && current == '\\' {
                index += 2;
                continue;
            }
            if !self.multiline && current == '\n' {
                return index;
            }
            if starts_with(chars, index, &self.close) {
                return index + close_len;
            }
            index += 1;
        }
        chars.len()
    }
}

fn match_string_start(spec: &LanguageSpec, chars: &[char], index: usize) -> Option<StringStart> {
    if spec.raw_strings {
        let mut prefix = 0usize;
        if chars.get(index) == Some(&'b') {
            prefix += 1;
        }
        if chars.get(index + prefix) == Some(&'r') {
            let mut hashes_at = index + prefix + 1;
            let mut hashes = 0usize;
            while chars.get(hashes_at) == Some(&'#') {
                hashes += 1;
                hashes_at += 1;
            }
            if chars.get(hashes_at) == Some(&'"') {
                return Some(StringStart {
                    prefix_len: prefix + 1 + hashes,
                    open_len: 1,
                    close: format!("\"{}", "#".repeat(hashes)),
                    multiline: true,
                    escapes: false,
                });
            }
        }
    }

    let mut prefix_len = 0usize;
    while prefix_len < 3
        && chars
            .get(index + prefix_len)
            .is_some_and(|c| spec.string_prefixes.contains(c))
    {
        prefix_len += 1;
    }
    let quote_at = index + prefix_len;
    let quote = spec
        .quotes
        .iter()
        .find(|quote| starts_with(chars, quote_at, quote.open))?;

    if spec.char_literals
        && quote.open == "'"
        && prefix_len == 0
        && !looks_like_char_literal(chars, index)
    {
        return None;
    }

    let raw_prefix = spec
        .string_prefixes
        .iter()
        .take(prefix_len)
        .any(|prefix| *prefix == 'r' || *prefix == 'R');

    Some(StringStart {
        prefix_len,
        open_len: quote.open.chars().count(),
        close: quote.close.to_string(),
        multiline: quote.multiline,
        escapes: quote.escapes && !raw_prefix,
    })
}

/// Distinguish Rust/Java/C `'a'`/`'\n'` character literals from Rust `'a`
/// lifetimes (which must be left unhighlighted, as upstream's grammar does).
fn looks_like_char_literal(chars: &[char], index: usize) -> bool {
    match chars.get(index + 1) {
        Some('\\') => true,
        Some(_) => chars.get(index + 2) == Some(&'\''),
        None => false,
    }
}

/// `key:` (YAML) / `key =` (TOML) at the start of a line, plus `[section]`.
fn scan_key(
    chars: &[char],
    index: usize,
    separator: char,
    ini_sections: bool,
) -> Option<(usize, TokenKind)> {
    if ini_sections && chars[index] == '[' {
        let end = find_close(chars, index + 1, "]")?;
        return Some((end, TokenKind::Type));
    }
    let start = index;
    let mut end = index;
    while end < chars.len()
        && (chars[end].is_alphanumeric() || matches!(chars[end], '_' | '-' | '.'))
    {
        end += 1;
    }
    if end == start {
        return None;
    }
    let mut after = end;
    while after < chars.len() && chars[after] == ' ' {
        after += 1;
    }
    if chars.get(after) == Some(&separator) {
        return Some((end, TokenKind::Variable));
    }
    None
}

fn scan_diff(chars: &[char]) -> Vec<Token> {
    let mut out = Vec::new();
    let mut index = 0usize;
    while index < chars.len() {
        let end = line_end(chars, index);
        let line = collect(&chars[index..end]);
        let kind = if line.starts_with("+++")
            || line.starts_with("---")
            || line.starts_with("@@")
            || line.starts_with("diff ")
            || line.starts_with("index ")
            || line.starts_with("new file")
            || line.starts_with("deleted file")
            || line.starts_with("similarity index")
            || line.starts_with("rename ")
        {
            TokenKind::Meta
        } else if line.starts_with('+') {
            TokenKind::Addition
        } else if line.starts_with('-') {
            TokenKind::Deletion
        } else {
            TokenKind::Plain
        };
        push_token(&mut out, kind, &line);
        if end < chars.len() {
            push_token(&mut out, TokenKind::Plain, "\n");
            index = end + 1;
        } else {
            index = end;
        }
    }
    out
}

fn scan_markup(chars: &[char]) -> Vec<Token> {
    let mut out = Vec::new();
    let mut index = 0usize;
    while index < chars.len() {
        let current = chars[index];
        if starts_with(chars, index, "<!--") {
            let end = find_close(chars, index + 4, "-->").unwrap_or(chars.len());
            push_token(&mut out, TokenKind::Comment, &collect(&chars[index..end]));
            index = end;
            continue;
        }
        if current == '<' {
            if matches!(chars.get(index + 1), Some('!') | Some('?')) {
                let end = find_close(chars, index, ">").unwrap_or(chars.len());
                push_token(&mut out, TokenKind::Meta, &collect(&chars[index..end]));
                index = end;
                continue;
            }
            let closing = chars.get(index + 1) == Some(&'/');
            let mut cursor = if closing { index + 1 } else { index };
            push_token(
                &mut out,
                TokenKind::Punctuation,
                &collect(&chars[index..=cursor]),
            );
            cursor += 1;
            let name_start = cursor;
            while cursor < chars.len() && is_tag_char(chars[cursor]) {
                cursor += 1;
            }
            if cursor > name_start {
                push_token(
                    &mut out,
                    TokenKind::Type,
                    &collect(&chars[name_start..cursor]),
                );
            }
            while cursor < chars.len() && chars[cursor] != '>' {
                if chars[cursor].is_whitespace() {
                    let start = cursor;
                    while cursor < chars.len() && chars[cursor].is_whitespace() {
                        cursor += 1;
                    }
                    push_token(&mut out, TokenKind::Plain, &collect(&chars[start..cursor]));
                    continue;
                }
                if chars[cursor] == '/' {
                    push_token(&mut out, TokenKind::Punctuation, "/");
                    cursor += 1;
                    continue;
                }
                if chars[cursor] == '=' {
                    push_token(&mut out, TokenKind::Operator, "=");
                    cursor += 1;
                    continue;
                }
                if matches!(chars[cursor], '"' | '\'') {
                    let quote = chars[cursor];
                    let start = cursor;
                    cursor += 1;
                    while cursor < chars.len() && chars[cursor] != quote {
                        if chars[cursor] == '\\' {
                            cursor += 1;
                        }
                        cursor += 1;
                    }
                    if cursor < chars.len() {
                        cursor += 1;
                    }
                    push_token(&mut out, TokenKind::String, &collect(&chars[start..cursor]));
                    continue;
                }
                if is_tag_char(chars[cursor]) {
                    let start = cursor;
                    while cursor < chars.len() && is_tag_char(chars[cursor]) {
                        cursor += 1;
                    }
                    push_token(
                        &mut out,
                        TokenKind::Variable,
                        &collect(&chars[start..cursor]),
                    );
                    continue;
                }
                push_token(&mut out, TokenKind::Plain, &chars[cursor].to_string());
                cursor += 1;
            }
            if cursor < chars.len() {
                push_token(&mut out, TokenKind::Punctuation, ">");
                cursor += 1;
            }
            index = cursor;
            continue;
        }
        if current == '&' {
            if let Some(end) = find_close(chars, index, ";") {
                if end - index <= 12 {
                    push_token(&mut out, TokenKind::Literal, &collect(&chars[index..end]));
                    index = end;
                    continue;
                }
            }
        }
        let start = index;
        while index < chars.len()
            && chars[index] != '<'
            && chars[index] != '&'
            && chars[index] != '\n'
        {
            index += 1;
        }
        if index == start {
            push_token(&mut out, TokenKind::Plain, &chars[index].to_string());
            index += 1;
        } else {
            push_token(&mut out, TokenKind::Plain, &collect(&chars[start..index]));
        }
    }
    out
}

fn is_tag_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '-' | '_' | ':' | '.' | '@' | '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(code: &str, language: &str) -> Vec<(TokenKind, String)> {
        tokenize(code, language)
            .expect("supported language")
            .into_iter()
            .map(|token| (token.kind, token.text))
            .collect()
    }

    fn kinds(code: &str, language: &str) -> Vec<(TokenKind, String)> {
        let mut found = Vec::new();
        for (kind, text) in tokens(code, language) {
            for part in text.split('\n') {
                if !part.trim().is_empty() {
                    found.push((kind, part.to_string()));
                }
            }
        }
        found
    }

    #[test]
    fn supports_aliases_and_rejects_unknown() {
        assert!(supports_language("rust"));
        assert!(supports_language("RS"));
        assert!(supports_language("py"));
        assert!(supports_language(" yml "));
        assert!(!supports_language(""));
        assert!(!supports_language("brainfuck"));
        assert!(tokenize("x", "cobol").is_none());
    }

    #[test]
    fn rust_keywords_functions_strings_comments_numbers() {
        let source = "// doc\nfn main() { let n: u32 = 0x1f; let s = r#\"raw // x\"#; }";
        let found = kinds(source, "rust");
        assert!(found.contains(&(TokenKind::Comment, "// doc".into())));
        assert!(found.contains(&(TokenKind::Keyword, "fn".into())));
        assert!(found.contains(&(TokenKind::Function, "main".into())));
        assert!(found.contains(&(TokenKind::Keyword, "let".into())));
        assert!(found.contains(&(TokenKind::Type, "u32".into())));
        assert!(found.contains(&(TokenKind::Number, "0x1f".into())));
        assert!(found.contains(&(TokenKind::String, "r#\"raw // x\"#".into())));
    }

    #[test]
    fn rust_lifetimes_are_not_strings() {
        let found = kinds("fn f<'a>(x: &'a str) -> &'a str { x }", "rust");
        assert!(
            !found
                .iter()
                .any(|(kind, text)| *kind == TokenKind::String && text.starts_with('\'')),
            "lifetimes must stay plain: {found:?}"
        );
        assert!(found
            .iter()
            .any(|(kind, text)| *kind == TokenKind::Type && text == "str"));
    }

    #[test]
    fn rust_attributes_are_meta() {
        let found = kinds("#[derive(Debug, Clone)]\nstruct S;", "rust");
        assert!(found.contains(&(TokenKind::Meta, "#[derive(Debug, Clone)]".into())));
        assert!(found.contains(&(TokenKind::Keyword, "struct".into())));
    }

    #[test]
    fn typescript_template_and_block_comment_span_lines() {
        let source = "/* a\n   b */\nconst s = `hi\n${name}`;";
        let found = kinds(source, "typescript");
        assert!(found.contains(&(TokenKind::Comment, "/* a".into())));
        assert!(found.contains(&(TokenKind::Comment, "   b */".into())));
        assert!(found.contains(&(TokenKind::Keyword, "const".into())));
        assert!(found.contains(&(TokenKind::String, "`hi".into())));
        assert!(found.contains(&(TokenKind::String, "${name}`".into())));
    }

    #[test]
    fn python_docstring_and_decorator() {
        let source = "@app.route\ndef handler():\n    \"\"\"multi\n    line\"\"\"\n    return None";
        let found = kinds(source, "python");
        assert!(found.contains(&(TokenKind::Meta, "@app.route".into())));
        assert!(found.contains(&(TokenKind::Keyword, "def".into())));
        assert!(found.contains(&(TokenKind::Function, "handler".into())));
        assert!(found
            .iter()
            .any(|(kind, text)| *kind == TokenKind::String && text == "\"\"\"multi"));
        assert!(found
            .iter()
            .any(|(kind, text)| *kind == TokenKind::String && text.trim() == "line\"\"\""));
        assert!(found.contains(&(TokenKind::Literal, "None".into())));
    }

    #[test]
    fn shell_shebang_variables_flags() {
        let source = "#!/bin/bash\nset -euo pipefail\necho $HOME > out";
        let found = kinds(source, "bash");
        assert!(found.contains(&(TokenKind::Comment, "#!/bin/bash".into())));
        assert!(found.contains(&(TokenKind::Variable, "-euo".into())));
        assert!(found.contains(&(TokenKind::Variable, "$HOME".into())));
        assert!(found.contains(&(TokenKind::Type, "echo".into())));
    }

    #[test]
    fn sql_keywords_are_case_insensitive() {
        let found = kinds("SELECT id from users where name = 'x' limit 1", "sql");
        assert!(found.contains(&(TokenKind::Keyword, "SELECT".into())));
        assert!(found.contains(&(TokenKind::Keyword, "from".into())));
        assert!(found.contains(&(TokenKind::Keyword, "where".into())));
        assert!(found.contains(&(TokenKind::String, "'x'".into())));
    }

    #[test]
    fn json_keys_are_strings_and_literals_are_numbers() {
        let found = kinds("{\"a\": 1, \"b\": true, \"c\": null}", "json");
        assert!(found.contains(&(TokenKind::String, "\"a\"".into())));
        assert!(found.contains(&(TokenKind::Number, "1".into())));
        assert!(found.contains(&(TokenKind::Literal, "true".into())));
        assert!(found.contains(&(TokenKind::Literal, "null".into())));
    }

    #[test]
    fn yaml_keys_comments_and_literals() {
        let found = kinds("# note\nname: pi\n- id: 3\n  flag: yes", "yaml");
        assert!(found.contains(&(TokenKind::Comment, "# note".into())));
        assert!(found.contains(&(TokenKind::Variable, "name".into())));
        assert!(found.contains(&(TokenKind::Variable, "id".into())));
        assert!(found.contains(&(TokenKind::Literal, "yes".into())));
        assert!(found.contains(&(TokenKind::Number, "3".into())));
    }

    #[test]
    fn toml_sections_and_keys() {
        let found = kinds("[package]\nname = \"pi\"\nedition = \"2021\"", "toml");
        assert!(found.contains(&(TokenKind::Type, "[package]".into())));
        assert!(found.contains(&(TokenKind::Variable, "name".into())));
        assert!(found.contains(&(TokenKind::String, "\"pi\"".into())));
    }

    #[test]
    fn html_tags_attributes_and_comment() {
        let source = "<!-- hi --><div class=\"a\" data-x='1'>text &amp; more</div>";
        let found = kinds(source, "html");
        assert!(found.contains(&(TokenKind::Comment, "<!-- hi -->".into())));
        assert!(found.contains(&(TokenKind::Type, "div".into())));
        assert!(found.contains(&(TokenKind::Variable, "class".into())));
        assert!(found.contains(&(TokenKind::String, "\"a\"".into())));
        assert!(found.contains(&(TokenKind::Literal, "&amp;".into())));
        assert!(found.contains(&(TokenKind::Type, "div".into())));
    }

    #[test]
    fn css_hex_colors_are_numbers() {
        let found = kinds("/* c */ .a { color: #ff00aa; }", "css");
        assert!(found.contains(&(TokenKind::Comment, "/* c */".into())));
        assert!(found.contains(&(TokenKind::Number, "#ff00aa".into())));
    }

    #[test]
    fn diff_lines_are_additions_deletions_and_meta() {
        let source = "--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+new\n context";
        let found = kinds(source, "diff");
        assert!(found.contains(&(TokenKind::Meta, "--- a/f".into())));
        assert!(found.contains(&(TokenKind::Meta, "+++ b/f".into())));
        assert!(found.contains(&(TokenKind::Meta, "@@ -1 +1 @@".into())));
        assert!(found.contains(&(TokenKind::Deletion, "-old".into())));
        assert!(found.contains(&(TokenKind::Addition, "+new".into())));
        assert!(found.contains(&(TokenKind::Plain, " context".into())));
    }

    #[test]
    fn unknown_language_keeps_plain_lines() {
        let lines = highlight_code("a\nb\n", Some("brainfuck"), SpanStyle::PLAIN);
        assert_eq!(lines.len(), 3);
        assert_eq!(crate::styled::plain_text(&lines[0]), "a");
        assert_eq!(crate::styled::plain_text(&lines[2]), "");
        assert_eq!(lines[0][0].style.fg, None);
    }

    #[test]
    fn highlight_keeps_base_foreground_on_plain_tokens() {
        let base = SpanStyle {
            fg: Some(ThemeColor::MdCodeBlock),
            ..SpanStyle::PLAIN
        };
        let lines = highlight_code("let x = 1;", Some("rust"), base);
        assert_eq!(lines.len(), 1);
        let keyword = lines[0]
            .iter()
            .find(|span| span.text == "let")
            .expect("keyword span");
        assert_eq!(keyword.style.fg, Some(ThemeColor::SyntaxKeyword));
        let space = lines[0]
            .iter()
            .find(|span| span.text == " ")
            .expect("plain span");
        assert_eq!(space.style.fg, Some(ThemeColor::MdCodeBlock));
    }

    #[test]
    fn highlight_code_preserves_line_count() {
        for source in ["", "one", "one\n", "one\ntwo", "\n\n"] {
            let lines = highlight_code(source, Some("rust"), SpanStyle::PLAIN);
            assert_eq!(
                lines.len(),
                source.split('\n').count(),
                "line count for {source:?}"
            );
            let rendered: Vec<String> = lines
                .iter()
                .map(|line| crate::styled::plain_text(line))
                .collect();
            assert_eq!(rendered.join("\n"), source);
        }
    }

    #[test]
    fn multiline_strings_keep_their_style_across_lines() {
        let lines = highlight_code("x = \"\"\"a\nb\"\"\"", Some("python"), SpanStyle::PLAIN);
        assert_eq!(lines.len(), 2);
        assert_eq!(crate::styled::plain_text(&lines[0]), "x = \"\"\"a");
        assert_eq!(crate::styled::plain_text(&lines[1]), "b\"\"\"");
        assert_eq!(
            lines[0].last().map(|span| span.style.fg),
            Some(Some(ThemeColor::SyntaxString))
        );
        assert_eq!(
            lines[1].last().map(|span| span.style.fg),
            Some(Some(ThemeColor::SyntaxString))
        );
    }

    #[test]
    fn aliases_and_missing_language_fall_back_to_plain() {
        assert!(supports_language("javascript"));
        assert!(supports_language("tsx"));
        let lines = highlight_code("plain", None, SpanStyle::PLAIN);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0][0].text, "plain");
    }
}
