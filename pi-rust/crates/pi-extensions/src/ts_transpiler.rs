//! TypeScript / TSX → JavaScript transpilation.
//!
//! The upstream pi TypeScript extensions (≈80 of them in
//! `packages/coding-agent/examples/`) are written for `tsc` / `tsx` /
//! `esbuild`. QuickJS — the JS engine embedded in `pi-extensions` —
//! speaks plain ESM JavaScript only. The previous `strip_simple_types`
//! pass only knew how to drop `import type` lines, which left every
//! extension that declared a typed variable, an `interface`, a generic
//! parameter, or a `export const X = …` un-loadable.
//!
//! This module ports the SWC-based transpile pipeline from
//! `pi_agent_rust/src/extensions_js.rs::transpile_typescript_module`:
//!
//!   1. `swc_ecma_parser` parses the source with `Syntax::Typescript`
//!      (decorators on, TSX on for `.tsx`/`.jsx`).
//!   2. `swc_ecma_transforms_base::resolver` runs first to materialize
//!      scope marks (the TS strip pass needs them).
//!   3. `swc_ecma_transforms_typescript::strip` erases every type-only
//!      construct — annotations, generics, interfaces, type aliases,
//!      enums, `declare`, accessibility modifiers, `import type`.
//!   4. `swc_ecma_codegen::Emitter` prints the cleaned AST back to JS.
//!
//! The result is a vanilla ESM source string that the existing
//! shim's `_pi_load_extension` can compile via `new Function(...)`.

use std::path::Path;
use std::sync::OnceLock;

use swc_common::sync::Lrc;
use swc_common::{FileName, GLOBALS, Globals, Mark, SourceMap};
use swc_ecma_ast::{EsVersion, Module as SwcModule, Pass, Program as SwcProgram};
use swc_ecma_codegen::{Emitter, text_writer::JsWriter};
use swc_ecma_parser::{EsSyntax, Parser as SwcParser, StringInput, Syntax, TsSyntax};
use swc_ecma_transforms_base::resolver;
use swc_ecma_transforms_typescript::strip;

use crate::module_cache;

/// One SWC `Globals` is created per call to avoid cross-thread
/// pollution of SWC's thread-local state. To amortize the cost we
/// cache a single `Globals` per thread (SWC itself uses `thread_local`
/// here, but the `GLOBALS` set pattern is the documented one).
thread_local! {
    static SWC_GLOBALS: OnceLock<Globals> = OnceLock::new();
}

/// What kind of source this file is, derived from its extension.
/// Drives both the SWC `Syntax` choice and the loader's CJS/ESM
/// dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    TypeScript,
    TypeScriptJsx,
    JavaScript,
    JavaScriptJsx,
    Json,
    Cjs,
}

impl SourceKind {
    pub fn for_path(path: &Path) -> Option<Self> {
        let ext = path.extension().and_then(|s| s.to_str())?;
        Some(match ext {
            "ts" => Self::TypeScript,
            "tsx" => Self::TypeScriptJsx,
            "cts" => Self::TypeScript,        // CJS-flavored TS
            "mts" => Self::TypeScript,        // ESM-flavored TS
            "jsx" => Self::JavaScriptJsx,
            "js" => Self::JavaScript,
            "mjs" => Self::JavaScript,        // ESM-flavored JS
            "cjs" => Self::Cjs,               // CJS-flavored JS
            "json" => Self::Json,
            _ => return None,
        })
    }

    pub fn is_typescript(self) -> bool {
        matches!(self, Self::TypeScript | Self::TypeScriptJsx)
    }

    pub fn is_jsx(self) -> bool {
        matches!(self, Self::TypeScriptJsx | Self::JavaScriptJsx)
    }
}

/// Strip TypeScript-only syntax from `source` and return the ESM
/// JavaScript equivalent. Returns the input unchanged for non-TS
/// sources, so callers can pass any extension through the same
/// pipeline.
///
/// `name` is used as the SWC `FileName`; it shows up in error
/// messages and parser recovery output.
///
/// If `cache_dir` is `Some`, this function consults and updates the
/// persistent transpiled-source cache (`module_cache`). A cache hit
/// returns the previously-stored output without re-running SWC.
pub fn transpile(source: &str, name: &str, kind: SourceKind) -> Result<String, String> {
    if !kind.is_typescript() {
        return Ok(source.to_string());
    }
    transpile_with_cache(source, name, kind, None)
}

/// Same as [`transpile`] but with an explicit cache directory. A
/// `Some(dir)` enables the cache; `None` forces a fresh transpile.
pub fn transpile_with_cache(
    source: &str,
    name: &str,
    kind: SourceKind,
    cache_dir: Option<&Path>,
) -> Result<String, String> {
    if !kind.is_typescript() {
        return Ok(source.to_string());
    }
    match cache_dir {
        Some(dir) => Ok(module_cache::get_or_insert_with(dir, source, name, || {
            run_swc(source, name, kind)
        })),
        None => run_swc(source, name, kind),
    }
}

