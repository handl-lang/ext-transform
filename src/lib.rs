//! `hnl-ext-transform` — the `transform` extension for `hnl` (ADR-0149).
//!
//! The transform machinery lives in `hnl-tool-transform-rewrite`
//! (`RewriteEngine`, ADR-0094) over the language's declarative
//! `lang.transforms` rules; this crate is the *packaging*: it exports the
//! ADR-0149 ABI (`handl_ext_manifest` / `handl_ext_dispatch` /
//! `handl_ext_free`) and implements the
//! `hnl transform <pass> <file.hnl> [--pipeline a,b,c] [--plan] [--trace]
//! [--stats] [--emit tree|json|source] [--write]` command surface —
//! byte-for-byte the semantics the built-in `transform` arm had before the
//! host went dumb.
//!
//! `transform` runs a named pass over the RAW, pre-desugar AST:
//! `Runtime::parse` applies `desugar` at parse time (ADR-0082), so this
//! command parses via `lex` + `PrattEngine` directly — the same raw-parse
//! path the bootstrap typechecker uses — then runs the pass through
//! `RewriteEngine` with the desugar natives + the kit's `.hnl` rewrite
//! modules layered underneath.
//!
//! The two-file categorical form (`hnl transform <rules.hnl> <program.hnl>`)
//! routes to [`transform_categorical`] — the same module the built-in host
//! linked (verbatim, shared with `ext-eval`'s `n_dim_opt` wiring).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use handl_ext::{ExtRequest, ExtResponse, ExtensionManifest};
use handl_kit_prelude::Kit;
use handl_language::LangError;
use serde_json::json;

// Verbatim copy of `ext-eval`'s module: its `run`/`pipeline_value`
// two-file surface is used here; the `n_dim_opt` helpers ext-eval wires
// into `hnl run` stay available but unused by the named-pass path.
#[allow(dead_code)]
mod transform_categorical;

handl_ext::export!(
    ExtensionManifest::new("transform")
        .version(env!("CARGO_PKG_VERSION"))
        .command(
            "transform",
            "transform <pass> <file.hnl> [--pipeline a,b,c] [--plan] [--trace] [--stats] [--emit tree|json|source] [--write]",
        ),
    dispatch
);

/// ABI dispatch: only the `transform` command is declared.
fn dispatch(cmd: &str, req: &ExtRequest) -> ExtResponse {
    match cmd {
        "transform" => run(&req.args, Path::new(&req.cwd)),
        other => ExtResponse::error(2, format!("transform: unknown command `{other}`\n")),
    }
}

// ---------------------------------------------------------------------------
// `transform <pass> <file> [--pipeline a,b,c] [--plan] [--trace] [--stats] [--emit tree|json]`
// ---------------------------------------------------------------------------

/// Output accumulator: the extension renders one `ExtResponse` instead of
/// printing, so stdout/stderr collect here and the code is returned.
///
/// (The two-file categorical driver keeps its own println!/eprintln! —
/// in-process those land on the host's streams verbatim.)
#[derive(Default)]
struct Out {
    stdout: String,
    stderr: String,
}

impl Out {
    fn out(&mut self, msg: impl std::fmt::Display) {
        self.stdout.push_str(&format!("{msg}\n"));
    }
    fn err(&mut self, msg: impl std::fmt::Display) {
        self.stderr.push_str(&format!("{msg}\n"));
    }
}

/// Parsed `hnl transform` arguments.
struct TransformArgs {
    /// The pass name (first positional) — ignored when `pipeline` is set.
    pass: Option<String>,
    /// The input file (second positional).
    file: Option<String>,
    /// `--pipeline a,b,c`: ordered pass list (overrides `pass`).
    pipeline: Option<Vec<String>>,
    /// `--plan`: report match sites without applying.
    plan: bool,
    /// `--trace`: print each rewrite.
    trace: bool,
    /// `--stats`: print `PassStats`.
    stats: bool,
    /// `--write`: emit preserving source and overwrite the file.
    write: bool,
    /// `--emit tree|json|source` (default `tree`).
    emit: String,
}

