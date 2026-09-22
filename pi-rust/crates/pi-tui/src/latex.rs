//! Terminal-friendly LaTeX math rendering.
//!
//! Port of `packages/tui/src/latex.ts`: a small LaTeX-to-Unicode renderer used
//! by the markdown view for `$…$` / `$$…$$` / `\(…\)` / `\[…\]` spans. The
//! renderer covers the subset of commands Pi displays in the terminal —
//! Greek letters, relations, operators, scripts, fractions, roots, accents,
//! matrices and the `align`/`cases` environments — and returns `None` for
//! anything malformed or unsupported, so the caller can fall back to the raw
//! source.
//!
//! The port keeps upstream's algorithm, including the placeholder-marker
//! pipeline: stacked fractions/limits/matrices are registered as
//! [`LayoutNode`]s while parsing, a private-use marker is emitted in their
//! place, and [`render_layout`] expands the markers into ASCII-art boxes once
//! the surrounding text has been normalised.
//!
//! Two deliberate deviations from upstream:
//!
//! * Width and length counting follows this crate's existing convention (one
//!   `char` is one column — see `markdown.rs`), so East-Asian-width glyphs are
//!   not padded to two columns.
//! * [`render_latex`] has no way to turn rendering off; the caller decides
//!   whether to substitute the returned text for the raw span.

use std::iter;

use crate::width::columns;

/// Map a LaTeX symbol command to its Unicode glyph.
#[rustfmt::skip]
fn symbol(key: &str) -> Option<&'static str> {
    match key {
        "alpha" => Some("α"),
        "beta" => Some("β"),
        "gamma" => Some("γ"),
        "delta" => Some("δ"),
        "epsilon" => Some("ϵ"),
        "varepsilon" => Some("ε"),
        "zeta" => Some("ζ"),
        "eta" => Some("η"),
        "theta" => Some("θ"),
        "vartheta" => Some("ϑ"),
        "iota" => Some("ι"),
        "kappa" => Some("κ"),
        "varkappa" => Some("ϰ"),
        "lambda" => Some("λ"),
        "mu" => Some("μ"),
        "nu" => Some("ν"),
        "xi" => Some("ξ"),
        "pi" => Some("π"),
        "varpi" => Some("ϖ"),
        "rho" => Some("ρ"),
        "varrho" => Some("ϱ"),
        "sigma" => Some("σ"),
        "varsigma" => Some("ς"),
        "tau" => Some("τ"),
        "upsilon" => Some("υ"),
        "phi" => Some("ϕ"),
        "varphi" => Some("φ"),
        "chi" => Some("χ"),
        "psi" => Some("ψ"),
        "omega" => Some("ω"),
        "Gamma" => Some("Γ"),
        "Delta" => Some("Δ"),
        "Theta" => Some("Θ"),
        "Lambda" => Some("Λ"),
        "Xi" => Some("Ξ"),
        "Pi" => Some("Π"),
        "Sigma" => Some("Σ"),
        "Upsilon" => Some("Υ"),
        "Phi" => Some("Φ"),
        "Psi" => Some("Ψ"),
        "Omega" => Some("Ω"),
        "pm" => Some("±"),
        "mp" => Some("∓"),
        "times" => Some("×"),
        "div" => Some("÷"),
        "cdot" => Some("·"),
        "ast" => Some("∗"),
        "star" => Some("⋆"),
        "circ" => Some("∘"),
        "bullet" => Some("•"),
        "oplus" => Some("⊕"),
        "ominus" => Some("⊖"),
        "otimes" => Some("⊗"),
        "oslash" => Some("⊘"),
        "odot" => Some("⊙"),
        "bigcirc" => Some("○"),
        "dagger" => Some("†"),
        "ddagger" => Some("‡"),
        "amalg" => Some("⨿"),
        "uplus" => Some("⊎"),
        "sqcap" => Some("⊓"),
        "sqcup" => Some("⊔"),
        "bowtie" => Some("⋈"),
        "Join" => Some("⋈"),
        "ltimes" => Some("⋉"),
        "rtimes" => Some("⋊"),
        "leftouterjoin" => Some("⟕"),
        "rightouterjoin" => Some("⟖"),
        "fullouterjoin" => Some("⟗"),
        "triangleleft" => Some("◁"),
        "triangleright" => Some("▷"),
        "wr" => Some("≀"),
        "cap" => Some("∩"),
        "cup" => Some("∪"),
        "bigcap" => Some("⋂"),
        "bigcup" => Some("⋃"),
        "bigwedge" => Some("⋀"),
        "bigvee" => Some("⋁"),
        "bigsqcup" => Some("⨆"),
        "biguplus" => Some("⨄"),
        "bigoplus" => Some("⨁"),
        "bigotimes" => Some("⨂"),
        "bigodot" => Some("⨀"),
        "setminus" => Some("∖"),
        "in" => Some("∈"),
        "notin" => Some("∉"),
        "ni" => Some("∋"),
        "subset" => Some("⊂"),
        "supset" => Some("⊃"),
        "subseteq" => Some("⊆"),
        "supseteq" => Some("⊇"),
        "sqsubset" => Some("⊏"),
        "sqsupset" => Some("⊐"),
        "sqsubseteq" => Some("⊑"),
        "sqsupseteq" => Some("⊒"),
        "prec" => Some("≺"),
        "preceq" => Some("≼"),
        "succ" => Some("≻"),
        "succeq" => Some("≽"),
        "ll" => Some("≪"),
        "gg" => Some("≫"),
        "le" => Some("≤"),
        "leq" => Some("≤"),
        "leqslant" => Some("≤"),
        "ge" => Some("≥"),
        "geq" => Some("≥"),
        "geqslant" => Some("≥"),
        "ne" => Some("≠"),
        "neq" => Some("≠"),
        "equiv" => Some("≡"),
        "approx" => Some("≈"),
        "sim" => Some("∼"),
        "simeq" => Some("≃"),
        "cong" => Some("≅"),
        "asymp" => Some("≍"),
        "doteq" => Some("≐"),
        "propto" => Some("∝"),
        "parallel" => Some("∥"),
        "perp" => Some("⊥"),
        "mid" => Some("∣"),
        "vdash" => Some("⊢"),
        "dashv" => Some("⊣"),
        "models" => Some("⊨"),
        "Vdash" => Some("⊩"),
        "Vvdash" => Some("⊪"),
        "nvdash" => Some("⊬"),
        "nvDash" => Some("⊭"),
        "forall" => Some("∀"),
        "exists" => Some("∃"),
        "nexists" => Some("∄"),
        "neg" => Some("¬"),
        "land" => Some("∧"),
        "wedge" => Some("∧"),
        "lor" => Some("∨"),
        "vee" => Some("∨"),
        "to" => Some("→"),
        "rightarrow" => Some("→"),
        "longrightarrow" => Some("→"),
        "leftarrow" => Some("←"),
        "longleftarrow" => Some("←"),
        "gets" => Some("←"),
        "leftrightarrow" => Some("↔"),
        "longleftrightarrow" => Some("↔"),
        "hookleftarrow" => Some("↩"),
        "hookrightarrow" => Some("↪"),
        "twoheadleftarrow" => Some("↞"),
        "twoheadrightarrow" => Some("↠"),
        "leftharpoonup" => Some("↼"),
        "leftharpoondown" => Some("↽"),
        "rightharpoonup" => Some("⇀"),
        "rightharpoondown" => Some("⇁"),
        "rightleftharpoons" => Some("⇌"),
        "leftrightharpoons" => Some("⇋"),
        "nearrow" => Some("↗"),
        "searrow" => Some("↘"),
        "swarrow" => Some("↙"),
        "nwarrow" => Some("↖"),
        "rightsquigarrow" => Some("⇝"),
        "leadsto" => Some("⇝"),
        "Rightarrow" => Some("⇒"),
        "Longrightarrow" => Some("⇒"),
        "Leftarrow" => Some("⇐"),
        "Longleftarrow" => Some("⇐"),
        "Leftrightarrow" => Some("⇔"),
        "Longleftrightarrow" => Some("⇔"),
        "implies" => Some("⇒"),
        "iff" => Some("⇔"),
        "mapsto" => Some("↦"),
        "longmapsto" => Some("↦"),
        "uparrow" => Some("↑"),
        "downarrow" => Some("↓"),
        "partial" => Some("∂"),
        "nabla" => Some("∇"),
        "int" => Some("∫"),
        "iint" => Some("∬"),
        "iiint" => Some("∭"),
        "oint" => Some("∮"),
        "sum" => Some("∑"),
        "prod" => Some("∏"),
        "coprod" => Some("∐"),
        "infty" => Some("∞"),
        "emptyset" => Some("∅"),
        "varnothing" => Some("∅"),
        "angle" => Some("∠"),
        "therefore" => Some("∴"),
        "because" => Some("∵"),
        "aleph" => Some("ℵ"),
        "beth" => Some("ℶ"),
        "gimel" => Some("ℷ"),
        "daleth" => Some("ℸ"),
        "top" => Some("⊤"),
        "bot" => Some("⊥"),
        "triangle" => Some("△"),
        "square" => Some("□"),
        "lozenge" => Some("◊"),
        "checkmark" => Some("✓"),
        "complement" => Some("∁"),
        "wp" => Some("℘"),
        "prime" => Some("′"),
        "ldots" => Some("…"),
        "dots" => Some("…"),
        "cdots" => Some("⋯"),
        "vdots" => Some("⋮"),
        "ddots" => Some("⋱"),
        "ell" => Some("ℓ"),
        "hbar" => Some("ℏ"),
        "Im" => Some("ℑ"),
        "Re" => Some("ℜ"),
        "langle" => Some("⟨"),
        "rangle" => Some("⟩"),
        "vert" => Some("|"),
        "lvert" => Some("|"),
        "rvert" => Some("|"),
        "Vert" => Some("‖"),
        "lVert" => Some("‖"),
        "rVert" => Some("‖"),
        "lbrace" => Some("{"),
        "rbrace" => Some("}"),
        "backslash" => Some("\\"),
        "lfloor" => Some("⌊"),
        "rfloor" => Some("⌋"),
        "lceil" => Some("⌈"),
        "rceil" => Some("⌉"),
        "colon" => Some(":"),
        _ => None,
    }
}