fn run_swc(source: &str, name: &str, kind: SourceKind) -> Result<String, String> {
    SWC_GLOBALS.with(|cell| {
        let globals = cell.get_or_init(Globals::new);
        GLOBALS.set(globals, || transpile_inner(source, name, kind))
    })
}

fn transpile_inner(source: &str, name: &str, kind: SourceKind) -> Result<String, String> {
    let cm: Lrc<SourceMap> = Lrc::default();
    let fm = cm.new_source_file(FileName::Custom(name.to_string()).into(), source.to_string());

    let syntax = Syntax::Typescript(TsSyntax {
        tsx: kind.is_jsx(),
        decorators: true,
        ..Default::default()
    });

    let mut parser = SwcParser::new(syntax, StringInput::from(&*fm), None);
    let module: SwcModule = parser
        .parse_module()
        .map_err(|err| format!("parse {name}: {err:?}"))?;

    let unresolved_mark = Mark::new();
    let top_level_mark = Mark::new();
    let mut program = SwcProgram::Module(module);

    // 1. Scope resolution — `strip` needs the unresolved / top-level
    //    marks to know which identifiers are types vs values.
    {
        let mut pass = resolver(unresolved_mark, top_level_mark, false);
        pass.process(&mut program);
    }
    // 2. TypeScript strip — the actual TS→JS transform.
    {
        let mut pass = strip(unresolved_mark, top_level_mark);
        pass.process(&mut program);
    }

    let SwcProgram::Module(module) = program else {
        return Err(format!("transpile {name}: expected module"));
    };

    // 3. Emit JS.
    let mut buf = Vec::new();
    {
        let mut emitter = Emitter {
            cfg: swc_ecma_codegen::Config::default()
                .with_minify(false)
                .with_target(EsVersion::Es2022),
            comments: None,
            cm: cm.clone(),
            wr: JsWriter::new(cm, "\n", &mut buf, None),
        };
        emitter
            .emit_module(&module)
            .map_err(|err| format!("emit {name}: {err}"))?;
    }

    String::from_utf8(buf).map_err(|err| format!("utf8 {name}: {err}"))
}

/// JSON module → ESM. JSON files export their parsed value as the
/// `default` export so they look like `import data from "./foo.json"`.
pub fn json_to_esm(raw: &str, name: &str) -> Result<String, String> {
    let value: serde_json::Value = serde_json::from_str(raw)
        .map_err(|err| format!("json parse {name}: {err}"))?;
    Ok(format!(
        "export default {};\n",
        serde_json::to_string(&value).map_err(|err| format!("json re-emit {name}: {err}"))?
    ))
}

/// Re-parse a TypeScript source as plain JavaScript, ignoring every
/// TS-only construct. Used by [`crate::auto_repair`] when SWC's strip
/// pass fails: a TypeScript source that SWC rejects is sometimes
/// still valid JavaScript after a few type annotations are dropped
/// by hand (or accepted as parse errors by QuickJS).
///
/// The returned source is *not* guaranteed to run — TS-only syntax
/// like `<T>` generics in JSX will leak through — but the auto-repair
/// caller accepts that: the goal is graceful degradation, not full
/// type erasure.
pub fn transpile_as_es(source: &str, name: &str, kind: SourceKind) -> Result<String, String> {
    if !kind.is_typescript() {
        return Err(format!(
            "transpile_as_es called with non-ts kind {kind:?} for {name}"
        ));
    }
    SWC_GLOBALS.with(|cell| {
        let globals = cell.get_or_init(Globals::new);
        GLOBALS.set(globals, || transpile_as_es_inner(source, name, kind))
    })
}