fn run(args: &[String], cwd: &Path) -> ExtResponse {
    let mut out = Out::default();
    let mut t = TransformArgs {
        pass: None,
        file: None,
        pipeline: None,
        plan: false,
        trace: false,
        stats: false,
        write: false,
        emit: "tree".to_string(),
    };

    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--pipeline" => {
                i += 1;
                match args.get(i) {
                    Some(v) => {
                        t.pipeline = Some(
                            v.split(',')
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                                .collect(),
                        )
                    }
                    None => {
                        out.err("transform: --pipeline requires a comma-separated value");
                        return code(2, out);
                    }
                }
            }
            "--emit" => {
                i += 1;
                match args.get(i) {
                    Some(v) => t.emit = v.clone(),
                    None => {
                        out.err("transform: --emit requires a value (tree|json|source)");
                        return code(2, out);
                    }
                }
            }
            "--plan" => t.plan = true,
            "--trace" => t.trace = true,
            "--stats" => t.stats = true,
            "--write" => t.write = true,
            "--" => {}
            s if s.starts_with("--") => {
                out.err(format!("transform: unknown flag `{s}`"));
                return code(2, out);
            }
            s => positional.push(s.to_string()),
        }
        i += 1;
    }

    // Positionals: `<pass> <file>`. When `--pipeline` is given, the pass
    // positional is optional — the last positional is always the file.
    if t.pipeline.is_some() {
        t.file = positional.pop();
        t.pass = positional.pop();
        if !positional.is_empty() {
            out.err("transform: too many positional arguments");
            return code(2, out);
        }
    } else {
        let mut it = positional.into_iter();
        t.pass = it.next();
        t.file = it.next();
        if it.next().is_some() {
            out.err("transform: too many positional arguments");
            return code(2, out);
        }
    }

    // Categorical two-file mode: `hnl transform <rules.hnl> <program.hnl>`.
    // The first positional ends in `.hnl` and exists, so route to the
    // prototype categorical rewrites rather than the named-pass engine. The
    // driver prints to the process streams directly (shared with the host).
    if let (Some(rules_path), Some(prog_path)) = (&t.pass, &t.file)
        && rules_path.ends_with(".hnl")
        && prog_path.ends_with(".hnl")
    {
        // Paths as given: the extension shares the host's process CWD
        // (`req.cwd`), and error messages print the literal argument.
        let c = transform_categorical::run(rules_path, prog_path);
        return code(c, out);
    }

    if t.emit != "tree" && t.emit != "json" && t.emit != "source" {
        out.err(format!(
            "transform: unknown emit format `{}` (expected tree|json|source)",
            t.emit
        ));
        return code(2, out);
    }

    if t.write && t.emit != "tree" && t.emit != "source" {
        out.err("transform: --write requires `--emit source`");
        return code(2, out);
    }

    let file = match &t.file {
        Some(f) => f.clone(),
        None => {
            out.err(
                "usage: hnl transform <pass> <file.hnl> [--pipeline a,b,c] [--plan] [--trace] [--stats] [--emit tree|json|source] [--write]",
            );
            return code(2, out);
        }
    };
    if t.pass.is_none() && t.pipeline.is_none() {
        out.err("transform: missing <pass> (or --pipeline)");
        return code(2, out);
    }

    let disk = resolve(cwd, &file);
    let src = match std::fs::read_to_string(&disk) {
        Ok(s) => s,
        Err(e) => {
            out.err(format!("transform: failed to read {file}: {e}"));
            return code(2, out);
        }
    };

    let kit = match Kit::new() {
        Ok(k) => k,
        Err(e) => {
            out.err(format!("transform: {e}"));
            return code(1, out);
        }
    };
    let lang = kit.runtime().lang();

    // Validate the requested pass(es): every pass must declare at least one
    // rule in `lang.transforms`. `"desugar"` additionally has the native
    // fallback table, but it always has declarative rules too.
    let passes: Vec<String> = match &t.pipeline {
        Some(ps) => ps.clone(),
        None => vec![t.pass.clone().unwrap()],
    };
    for p in &passes {
        if !lang.transforms.iter().any(|d| d.pass == *p) {
            let mut known: Vec<String> = lang
                .transforms
                .iter()
                .map(|d| d.pass.clone())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            known.sort();
            out.err(format!(
                "transform: unknown pass `{p}` (known passes: {})",
                known.join(", ")
            ));
            return code(2, out);
        }
    }

    // Raw-parse each top-level statement (split at bracket-depth zero, the
    // same convention as the typechecker/linter/test runner) — no desugar.
    let stmts = match transform_parse_raw(lang, &src) {
        Ok(s) => s,
        Err(e) => {
            out.err(format!("transform: {file}: parse error: {e}"));
            return code(2, out);
        }
    };

    // Build the handl-native rewrite registry (ADR-0094) from the kit's
    // `.hnl` modules, layer the `desugar.*` natives underneath as a
    // fallback, and wire both into the engine. Without this, a
    // `transforms.yaml` rule with `rule: !Native "handl:<mod>.<fn>"` errors
    // as "unknown native" — the adapter is the production seam.
    let handl_natives = Arc::new(kit.handl_rewrite_adapter().with_fallback(Arc::new(
        hnl_tool_transform_rewrite::native::desugar_native_registry(),
    )));
    let engine = hnl_tool_transform_rewrite::RewriteEngine::new()
        .with_desugar_and_handl_natives(handl_natives);

    if t.plan {
        // Match-only: report match sites + rule ids without rewriting.
        let mut count = 0usize;
        for (n, (ast, _)) in stmts.iter().enumerate() {
            let root = handl_language::AstRef::new(ast.len() - 1);
            let sites: Vec<_> = passes
                .iter()
                .flat_map(|p| engine.find_matches(lang, ast, root, p))
                .collect();
            if sites.is_empty() {
                continue;
            }
            if stmts.len() > 1 {
                out.out(format!("-- statement {}", n + 1));
            }
            for m in sites {
                let node = ast.get(m.at);
                let kind = node.map(|nd| nd.kind.as_str()).unwrap_or("?");
                let span = node
                    .and_then(|nd| nd.span)
                    .map(|s| format!(" @{}..{}", s.start, s.end))
                    .unwrap_or_default();
                out.out(format!("match {kind}{span} <- {}", m.rule_id));
                count += 1;
            }
        }
        out.out(format!("plan: {count} match site(s)"));
        return code(0, out);
    }

    let pass_refs: Vec<&str> = passes.iter().map(String::as_str).collect();
    let mut json_out: Vec<serde_json::Value> = Vec::new();
    let mut source_parts: Vec<String> = Vec::new();
    let mut had_error = false;

    for (n, (ast, info)) in stmts.iter().enumerate() {
        let root = handl_language::AstRef::new(ast.len() - 1);
        let res = if t.pipeline.is_some() {
            engine.run_pipeline(lang, ast, root, &pass_refs)
        } else {
            engine.run_pass(lang, ast, root, pass_refs[0])
        };
        let res = match res {
            Ok(o) => o,
            Err(e) => {
                out.err(format!("transform: {file}: pass failed: {e}"));
                had_error = true;
                continue;
            }
        };

        if t.write || t.emit == "source" {
            if res.trace.rewrites.is_empty() {
                source_parts.push(src[info.start..info.end].to_string());
            } else {
                if let Err(kinds) = check_emittable(lang, &res.ast, res.root) {
                    out.err(format!(
                        "transform: {file}: pass '{}' produced internal-form node(s): {}",
                        passes.join(", "),
                        kinds.join(", ")
                    ));
                    out.err(
                        "  --emit source and --write are only meaningful for surface-preserving passes",
                    );
                    had_error = true;
                    continue;
                }
                let changed: Vec<handl_language::AstRef> =
                    res.trace.rewrites.iter().map(|r| r.to).collect();
                let emitter =
                    match hnl_tool_fmt::PreserveEmitter::new(&src, &res.ast, res.root, &changed) {
                        Ok(e) => e,
                        Err(e) => {
                            out.err(format!(
                                "transform: {file}: emit failed for statement {}: {e}",
                                n + 1
                            ));
                            had_error = true;
                            continue;
                        }
                    };
                match emitter.format(lang, &res.ast, res.root) {
                    Ok(s) => source_parts.push(s),
                    Err(e) => {
                        out.err(format!(
                            "transform: {file}: emit failed for statement {}: {e}",
                            n + 1
                        ));
                        had_error = true;
                        continue;
                    }
                }
            }
        } else if t.emit == "json" {
            json_out.push(json!({
                "statement": n + 1,
                "root": res.root.index(),
                "nodes": res.ast.nodes(),
            }));
        } else {
            if stmts.len() > 1 {
                out.out(format!("-- statement {}", n + 1));
            }
            let mut buf = String::new();
            render_ast_tree(&res.ast, res.root.index(), 0, &mut buf);
            out.stdout.push_str(&buf);
        }

        if t.trace {
            for r in &res.trace.rewrites {
                let from_kind = ast.get(r.from).map(|nd| nd.kind.as_str()).unwrap_or("?");
                let to_kind = res.ast.get(r.to).map(|nd| nd.kind.as_str()).unwrap_or("?");
                out.out(format!(
                    "rewrite #{} {from_kind} -> #{} {to_kind} [{}]",
                    r.from.index(),
                    r.to.index(),
                    r.rule_id
                ));
            }
        }

        if t.stats {
            out.out(format!("visited: {}", res.stats.visited));
            out.out(format!("rewritten: {}", res.stats.rewritten));
            out.out(format!(
                "duration_ms: {:.3}",
                res.stats.duration.as_secs_f64() * 1000.0
            ));
            for (rule, count) in &res.stats.per_rule {
                out.out(format!("rule {rule}: {count}"));
            }
        }
    }

    if t.emit == "json" {
        out.out(serde_json::to_string_pretty(&json_out).unwrap_or_default());
    } else if t.write {
        if had_error {
            return code(1, out);
        }
        let mut emitted = String::new();
        if !stmts.is_empty() {
            emitted.push_str(&src[0..stmts[0].1.start]);
            for (i, part) in source_parts.iter().enumerate() {
                emitted.push_str(part);
                if i + 1 < stmts.len() {
                    emitted.push_str(&src[stmts[i].1.end..stmts[i + 1].1.start]);
                } else {
                    emitted.push_str(&src[stmts[i].1.end..]);
                }
            }
        }
        if let Err(e) = write_atomic(&disk, &emitted) {
            out.err(format!("transform: failed to write {file}: {e}"));
            return code(2, out);
        }
    } else if t.emit == "source" {
        if had_error {
            return code(1, out);
        }
        let mut emitted = String::new();
        if !stmts.is_empty() {
            emitted.push_str(&src[0..stmts[0].1.start]);
            for (i, part) in source_parts.iter().enumerate() {
                emitted.push_str(part);
                if i + 1 < stmts.len() {
                    emitted.push_str(&src[stmts[i].1.end..stmts[i + 1].1.start]);
                } else {
                    emitted.push_str(&src[stmts[i].1.end..]);
                }
            }
        }
        out.stdout.push_str(&emitted);
    }

    code(if had_error { 1 } else { 0 }, out)
}