/// Map a symbol to its `\not` form.
#[rustfmt::skip]
fn negated_symbol(key: &str) -> Option<&'static str> {
    match key {
        "<" => Some("≮"),
        ">" => Some("≯"),
        "=" => Some("≠"),
        "∈" => Some("∉"),
        "∋" => Some("∌"),
        "∣" => Some("∤"),
        "∥" => Some("∦"),
        "∼" => Some("≁"),
        "≃" => Some("≄"),
        "≅" => Some("≇"),
        "≈" => Some("≉"),
        "≡" => Some("≢"),
        "≤" => Some("≰"),
        "≥" => Some("≱"),
        "≺" => Some("⊀"),
        "≻" => Some("⊁"),
        "⊂" => Some("⊄"),
        "⊃" => Some("⊅"),
        "⊆" => Some("⊈"),
        "⊇" => Some("⊉"),
        "⊢" => Some("⊬"),
        "⊨" => Some("⊭"),
        "↔" => Some("↮"),
        "←" => Some("↚"),
        "→" => Some("↛"),
        "⇒" => Some("⇏"),
        "⇐" => Some("⇍"),
        "⇔" => Some("⇎"),
        "≼" => Some("⋠"),
        "≽" => Some("⋡"),
        _ => None,
    }
}

/// Map a character to its blackboard-bold form.
#[rustfmt::skip]
fn blackboard(key: char) -> Option<char> {
    match key {
        'C' => Some('ℂ'),
        'H' => Some('ℍ'),
        'N' => Some('ℕ'),
        'P' => Some('ℙ'),
        'Q' => Some('ℚ'),
        'R' => Some('ℝ'),
        'Z' => Some('ℤ'),
        _ => None,
    }
}

/// Map a character to its Unicode superscript.
#[rustfmt::skip]
fn superscript(key: char) -> Option<char> {
    match key {
        '0' => Some('⁰'),
        '1' => Some('¹'),
        '2' => Some('²'),
        '3' => Some('³'),
        '4' => Some('⁴'),
        '5' => Some('⁵'),
        '6' => Some('⁶'),
        '7' => Some('⁷'),
        '8' => Some('⁸'),
        '9' => Some('⁹'),
        '+' => Some('⁺'),
        '-' => Some('⁻'),
        '=' => Some('⁼'),
        '(' => Some('⁽'),
        ')' => Some('⁾'),
        'a' => Some('ᵃ'),
        'b' => Some('ᵇ'),
        'c' => Some('ᶜ'),
        'd' => Some('ᵈ'),
        'e' => Some('ᵉ'),
        'f' => Some('ᶠ'),
        'g' => Some('ᵍ'),
        'h' => Some('ʰ'),
        'i' => Some('ⁱ'),
        'j' => Some('ʲ'),
        'k' => Some('ᵏ'),
        'l' => Some('ˡ'),
        'm' => Some('ᵐ'),
        'n' => Some('ⁿ'),
        'o' => Some('ᵒ'),
        'p' => Some('ᵖ'),
        'r' => Some('ʳ'),
        's' => Some('ˢ'),
        't' => Some('ᵗ'),
        'u' => Some('ᵘ'),
        'v' => Some('ᵛ'),
        'w' => Some('ʷ'),
        'x' => Some('ˣ'),
        'y' => Some('ʸ'),
        'z' => Some('ᶻ'),
        _ => None,
    }
}

/// Map a character to its Unicode subscript.
#[rustfmt::skip]
fn subscript(key: char) -> Option<char> {
    match key {
        '0' => Some('₀'),
        '1' => Some('₁'),
        '2' => Some('₂'),
        '3' => Some('₃'),
        '4' => Some('₄'),
        '5' => Some('₅'),
        '6' => Some('₆'),
        '7' => Some('₇'),
        '8' => Some('₈'),
        '9' => Some('₉'),
        '+' => Some('₊'),
        '-' => Some('₋'),
        '=' => Some('₌'),
        '(' => Some('₍'),
        ')' => Some('₎'),
        'a' => Some('ₐ'),
        'e' => Some('ₑ'),
        'h' => Some('ₕ'),
        'i' => Some('ᵢ'),
        'j' => Some('ⱼ'),
        'k' => Some('ₖ'),
        'l' => Some('ₗ'),
        'm' => Some('ₘ'),
        'n' => Some('ₙ'),
        'o' => Some('ₒ'),
        'p' => Some('ₚ'),
        'r' => Some('ᵣ'),
        's' => Some('ₛ'),
        't' => Some('ₜ'),
        'u' => Some('ᵤ'),
        'v' => Some('ᵥ'),
        'x' => Some('ₓ'),
        _ => None,
    }
}