fn transpile_as_es_inner(source: &str, name: &str, kind: SourceKind) -> Result<String, String> {
    let cm: Lrc<SourceMap> = Lrc::default();
    let fm = cm.new_source_file(FileName::Custom(name.to_string()).into(), source.to_string());

    let syntax = Syntax::Es(EsSyntax {
        jsx: kind.is_jsx(),
        ..Default::default()
    });

    let mut parser = SwcParser::new(syntax, StringInput::from(&*fm), None);
    let module: SwcModule = parser
        .parse_module()
        .map_err(|err| format!("parse-as-es {name}: {err:?}"))?;

    let mut buf = Vec::new();
    {
        let mut emitter = Emitter {
            cfg: swc_ecma_codegen::Config::default()
                .with_minify(false)
                .with_target(EsVersion::Es2022),
            comments: None,
            cm: cm.clone(),
            wr: JsWriter::new(cm, "\n", &mut buf, None),
        };
        emitter
            .emit_module(&module)
            .map_err(|err| format!("emit-as-es {name}: {err}"))?;
    }
    String::from_utf8(buf).map_err(|err| format!("utf8-as-es {name}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_kind_classifies_extensions() {
        assert_eq!(
            SourceKind::for_path(Path::new("/x/y.ts")),
            Some(SourceKind::TypeScript)
        );
        assert_eq!(
            SourceKind::for_path(Path::new("/x/y.tsx")),
            Some(SourceKind::TypeScriptJsx)
        );
        assert_eq!(
            SourceKind::for_path(Path::new("/x/y.cts")),
            Some(SourceKind::TypeScript)
        );
        assert_eq!(
            SourceKind::for_path(Path::new("/x/y.mts")),
            Some(SourceKind::TypeScript)
        );
        assert_eq!(
            SourceKind::for_path(Path::new("/x/y.js")),
            Some(SourceKind::JavaScript)
        );
        assert_eq!(
            SourceKind::for_path(Path::new("/x/y.mjs")),
            Some(SourceKind::JavaScript)
        );
        assert_eq!(
            SourceKind::for_path(Path::new("/x/y.cjs")),
            Some(SourceKind::Cjs)
        );
        assert_eq!(
            SourceKind::for_path(Path::new("/x/y.json")),
            Some(SourceKind::Json)
        );
        assert_eq!(SourceKind::for_path(Path::new("/x/y.txt")), None);
    }

    #[test]
    fn non_ts_source_passes_through() {
        let src = "const x = 1; export default x;";
        let out = transpile(src, "noop.js", SourceKind::JavaScript).unwrap();
        assert_eq!(out, src);
    }

    #[test]
    fn strips_simple_type_annotations() {
        let src = "const x: number = 1;\nexport default x;\n";
        let out = transpile(src, "a.ts", SourceKind::TypeScript).unwrap();
        assert!(!out.contains("number"), "type leaked: {out}");
        assert!(out.contains("const x"), "binding lost: {out}");
        assert!(out.contains("export default"), "default lost: {out}");
    }

    #[test]
    fn strips_generic_type_arguments() {
        let src = r#"
            const m: Record<string, number> = { a: 1 };
            function f<T extends string>(x: T): T { return x; }
            export default { m, f };
        "#;
        let out = transpile(src, "b.ts", SourceKind::TypeScript).unwrap();
        assert!(!out.contains("Record<string"), "type leaked: {out}");
        assert!(!out.contains("extends string"), "type leaked: {out}");
        assert!(!out.contains(": T"), "annotation leaked: {out}");
    }

    #[test]
    fn strips_interface_type_alias_enum_declare() {
        let src = r#"
            interface Foo { x: number }
            type Bar = { y: string };
            enum E { A, B }
            declare const globalThing: unknown;
            const out = "ok";
            export default out;
        "#;
        let out = transpile(src, "c.ts", SourceKind::TypeScript).unwrap();
        assert!(!out.contains("interface"), "interface leaked: {out}");
        assert!(!out.contains("type Bar"), "type alias leaked: {out}");
        assert!(!out.contains("enum E"), "enum leaked: {out}");
        assert!(!out.contains("declare const"), "declare leaked: {out}");
        assert!(out.contains("const out"), "binding lost: {out}");
    }

    #[test]
    fn strips_import_type_and_type_only_specifiers() {
        let src = r#"
            import type { Foo } from "./foo";
            import { type Bar, baz } from "./bar";
            export default baz;
        "#;
        let out = transpile(src, "d.ts", SourceKind::TypeScript).unwrap();
        // `import type` whole line drops; mixed import keeps the value specifier.
        assert!(out.contains("baz"), "value specifier lost: {out}");
    }

    #[test]
    fn strips_type_assertions() {
        let src = r#"
            const v = (window as unknown as { hello: () => void }).hello();
            export default v;
        "#;
        let out = transpile(src, "e.ts", SourceKind::TypeScript).unwrap();
        assert!(!out.contains("as unknown"), "type assertion leaked: {out}");
    }

    #[test]
    fn strips_export_const_and_export_let() {
        let src = r#"
            export const FOO: number = 1;
            export let bar = "hi";
            export default function (pi: unknown) { return pi; }
        "#;
        let out = transpile(src, "f.ts", SourceKind::TypeScript).unwrap();
        assert!(out.contains("const FOO"), "FOO lost: {out}");
        assert!(out.contains("let bar"), "bar lost: {out}");
        assert!(!out.contains(": number"), "annotation leaked: {out}");
    }

    #[test]
    fn json_module_emits_esm_default() {
        let src = r#"{"name": "x", "version": 1}"#;
        let out = json_to_esm(src, "p.json").unwrap();
        assert!(out.contains("export default"), "default missing: {out}");
        assert!(out.contains("\"name\""), "value missing: {out}");
    }
}