/// Source span of a single top-level statement within the original file.
///
/// `start` and `end` are byte offsets of the trimmed statement text; the
/// `source` field holds that text for parsing.
struct StatementInfo {
    source: String,
    start: usize,
    end: usize,
}

/// Raw-parse `src` into one [`handl_language::Ast`] per top-level statement,
/// WITHOUT the desugar pass (`Runtime::parse` always desugars — ADR-0082).
///
/// Statements are split at bracket-depth zero, respecting string literals
/// and `--` comments — the same convention as the bootstrap typechecker,
/// linter, and test runner. Each returned AST's spans are offset back into
/// the original `src` so the preserving emitter can splice from it.
fn transform_parse_raw(
    lang: &handl_language::Language,
    src: &str,
) -> Result<Vec<(handl_language::Ast, StatementInfo)>, LangError> {
    use hnl_tool_parse::ParserEngine;

    let mut asts = Vec::new();
    for info in split_statements(src) {
        let tokens = hnl_tool_lex_pattern::lex(lang, &info.source)?;
        if tokens.as_slice().is_empty() {
            continue;
        }
        let req = hnl_tool_parse::ParserRequest::new(tokens);
        let mut ast = hnl_tool_parse_pratt::PrattEngine::new().parse(lang, &req)?;
        if !ast.is_empty() {
            offset_spans(&mut ast, info.start)?;
            asts.push((ast, info));
        }
    }
    Ok(asts)
}