/// Map an accent command to its combining character.
#[rustfmt::skip]
fn accent(key: &str) -> Option<&'static str> {
    match key {
        "acute" => Some("́"),
        "bar" => Some("̅"),
        "breve" => Some("̆"),
        "check" => Some("̌"),
        "ddot" => Some("̈"),
        "dot" => Some("̇"),
        "grave" => Some("̀"),
        "hat" => Some("̂"),
        "mathring" => Some("̊"),
        "overleftarrow" => Some("⃖"),
        "overleftrightarrow" => Some("⃡"),
        "overline" => Some("̅"),
        "overrightarrow" => Some("⃗"),
        "tilde" => Some("̃"),
        "underline" => Some("̲"),
        "vec" => Some("⃗"),
        "widehat" => Some("̂"),
        "widetilde" => Some("̃"),
        _ => None,
    }
}

/// Named operators (`\sin`, `\lim`, …) rendered upright.
#[rustfmt::skip]
fn is_named_operator(key: &str) -> bool {
    matches!(key,
        "Pr" | "arccos" | "arcsin" | "arctan" | "arg" | "cos" | "cosh" | "cot" | "coth" | "csc" | "deg" | "det" |
        "dim" | "exp" | "gcd" | "hom" | "inf" | "ker" | "lg" | "lim" | "liminf" | "limsup" | "ln" | "log" |
        "max" | "min" | "sec" | "sin" | "sinh" | "sup" | "tan" | "tanh"
    )
}

/// Operators that accept limits above/below.
#[rustfmt::skip]
fn is_limit_operator(key: &str) -> bool {
    matches!(key,
        "argmax" | "argmin" | "inf" | "injlim" | "lim" | "liminf" | "limsup" | "max" | "min" | "projlim" | "sup"
    )
}

/// Large operators that accept display limits.
#[rustfmt::skip]
fn is_display_limit_symbol(key: &str) -> bool {
    matches!(key,
        "bigcap" | "bigcup" | "bigodot" | "bigoplus" | "bigotimes" | "bigsqcup" | "biguplus" | "bigvee" |
        "bigwedge" | "coprod" | "iiint" | "iint" | "int" | "oint" | "prod" | "sum"
    )
}

/// Relations rendered with surrounding spaces.
#[rustfmt::skip]
fn is_relation_command(key: &str) -> bool {
    matches!(key,
        "Join" | "Leftarrow" | "Leftrightarrow" | "Longleftarrow" | "Longleftrightarrow" | "Longrightarrow" |
        "Rightarrow" | "Vdash" | "Vvdash" | "approx" | "asymp" | "bowtie" | "cong" | "dashv" | "doteq" |
        "downarrow" | "equiv" | "fullouterjoin" | "ge" | "geq" | "geqslant" | "gets" | "gg" | "hookleftarrow" |
        "hookrightarrow" | "iff" | "implies" | "in" | "le" | "leadsto" | "leftarrow" | "leftharpoondown" |
        "leftharpoonup" | "leftouterjoin" | "leftrightarrow" | "leftrightharpoons" | "leq" | "leqslant" | "ll" |
        "longleftarrow" | "longleftrightarrow" | "longmapsto" | "longrightarrow" | "ltimes" | "mapsto" | "mid" |
        "models" | "ne" | "nearrow" | "neq" | "ni" | "notin" | "nvDash" | "nvdash" | "nwarrow" | "parallel" |
        "perp" | "prec" | "preceq" | "propto" | "rightarrow" | "rightharpoondown" | "rightharpoonup" |
        "rightleftharpoons" | "rightouterjoin" | "rightsquigarrow" | "rtimes" | "searrow" | "sim" | "simeq" |
        "sqsubset" | "sqsubseteq" | "sqsupset" | "sqsupseteq" | "subset" | "subseteq" | "succ" | "succeq" |
        "supset" | "supseteq" | "swarrow" | "to" | "triangleleft" | "triangleright" | "twoheadleftarrow" |
        "twoheadrightarrow" | "uparrow" | "vdash"
    )
}

/// Spacing commands rendered as a single space.
#[rustfmt::skip]
fn is_spacing_command(key: &str) -> bool {
    matches!(key,
        " " | "," | ":" | ";" | ">" | "enskip" | "enspace" | "medspace" | "qquad" | "quad" | "thickspace" |
        "thinspace"
    )
}

/// Commands that remove the preceding space.
#[rustfmt::skip]
fn is_negative_spacing_command(key: &str) -> bool {
    matches!(key,
        "!" | "negmedspace" | "negthickspace" | "negthinspace"
    )
}

/// Style commands that render as nothing.
#[rustfmt::skip]
fn is_ignored_command(key: &str) -> bool {
    matches!(key,
        "displaystyle" | "limits" | "nolimits" | "scriptscriptstyle" | "scriptstyle" | "textstyle"
    )
}

/// Manually-sized delimiters, rendered as nothing.
#[rustfmt::skip]
fn is_size_command(key: &str) -> bool {
    matches!(key,
        "Big" | "Bigg" | "Biggl" | "Biggr" | "Bigl" | "Bigr" | "big" | "bigg" | "biggl" | "biggr" | "bigl" |
        "bigr"
    )
}

/// Commands that render their argument verbatim.
#[rustfmt::skip]
fn is_plain_wrapper(key: &str) -> bool {
    matches!(key,
        "bm" | "boldsymbol" | "emph" | "mathbf" | "mathcal" | "mathfrak" | "mathit" | "mathnormal" | "mathrm" |
        "mathscr" | "mathsf" | "mathtt" | "mathup" | "mbox" | "overbrace" | "pmb" | "smash" | "substack" |
        "text" | "textbf" | "textit" | "textmd" | "textnormal" | "textrm" | "textsc" | "textsf" | "textsl" |
        "texttt" | "textup" | "underbrace"
    )
}

// ---------------------------------------------------------------------------
// Small text helpers
// ---------------------------------------------------------------------------

/// Width of `text` in terminal columns ([`crate::width`]).
fn visible_width(text: &str) -> usize {
    columns(text)
}

/// Replace every character of `value`, or return `None` if any is missing.
fn replace_characters(value: &str, replacements: fn(char) -> Option<char>) -> Option<String> {
    let mut result = String::new();
    for character in value.chars() {
        result.push(replacements(character)?);
    }
    Some(result)
}

/// Remove whitespace around `=`, `+` and `-` (`/\s*([=+-])\s*/g`).
fn strip_spaces_around_signs(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::new();
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        if character.is_whitespace() {
            let start = index;
            while index < chars.len() && chars[index].is_whitespace() {
                index += 1;
            }
            if chars
                .get(index)
                .is_some_and(|c| matches!(c, '=' | '+' | '-'))
            {
                continue;
            }
            out.extend(&chars[start..index]);
            continue;
        }
        if matches!(character, '=' | '+' | '-') {
            out.push(character);
            index += 1;
            while index < chars.len() && chars[index].is_whitespace() {
                index += 1;
            }
            continue;
        }
        out.push(character);
        index += 1;
    }
    out
}