/// Offset every span in `ast` by `delta`.
fn offset_spans(ast: &mut handl_language::Ast, delta: usize) -> Result<(), LangError> {
    for i in 0..ast.len() {
        let r = handl_language::AstRef::new(i);
        let node = ast
            .get_mut(r)
            .ok_or(handl_language::LangError::AstIndexOutOfBounds { index: i })?;
        if let Some(span) = node.span.as_mut() {
            span.start += delta;
            span.end += delta;
        }
    }
    Ok(())
}

/// Split `src` into top-level statement chunks — delegates to the shared
/// `plugin-ext-diag` implementation used by the bootstrap typechecker,
/// linter, and test runner (the Pratt parser parses one statement per
/// call).
fn split_statements(src: &str) -> Vec<StatementInfo> {
    handl_plugin_ext_diag::split_statements(src)
        .into_iter()
        .map(|c| StatementInfo {
            start: c.offset,
            end: c.offset + c.text.len(),
            source: c.text,
        })
        .collect()
}

/// Check whether `ast` can be emitted back to surface syntax without loss.
///
/// Created (`span: None`) nodes whose kind has no grammar row and no useful
/// leaf payload are internal forms (e.g. `rec`, `if__continue`) and cannot be
/// turned into valid source. Returns the list of offending kinds.
fn check_emittable(
    lang: &handl_language::Language,
    ast: &handl_language::Ast,
    root: handl_language::AstRef,
) -> Result<(), Vec<String>> {
    let grammar_kinds: std::collections::HashSet<String> = lang
        .grammar
        .iter()
        .map(|r| r.child_shape.node_kind.clone())
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    let mut stack = vec![root.index()];
    let mut bad = Vec::new();
    while let Some(i) = stack.pop() {
        if !seen.insert(i) {
            continue;
        }
        let Some(node) = ast.get(handl_language::AstRef::new(i)) else {
            continue;
        };
        if node.span.is_none() && !grammar_kinds.contains(&node.kind) && !has_leaf_payload(node) {
            bad.push(node.kind.clone());
        }
        if let Some(children) = node.payload.get("children").and_then(|c| c.as_array()) {
            for c in children {
                if let Some(idx) = c.as_u64() {
                    stack.push(idx as usize);
                }
            }
        }
    }
    if bad.is_empty() {
        Ok(())
    } else {
        bad.sort();
        bad.dedup();
        Err(bad)
    }
}

/// Whether a node has a leaf-level payload that can be rendered as text.
fn has_leaf_payload(node: &handl_language::AstNode) -> bool {
    if node.payload.get("value").is_some() || node.payload.get("name").is_some() {
        return true;
    }
    !node.payload.is_object() && !node.payload.is_null()
}

/// Render an [`handl_language::Ast`] subtree as an indented tree:
/// `kind` plus a compact payload summary (the `children` index array is
/// elided — it is rendered structurally as indentation).
fn render_ast_tree(ast: &handl_language::Ast, idx: usize, depth: usize, out: &mut String) {
    let Some(node) = ast.get(handl_language::AstRef::new(idx)) else {
        return;
    };
    let indent = "  ".repeat(depth);

    // Payload summary: everything except the `children` index array.
    let summary = match &node.payload {
        serde_json::Value::Null => String::new(),
        serde_json::Value::Object(map) => {
            let rest: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .filter(|(k, _)| k.as_str() != "children")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if rest.is_empty() {
                String::new()
            } else {
                format!(" {}", serde_json::Value::Object(rest))
            }
        }
        other => format!(" {other}"),
    };

    let span = node
        .span
        .map(|s| format!("  ; @{}..{}", s.start, s.end))
        .unwrap_or_default();
    out.push_str(&format!("{indent}{}{summary}{span}\n", node.kind));

    if let Some(children) = node.payload.get("children").and_then(|c| c.as_array()) {
        for c in children {
            if let Some(ci) = c.as_u64() {
                render_ast_tree(ast, ci as usize, depth + 1, out);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve `path` (possibly relative) against the request's `cwd`.
fn resolve(cwd: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

/// Write `content` to `path` atomically: write to a temp file in the same
/// directory, then rename over the target.
fn write_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(".hnl-fmt-tmp-{}", std::process::id()));
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn code(c: i32, out: Out) -> ExtResponse {
    ExtResponse {
        code: c,
        stdout: out.stdout,
        stderr: out.stderr,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    /// A unique temp dir per test.
    fn tmpdir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hnl-ext-transform-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn unknown_flag_is_usage_error() {
        let res = run(&["--bogus".into()], Path::new("."));
        assert_eq!(res.code, 2);
        assert!(res.stderr.contains("transform: unknown flag `--bogus`"));
    }

    #[test]
    fn missing_args_is_usage_error() {
        let res = run(&[], Path::new("."));
        assert_eq!(res.code, 2);
        assert!(res.stderr.contains("usage: hnl transform"));
    }

    #[test]
    fn missing_pass_is_usage_error() {
        let res = run(&["f.hnl".into()], Path::new("."));
        assert_eq!(res.code, 2);
        // One positional ending in .hnl + no file → treated as categorical
        // only when BOTH positionals are .hnl; a lone one is `pass`, and the
        // missing file is a usage error.
        assert!(res.stderr.contains("usage: hnl transform"));
    }

    #[test]
    fn unknown_pass_lists_known_passes() {
        let dir = tmpdir("unknown-pass");
        let file = dir.join("p.hnl");
        std::fs::write(&file, "x := 1;").unwrap();
        let res = run(
            &["bogus-pass".into(), file.to_string_lossy().into_owned()],
            &dir,
        );
        assert_eq!(res.code, 2);
        assert!(
            res.stderr.contains("transform: unknown pass `bogus-pass`"),
            "stderr: {}",
            res.stderr
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The `export!`-generated manifest symbol, called directly.
    #[test]
    fn manifest_declares_transform() {
        let raw = handl_ext_manifest();
        assert!(!raw.is_null());
        let json = unsafe { CStr::from_ptr(raw) }
            .to_string_lossy()
            .into_owned();
        unsafe { handl_ext_free(raw) };
        let manifest: ExtensionManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(manifest.name, "transform");
        let names: Vec<&str> = manifest.commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["transform"]);
    }
}