/// Render a sub/superscript value with Unicode glyphs when possible.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Script {
    Sub,
    Sup,
}

fn format_script(value: &str, kind: Script) -> String {
    let value = value.trim();
    let replacements: fn(char) -> Option<char> = match kind {
        Script::Sub => subscript,
        Script::Sup => superscript,
    };
    if let Some(unicode) = replace_characters(&strip_spaces_around_signs(value), replacements) {
        return unicode;
    }

    let prefix = if kind == Script::Sub { "_" } else { "^" };
    let single = value.chars().count() == 1;
    let all_letters =
        kind == Script::Sub && !value.is_empty() && value.chars().all(|c| c.is_ascii_alphabetic());
    if single || all_letters {
        return format!("{prefix}{value}");
    }
    format!("{prefix}({value})")
}

/// Inline fraction, e.g. `1/2` or `(x+1)/2`.
fn format_fraction(numerator: &str, denominator: &str) -> String {
    let numerator = numerator.trim();
    let denominator = denominator.trim();
    let simple_numerator =
        !numerator.is_empty() && numerator.chars().all(|c| c.is_alphanumeric() || c == '.');
    let simple_denominator = (!denominator.is_empty()
        && denominator.chars().all(|c| c.is_numeric() || c == '.'))
        || denominator.chars().count() == 1;
    let numerator = if simple_numerator {
        numerator.to_string()
    } else {
        format!("({numerator})")
    };
    let denominator = if simple_denominator {
        denominator.to_string()
    } else {
        format!("({denominator})")
    };
    format!("{numerator}/{denominator}")
}

/// `√x`, `∛x`, or `√(x+1)` for compound roots.
fn format_root(value: &str, symbol: &str) -> String {
    let value = value.trim();
    if !value.is_empty() && value.chars().all(|c| c.is_alphanumeric() || c == '.') {
        format!("{symbol}{value}")
    } else {
        format!("{symbol}({value})")
    }
}

// ---------------------------------------------------------------------------
// Layout markers
// ---------------------------------------------------------------------------

const NAMED_OPERATOR_START: char = '\u{f0004}';
const NAMED_OPERATOR_END: char = '\u{f0005}';
const LAYOUT_MARKER_START: char = '\u{f0000}';
const LAYOUT_MARKER_END: char = '\u{f0001}';
const PROTECTED_SPACE: char = '\u{f0002}';
const NEGATIVE_SPACE: &str = "\u{0}";

/// Collapse runs of spaces and tabs, then trim (`/[ \t]+/g` + `trim`).
fn collapse_spaces(line: &str) -> String {
    let mut out = String::new();
    let mut in_run = false;
    for character in line.chars() {
        if character == ' ' || character == '\t' {
            if !in_run {
                out.push(' ');
                in_run = true;
            }
        } else {
            out.push(character);
            in_run = false;
        }
    }
    out.trim().to_string()
}

/// Drop named-operator markers, inserting the spaces they stood for.
fn normalize_output(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut expanded = String::new();
    for (index, character) in chars.iter().enumerate() {
        if *character == NAMED_OPERATOR_START {
            if let Some(previous) = index.checked_sub(1).and_then(|i| chars.get(i)) {
                if previous.is_alphanumeric()
                    || matches!(*previous, ')' | ']' | '}' | LAYOUT_MARKER_END)
                {
                    expanded.push(' ');
                }
            }
        } else if *character == NAMED_OPERATOR_END {
            if let Some(next) = chars.get(index + 1) {
                if next.is_alphanumeric() || matches!(*next, '√' | LAYOUT_MARKER_START) {
                    expanded.push(' ');
                }
            }
        } else {
            expanded.push(*character);
        }
    }

    let lines: Vec<String> = expanded.split('\n').map(collapse_spaces).collect();
    let count = lines.len();
    let kept: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(index, line)| !line.is_empty() || (*index > 0 && *index < count - 1))
        .map(|(_, line)| line.as_str())
        .collect();
    kept.join("\n").trim().to_string()
}

// ---------------------------------------------------------------------------
// Vertical layout (stacked fractions, limits, matrices)
// ---------------------------------------------------------------------------

/// A node that occupies more than one terminal line.
#[derive(Clone, Debug, PartialEq, Eq)]
enum LayoutNode {
    Fraction {
        numerator: String,
        denominator: String,
    },
    Operator {
        operator: String,
        lower: Option<String>,
        upper: Option<String>,
    },
    Matrix {
        lines: Vec<String>,
        baseline: usize,
    },
}

/// A rendered multi-line box: the line that it centres on plus its width.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Layout {
    lines: Vec<String>,
    width: usize,
    baseline: usize,
}

/// Pad `line` to `width`, optionally centring it.
fn pad_layout_line(line: &str, width: usize, centered: bool) -> String {
    let padding = width.saturating_sub(visible_width(line));
    let left = if centered { padding / 2 } else { 0 };
    format!("{}{}{}", " ".repeat(left), line, " ".repeat(padding - left))
}

/// Place layouts side by side, aligning them on their baselines.
fn join_layouts(layouts: &[Layout]) -> Layout {
    if layouts.is_empty() {
        return Layout {
            lines: vec![String::new()],
            width: 0,
            baseline: 0,
        };
    }
    let baseline = layouts
        .iter()
        .map(|layout| layout.baseline)
        .max()
        .unwrap_or(0);
    let below = layouts
        .iter()
        .map(|layout| {
            layout
                .lines
                .len()
                .saturating_sub(layout.baseline)
                .saturating_sub(1)
        })
        .max()
        .unwrap_or(0);

    let mut lines = Vec::new();
    for row in 0..=baseline + below {
        let mut line = String::new();
        for layout in layouts {
            let source_row = row as isize - baseline as isize + layout.baseline as isize;
            if source_row >= 0 && (source_row as usize) < layout.lines.len() {
                line.push_str(&pad_layout_line(
                    &layout.lines[source_row as usize],
                    layout.width,
                    false,
                ));
            } else {
                line.push_str(&" ".repeat(layout.width));
            }
        }
        lines.push(line.trim_end().to_string());
    }
    Layout {
        lines,
        width: layouts.iter().map(|layout| layout.width).sum(),
        baseline,
    }
}

/// Expand the layout markers left in `source` into their multi-line boxes.
fn render_layout(source: &str, nodes: &[LayoutNode]) -> Layout {
    let mut rendered_lines: Vec<String> = Vec::new();
    let mut first_baseline = 0usize;

    for source_line in source.split('\n') {
        let chars: Vec<char> = source_line.chars().collect();
        let mut layouts: Vec<Layout> = Vec::new();
        let mut position = 0usize;
        let mut previous_node: Option<&LayoutNode> = None;

        let mut index = 0usize;
        while index < chars.len() {
            if chars[index] != LAYOUT_MARKER_START {
                index += 1;
                continue;
            }
            let mut cursor = index + 1;
            let mut digits = String::new();
            while cursor < chars.len() && chars[cursor].is_ascii_digit() {
                digits.push(chars[cursor]);
                cursor += 1;
            }
            if digits.is_empty() || chars.get(cursor) != Some(&LAYOUT_MARKER_END) {
                index += 1;
                continue;
            }
            let node = digits.parse::<usize>().ok().and_then(|n| nodes.get(n));
            let Some(node) = node else {
                index = cursor + 1;
                continue;
            };

            if index > position {
                let sliced: String = chars[position..index].iter().collect();
                let trimmed = if previous_node.is_some() {
                    sliced.trim_start()
                } else {
                    sliced.as_str()
                }
                .trim_end();
                let preserve_leading = matches!(previous_node, Some(LayoutNode::Matrix { .. }))
                    && sliced.starts_with(char::is_whitespace);
                let preserve_trailing = matches!(node, LayoutNode::Matrix { .. })
                    && sliced.ends_with(char::is_whitespace);
                let text = if !trimmed.is_empty() {
                    format!(
                        "{}{}{}",
                        if preserve_leading { " " } else { "" },
                        trimmed,
                        if preserve_trailing { " " } else { "" }
                    )
                } else if preserve_leading || preserve_trailing {
                    " ".to_string()
                } else {
                    String::new()
                };
                layouts.push(Layout {
                    width: visible_width(&text),
                    lines: vec![text],
                    baseline: 0,
                });
            }

            match node {
                LayoutNode::Fraction {
                    numerator,
                    denominator,
                } => {
                    let numerator = render_layout(numerator, nodes);
                    let denominator = render_layout(denominator, nodes);
                    let content_width = numerator.width.max(denominator.width).max(1);
                    let width = content_width + 2;
                    let mut lines: Vec<String> = numerator
                        .lines
                        .iter()
                        .map(|line| pad_layout_line(line, width, true))
                        .collect();
                    lines.push(format!(" {} ", "─".repeat(content_width)));
                    lines.extend(
                        denominator
                            .lines
                            .iter()
                            .map(|line| pad_layout_line(line, width, true)),
                    );
                    layouts.push(Layout {
                        lines,
                        width,
                        baseline: numerator.lines.len(),
                    });
                }
                LayoutNode::Operator {
                    operator,
                    lower,
                    upper,
                } => {
                    let content_width = visible_width(operator)
                        .max(lower.as_deref().map_or(0, visible_width))
                        .max(upper.as_deref().map_or(0, visible_width));
                    let mut lines = Vec::new();
                    if let Some(upper) = upper {
                        lines.push(format!("{} ", pad_layout_line(upper, content_width, true)));
                    }
                    lines.push(format!(
                        "{} ",
                        pad_layout_line(operator, content_width, true)
                    ));
                    if let Some(lower) = lower {
                        lines.push(format!("{} ", pad_layout_line(lower, content_width, true)));
                    }
                    layouts.push(Layout {
                        lines,
                        width: content_width + 1,
                        baseline: if upper.is_none() { 0 } else { 1 },
                    });
                }
                LayoutNode::Matrix { lines, baseline } => {
                    let width = lines
                        .iter()
                        .map(|line| visible_width(line))
                        .max()
                        .unwrap_or(0);
                    layouts.push(Layout {
                        lines: lines
                            .iter()
                            .map(|line| pad_layout_line(line, width, false))
                            .collect(),
                        width,
                        baseline: *baseline,
                    });
                }
            }

            position = cursor + 1;
            previous_node = Some(node);
            index = cursor + 1;
        }

        if position < chars.len() {
            let sliced: String = chars[position..].iter().collect();
            let trimmed = if previous_node.is_some() {
                sliced.trim_start()
            } else {
                sliced.as_str()
            };
            let text = if matches!(previous_node, Some(LayoutNode::Matrix { .. }))
                && sliced.starts_with(char::is_whitespace)
            {
                format!(" {trimmed}")
            } else {
                trimmed.to_string()
            };
            layouts.push(Layout {
                width: visible_width(&text),
                lines: vec![text],
                baseline: 0,
            });
        }

        let line_layout = join_layouts(&layouts);
        if rendered_lines.is_empty() {
            first_baseline = line_layout.baseline;
        }
        rendered_lines.extend(line_layout.lines);
    }

    Layout {
        width: rendered_lines
            .iter()
            .map(|line| visible_width(line))
            .max()
            .unwrap_or(0),
        lines: rendered_lines,
        baseline: first_baseline,
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// How a lower script is rendered when it is not stacked.
#[derive(Clone, Copy)]
enum LowerStyle {
    Bracket,
    Script,
}

/// Recursive LaTeX parser; see the module docs.
struct LatexParser<'a> {
    source: Vec<char>,
    layout_nodes: &'a mut Vec<LayoutNode>,
    display: bool,
    position: usize,
    supported: bool,
    stack_fractions: bool,
}

impl<'a> LatexParser<'a> {
    fn new(source: &str, layout_nodes: &'a mut Vec<LayoutNode>, display: bool) -> Self {
        Self {
            source: source.chars().collect(),
            layout_nodes,
            display,
            position: 0,
            supported: true,
            stack_fractions: true,
        }
    }

    /// Render the whole source, or `None` when it is unsupported/malformed.
    fn render(&mut self) -> Option<String> {
        let rendered = self.parse_sequence(None);
        if !self.supported || self.position != self.source.len() {
            return None;
        }
        Some(normalize_output(&rendered))
    }

    fn parse_sequence(&mut self, end_character: Option<char>) -> String {
        let mut result = String::new();
        while self.position < self.source.len() {
            let character = self.source[self.position];
            if end_character == Some(character) {
                self.position += 1;
                return result;
            }

            if character == '}' {
                self.supported = false;
                return result;
            }

            if character == '{' {
                self.position += 1;
                result.push_str(&self.parse_sequence(Some('}')));
                continue;
            }

            if character == '\\' {
                let command = self.parse_command();
                if command == NEGATIVE_SPACE {
                    result = result.trim_end().to_string();
                    if result.ends_with(NAMED_OPERATOR_END) {
                        result.truncate(result.len() - NAMED_OPERATOR_END.len_utf8());
                    }
                } else {
                    result.push_str(&command);
                }
                continue;
            }

            if character == '^' || character == '_' {
                self.position += 1;
                result = result.trim_end().to_string();
                let kind = if character == '_' {
                    Script::Sub
                } else {
                    Script::Sup
                };
                let script = format_script(&self.parse_required_argument(false), kind);
                if result.ends_with(NAMED_OPERATOR_END) {
                    result.truncate(result.len() - NAMED_OPERATOR_END.len_utf8());
                    result.push_str(&script);
                    result.push(NAMED_OPERATOR_END);
                } else {
                    result.push_str(&script);
                }
                continue;
            }

            if character.is_whitespace() {
                result.push_str(&self.parse_whitespace());
                continue;
            }

            if matches!(character, '=' | '<' | '>') {
                result = format!("{} {} ", result.trim_end(), character);
                self.position += 1;
                continue;
            }

            if character == '&' {
                self.position += 1;
                continue;
            }

            if character == '~' {
                self.position += 1;
                result.push(' ');
                continue;
            }

            if character == '.' {
                if let Some(index) = trailing_layout_marker_index(&result) {
                    if let Some(LayoutNode::Matrix { lines, .. }) = self.layout_nodes.get_mut(index)
                    {
                        match lines.last_mut() {
                            Some(last) => last.push(character),
                            None => lines.push(character.to_string()),
                        }
                        self.position += 1;
                        continue;
                    }
                }
            }

            result.push(character);
            self.position += 1;
        }

        if end_character.is_some() {
            self.supported = false;
        }
        result
    }

    fn parse_whitespace(&mut self) -> String {
        while self.position < self.source.len() && self.source[self.position].is_whitespace() {
            self.position += 1;
        }
        " ".to_string()
    }

    fn parse_command(&mut self) -> String {
        self.position += 1;
        if self.position >= self.source.len() {
            self.supported = false;
            return String::new();
        }

        let first = self.source[self.position];
        if first == '\n' || first == '\r' {
            self.position += 1;
            if first == '\r' && self.source.get(self.position) == Some(&'\n') {
                self.position += 1;
            }
            return " ".to_string();
        }

        let command: String;
        if first.is_ascii_alphabetic() {
            let start = self.position;
            while self.position < self.source.len()
                && self.source[self.position].is_ascii_alphabetic()
            {
                self.position += 1;
            }
            command = self.source[start..self.position].iter().collect();
        } else {
            command = first.to_string();
            self.position += 1;
        }

        if command == "\\" {
            return "\n".to_string();
        }
        if is_spacing_command(&command) {
            return " ".to_string();
        }
        if is_negative_spacing_command(&command) {
            return NEGATIVE_SPACE.to_string();
        }
        if is_ignored_command(&command) {
            return String::new();
        }
        if matches!(command.as_str(), "{" | "}" | "$" | "%" | "#" | "_" | "&") {
            return command;
        }
        if command == "|" {
            return "‖".to_string();
        }
        if command == "not" {
            let value = self.parse_required_argument(false).trim().to_string();
            if let Some(negated) = negated_symbol(&value) {
                return format!(" {negated} ");
            }
            let characters: Vec<char> = value.chars().collect();
            if characters.is_empty() {
                self.supported = false;
                return String::new();
            }
            let rest: String = characters[1..].iter().collect();
            return format!(" {}\u{338}{rest} ", characters[0]);
        }
        if is_limit_operator(&command) {
            return self.parse_operator(&command, LowerStyle::Bracket, true, true);
        }

        if let Some(symbol) = symbol(&command) {
            if is_display_limit_symbol(&command) {
                return self.parse_operator(symbol, LowerStyle::Script, true, false);
            }
            return if command == "cdot" || command == "times" || is_relation_command(&command) {
                format!(" {symbol} ")
            } else {
                symbol.to_string()
            };
        }
        if is_named_operator(&command) {
            return format!("{NAMED_OPERATOR_START}{command}{NAMED_OPERATOR_END}");
        }
        if is_size_command(&command) {
            return String::new();
        }
        if matches!(command.as_str(), "left" | "middle" | "right") {
            if self.source.get(self.position) == Some(&'.') {
                self.position += 1;
            }
            return String::new();
        }
        if matches!(command.as_str(), "frac" | "dfrac" | "tfrac") {
            let should_stack = self.display && self.stack_fractions && command != "tfrac";
            let numerator = self.parse_required_argument(!should_stack);
            let denominator = self.parse_required_argument(!should_stack);
            if should_stack {
                let index = self.layout_nodes.len();
                self.layout_nodes.push(LayoutNode::Fraction {
                    numerator: normalize_output(&numerator),
                    denominator: normalize_output(&denominator),
                });
                return format!("{LAYOUT_MARKER_START}{index}{LAYOUT_MARKER_END}");
            }
            return format_fraction(&numerator, &denominator);
        }
        if command == "sqrt" {
            let degree = self
                .parse_optional_argument()
                .map(|degree| degree.trim().to_string());
            let value = self.parse_required_argument(true);
            return match degree.as_deref() {
                None | Some("2") => format_root(&value, "√"),
                Some("3") => format_root(&value, "∛"),
                Some("4") => format_root(&value, "∜"),
                Some(degree) => format!(
                    "{}{}",
                    format_script(degree, Script::Sup),
                    format_root(&value, "√")
                ),
            };
        }
        if command == "boxed" || command == "fbox" {
            return format!("[{}]", self.parse_required_argument(true).trim());
        }
        if command == "binom" || command == "dbinom" || command == "tbinom" {
            let top = self.parse_required_argument(true);
            let bottom = self.parse_required_argument(true);
            return format!("({top} choose {bottom})");
        }
        if let Some(mark) = accent(&command) {
            let value = self.parse_required_argument(true);
            return if value.chars().count() == 1 {
                format!("{value}{mark}")
            } else {
                format!("{command}({value})")
            };
        }
        if command == "mathbb" {
            let value = self.parse_required_argument(true);
            return value
                .chars()
                .map(|character| blackboard(character).unwrap_or(character))
                .collect();
        }
        if command == "operatorname" {
            let starred = self.source.get(self.position) == Some(&'*');
            if starred {
                self.position += 1;
            }
            let operator = normalize_output(&self.parse_required_argument(true))
                .trim()
                .to_string();
            return self.parse_operator(&operator, LowerStyle::Bracket, starred, true);
        }
        if command == "mod" || command == "bmod" {
            return " mod ".to_string();
        }
        if command == "pmod" || command == "pod" {
            let value = self.parse_required_argument(true).trim().to_string();
            return if command == "pmod" {
                format!(" (mod {value})")
            } else {
                format!(" ({value})")
            };
        }
        if command == "overset" || command == "stackrel" {
            let upper = self.parse_required_argument(true);
            let value = self.parse_required_argument(true).trim().to_string();
            return format!("{value}{}", format_script(&upper, Script::Sup));
        }
        if command == "underset" {
            let lower = self.parse_required_argument(true);
            let value = self.parse_required_argument(true).trim().to_string();
            return format!("{value}{}", format_script(&lower, Script::Sub));
        }
        if is_plain_wrapper(&command) {
            let value = self.parse_required_argument(true);
            return if command.starts_with("text") || command == "mbox" {
                value
            } else {
                value.trim().to_string()
            };
        }
        if command == "begin" {
            return self.parse_environment();
        }
        if command == "end" {
            self.supported = false;
            return String::new();
        }

        self.supported = false;
        format!("\\{command}")
    }

    fn parse_operator(
        &mut self,
        operator: &str,
        inline_lower_style: LowerStyle,
        display_limits: bool,
        spaced: bool,
    ) -> String {
        let mut use_display_limits = display_limits;
        let mut modifier_position = self.position;
        while modifier_position < self.source.len()
            && matches!(self.source[modifier_position], ' ' | '\t')
        {
            modifier_position += 1;
        }
        if let Some((limits, consumed)) = match_limits_modifier(&self.source, modifier_position) {
            use_display_limits = limits;
            self.position = modifier_position + consumed;
        }

        let mut lower: Option<String> = None;
        let mut upper: Option<String> = None;
        loop {
            let mut script_position = self.position;
            while script_position < self.source.len()
                && matches!(self.source[script_position], ' ' | '\t')
            {
                script_position += 1;
            }
            let kind = match self.source.get(script_position) {
                Some(character) => *character,
                None => break,
            };
            if kind != '_' && kind != '^' {
                break;
            }
            self.position = script_position + 1;
            let value = normalize_output(&self.parse_required_argument(false)).replace(' ', "");
            if kind == '_' {
                if lower.is_some() {
                    self.supported = false;
                }
                lower = Some(value);
            } else {
                if upper.is_some() {
                    self.supported = false;
                }
                upper = Some(value);
            }
        }

        if self.display && use_display_limits && (lower.is_some() || upper.is_some()) {
            let index = self.layout_nodes.len();
            self.layout_nodes.push(LayoutNode::Operator {
                operator: operator.to_string(),
                lower,
                upper,
            });
            return format!("{LAYOUT_MARKER_START}{index}{LAYOUT_MARKER_END}");
        }

        let mut rendered = operator.to_string();
        if let Some(lower) = &lower {
            rendered.push_str(&match inline_lower_style {
                LowerStyle::Bracket => format!("[{lower}]"),
                LowerStyle::Script => format_script(lower, Script::Sub),
            });
        }
        if let Some(upper) = &upper {
            rendered.push_str(&format_script(upper, Script::Sup));
        }
        if spaced {
            format!(" {rendered} ")
        } else {
            rendered
        }
    }

    fn parse_required_argument(&mut self, stack_fractions: bool) -> String {
        let previous_stack_fractions = self.stack_fractions;
        self.stack_fractions = previous_stack_fractions && stack_fractions;
        let value = self.parse_required_argument_value();
        self.stack_fractions = previous_stack_fractions;
        value
    }

    fn parse_required_argument_value(&mut self) -> String {
        while self.position < self.source.len() && self.source[self.position].is_whitespace() {
            self.position += 1;
        }
        if self.position >= self.source.len() {
            self.supported = false;
            return String::new();
        }
        if self.source[self.position] == '{' {
            self.position += 1;
            return self.parse_sequence(Some('}'));
        }
        if self.source[self.position] == '\\' {
            return self.parse_command();
        }
        let value = self.source[self.position];
        self.position += 1;
        value.to_string()
    }

    fn parse_optional_argument(&mut self) -> Option<String> {
        while self.position < self.source.len() && matches!(self.source[self.position], ' ' | '\t')
        {
            self.position += 1;
        }
        if self.source.get(self.position) != Some(&'[') {
            return None;
        }
        let end = self
            .source
            .iter()
            .enumerate()
            .skip(self.position + 1)
            .find(|(_, character)| **character == ']')
            .map(|(index, _)| index);
        let Some(end) = end else {
            self.supported = false;
            return None;
        };
        let value: String = self.source[self.position + 1..end].iter().collect();
        self.position = end + 1;
        Some(self.render_nested(&value, true))
    }

    fn read_raw_group(&mut self) -> Option<String> {
        while self.position < self.source.len() && matches!(self.source[self.position], ' ' | '\t')
        {
            self.position += 1;
        }
        if self.source.get(self.position) != Some(&'{') {
            self.supported = false;
            return None;
        }

        self.position += 1;
        let start = self.position;
        let mut depth = 1usize;
        while self.position < self.source.len() {
            let character = self.source[self.position];
            if character == '\\' {
                self.position += 2;
                continue;
            }
            if character == '{' {
                depth += 1;
            }
            if character == '}' {
                depth -= 1;
            }
            if depth == 0 {
                let value: String = self.source[start..self.position].iter().collect();
                self.position += 1;
                return Some(value);
            }
            self.position += 1;
        }
        self.supported = false;
        None
    }

    /// Split an environment body on `\\`, dropping an optional `[…]` spacing argument.
    fn split_environment_rows(body: &str) -> Vec<String> {
        let chars: Vec<char> = body.chars().collect();
        let mut rows = Vec::new();
        let mut current = String::new();
        let mut index = 0usize;
        while index < chars.len() {
            if chars[index] == '\\' && chars.get(index + 1) == Some(&'\\') {
                let mut consumed = index + 2;
                if chars.get(consumed) == Some(&'[') {
                    let mut cursor = consumed + 1;
                    let mut closed = false;
                    while cursor < chars.len() {
                        if chars[cursor] == ']' {
                            closed = true;
                            break;
                        }
                        if chars[cursor] == '\n' {
                            break;
                        }
                        cursor += 1;
                    }
                    if closed {
                        consumed = cursor + 1;
                    }
                }
                rows.push(std::mem::take(&mut current));
                index = consumed;
                continue;
            }
            current.push(chars[index]);
            index += 1;
        }
        rows.push(current);
        rows
    }

    fn parse_environment(&mut self) -> String {
        let Some(environment) = self.read_raw_group() else {
            return String::new();
        };
        let end_marker = format!("\\end{{{environment}}}");
        let Some(end) = find_subsequence(&self.source, self.position, &end_marker) else {
            self.supported = false;
            return String::new();
        };
        let body: String = self.source[self.position..end].iter().collect();
        self.position = end + end_marker.chars().count();

        if matches!(
            environment.as_str(),
            "equation" | "equation*" | "displaymath"
        ) {
            return self.render_nested(&body, true).trim().to_string();
        }

        if matches!(
            environment.as_str(),
            "aligned"
                | "align"
                | "align*"
                | "alignedat"
                | "alignat"
                | "alignat*"
                | "gather"
                | "gathered"
                | "multline"
                | "multline*"
                | "split"
        ) {
            let aligned_at = matches!(environment.as_str(), "alignedat" | "alignat" | "alignat*");
            let aligned_body = if aligned_at {
                strip_leading_group(&body)
            } else {
                body
            };
            let rendered: Vec<String> = Self::split_environment_rows(&aligned_body)
                .iter()
                .map(|row| {
                    let cells: Vec<&str> = row.split('&').collect();
                    let source = if aligned_at {
                        let mut parts = Vec::new();
                        let mut index = 0usize;
                        while index < cells.len() {
                            parts.push(format!(
                                "{}{}",
                                cells[index],
                                cells.get(index + 1).copied().unwrap_or("")
                            ));
                            index += 2;
                        }
                        parts.join(" ")
                    } else {
                        cells.join("")
                    };
                    self.render_nested(&source, true).trim().to_string()
                })
                .filter(|row| !row.is_empty())
                .collect();
            return rendered.join("\n");
        }

        if matches!(environment.as_str(), "cases" | "cases*") {
            let rows: Vec<Vec<String>> = Self::split_environment_rows(&body)
                .iter()
                .map(|row| {
                    row.split('&')
                        .map(|cell| self.render_nested(cell, false).trim().to_string())
                        .collect()
                })
                .filter(|row: &Vec<String>| row.iter().any(|cell| !cell.is_empty()))
                .collect();
            let count = rows.len();
            let mut rendered_rows = Vec::new();
            for (index, row) in rows.iter().enumerate() {
                let value = strip_trailing_comma(row.first().map(String::as_str).unwrap_or(""));
                let condition = row.get(1).map(String::as_str).unwrap_or("");
                let delimiter = if index == 0 {
                    "⎧"
                } else if index + 1 == count {
                    "⎩"
                } else {
                    "⎨"
                };
                let condition_prefix = if starts_with_condition_keyword(condition) {
                    " "
                } else {
                    " if "
                };
                let mut line = format!("{delimiter} {value}");
                if !condition.is_empty() {
                    line.push_str(condition_prefix);
                    line.push_str(condition);
                }
                rendered_rows.push(line);
            }
            return rendered_rows.join("\n");
        }

        if matches!(
            environment.as_str(),
            "array"
                | "matrix"
                | "smallmatrix"
                | "pmatrix"
                | "bmatrix"
                | "Bmatrix"
                | "vmatrix"
                | "Vmatrix"
        ) {
            let matrix_body = if environment == "array" {
                strip_leading_group(&body)
            } else {
                body
            };
            return self.render_matrix(&environment, &matrix_body);
        }

        self.supported = false;
        body
    }

    fn render_matrix(&mut self, environment: &str, body: &str) -> String {
        let matrix: Vec<Vec<String>> = Self::split_environment_rows(body)
            .iter()
            .map(|row| {
                row.split('&')
                    .map(|cell| self.render_nested(cell, false).trim().to_string())
                    .collect()
            })
            .filter(|row: &Vec<String>| row.iter().any(|cell| !cell.is_empty()))
            .collect();
        let column_count = matrix.iter().map(|row| row.len()).max().unwrap_or(0);
        let column_widths: Vec<usize> = (0..column_count)
            .map(|column| {
                matrix
                    .iter()
                    .map(|row| visible_width(row.get(column).map(String::as_str).unwrap_or("")))
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let rows: Vec<String> = matrix
            .iter()
            .map(|row| {
                (0..column_count)
                    .map(|column| {
                        let cell = row.get(column).map(String::as_str).unwrap_or("");
                        let padding = column_widths[column].saturating_sub(visible_width(cell));
                        let fill: String = iter::repeat(PROTECTED_SPACE).take(padding).collect();
                        format!("{cell}{fill}")
                    })
                    .collect::<Vec<_>>()
                    .join(" │ ")
            })
            .collect();

        let lines: Vec<String> = if matches!(environment, "array" | "matrix" | "smallmatrix") {
            rows
        } else {
            let delimiter = match environment {
                "pmatrix" => ["⎛", "⎞", "⎜", "⎟", "⎝", "⎠"],
                "bmatrix" => ["⎡", "⎤", "⎢", "⎥", "⎣", "⎦"],
                "Bmatrix" => ["⎧", "⎫", "⎨", "⎬", "⎩", "⎭"],
                "vmatrix" => ["│", "│", "│", "│", "│", "│"],
                "Vmatrix" => ["║", "║", "║", "║", "║", "║"],
                _ => {
                    self.supported = false;
                    return rows.join("\n");
                }
            };
            rows.iter()
                .enumerate()
                .map(|(index, row)| {
                    let last = index + 1 == rows.len();
                    let left = if index == 0 {
                        delimiter[0]
                    } else if last {
                        delimiter[4]
                    } else {
                        delimiter[2]
                    };
                    let right = if index == 0 {
                        delimiter[1]
                    } else if last {
                        delimiter[5]
                    } else {
                        delimiter[3]
                    };
                    format!("{left} {row} {right}")
                })
                .collect()
        };

        if lines.len() <= 1 {
            return lines.first().cloned().unwrap_or_default();
        }
        let index = self.layout_nodes.len();
        self.layout_nodes
            .push(LayoutNode::Matrix { lines, baseline: 0 });
        format!("{LAYOUT_MARKER_START}{index}{LAYOUT_MARKER_END}")
    }

    fn render_nested(&mut self, source: &str, stack_fractions: bool) -> String {
        let display = self.display && stack_fractions;
        let rendered = {
            let mut parser = LatexParser::new(source, &mut *self.layout_nodes, display);
            parser.render()
        };
        match rendered {
            Some(rendered) => rendered,
            None => {
                self.supported = false;
                source.to_string()
            }
        }
    }
}

/// Index of a trailing `\u{f0000}<digits>\u{f0001}` marker in `value`.
fn trailing_layout_marker_index(value: &str) -> Option<usize> {
    let chars: Vec<char> = value.chars().collect();
    if chars.last() != Some(&LAYOUT_MARKER_END) {
        return None;
    }
    let mut index = chars.len() - 1;
    let mut digits: Vec<char> = Vec::new();
    while index > 0 && chars[index - 1].is_ascii_digit() {
        digits.push(chars[index - 1]);
        index -= 1;
    }
    if digits.is_empty() || index == 0 || chars[index - 1] != LAYOUT_MARKER_START {
        return None;
    }
    digits.iter().rev().collect::<String>().parse().ok()
}

/// Match `\limits` / `\nolimits` at `start`; returns `(is_limits, length)`.
fn match_limits_modifier(source: &[char], start: usize) -> Option<(bool, usize)> {
    for (text, limits) in [("\\limits", true), ("\\nolimits", false)] {
        let expected: Vec<char> = text.chars().collect();
        if start + expected.len() <= source.len()
            && source[start..start + expected.len()] == expected[..]
        {
            match source.get(start + expected.len()) {
                Some(character) if character.is_ascii_alphabetic() => {}
                _ => return Some((limits, expected.len())),
            }
        }
    }
    None
}

/// Index of `needle` in `haystack` at or after `from`.
fn find_subsequence(haystack: &[char], from: usize, needle: &str) -> Option<usize> {
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() || from > haystack.len() {
        return None;
    }
    let mut index = from;
    while index + needle.len() <= haystack.len() {
        if haystack[index..index + needle.len()] == needle[..] {
            return Some(index);
        }
        index += 1;
    }
    None
}

/// Drop a leading `{…}` group (used by `array` / `alignat`).
fn strip_leading_group(body: &str) -> String {
    let chars: Vec<char> = body.chars().collect();
    let mut index = 0usize;
    while index < chars.len() && chars[index].is_whitespace() {
        index += 1;
    }
    if chars.get(index) != Some(&'{') {
        return body.to_string();
    }
    let mut cursor = index + 1;
    while cursor < chars.len() && chars[cursor] != '}' {
        cursor += 1;
    }
    if cursor >= chars.len() {
        return body.to_string();
    }
    chars[cursor + 1..].iter().collect()
}

/// Remove a trailing `,` plus trailing whitespace (`/,\s*$/`).
fn strip_trailing_comma(value: &str) -> String {
    let trimmed = value.trim_end();
    trimmed.strip_suffix(',').unwrap_or(trimmed).to_string()
}

/// True when `condition` starts with `if`/`when`/`for`/`otherwise` as a word.
fn starts_with_condition_keyword(condition: &str) -> bool {
    let lower = condition.to_lowercase();
    for keyword in ["if", "when", "for", "otherwise"] {
        if let Some(rest) = lower.strip_prefix(keyword) {
            let boundary = rest
                .chars()
                .next()
                .map(|c| !c.is_alphanumeric() && c != '_');
            if boundary.unwrap_or(true) {
                return true;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Render a basic LaTeX math expression as terminal-friendly Unicode text.
///
/// Returns `None` when the expression contains unsupported or malformed syntax,
/// so the caller can fall back to the raw source. Inline mode: fractions stay
/// on one line and operator limits are not stacked.
pub fn render_latex(source: &str) -> Option<String> {
    render_latex_with(source, false)
}

/// Like [`render_latex`], but with display mode enabled when `display` is set:
/// fractions and operator limits are stacked vertically.
pub fn render_latex_with(source: &str, display: bool) -> Option<String> {
    let mut layout_nodes: Vec<LayoutNode> = Vec::new();
    let rendered = {
        let mut parser = LatexParser::new(source, &mut layout_nodes, display);
        parser.render()
    }?;

    if layout_nodes.is_empty() {
        return Some(rendered.replace(PROTECTED_SPACE, " "));
    }

    let lines = render_layout(&rendered, &layout_nodes).lines;
    let indentation = lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.chars().count() - line.trim_start().chars().count())
        .min()
        .unwrap_or(usize::MAX);
    let dedented: Vec<String> = lines
        .iter()
        .map(|line| {
            line.chars()
                .skip(indentation)
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect();
    Some(dedented.join("\n").trim_end().replace(PROTECTED_SPACE, " "))
}
