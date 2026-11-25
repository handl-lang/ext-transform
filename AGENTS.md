# ext-transform

`hnl-ext-transform` — `hnl transform` as a dynamic `hnl` extension: the
transform runner packaged as a `libhnl_ext_transform.{so,dylib,dll}`
cdylib the dumb `hnl` host discovers at runtime.

## Layout

| path | role |
| --- | --- |
| `src/` | extension ABI exports + `transform` command wiring |
| `src/transform_categorical.rs` | two-file (`rules.hnl` × `program.hnl`) categorical runner — verbatim copy of `ext-eval`'s module |
| `xtask/` | vendored gate crate (`fmt`, `clippy`, `test`, `doc`, `gate`) |
| `workflows/gate.yaml` | per-push gate — `handl-gates` rust campaign via bupha |

## Contract

- Commands: `hnl transform <pass> <file.hnl> [--pipeline a,b,c] [--plan]
  [--trace] [--stats] [--emit tree|json|source] [--write]` and the
  two-file categorical mode `hnl transform <rules.hnl> <program.hnl>`.
- Runs a named pass (declared in `lang.transforms`) over the RAW,
  pre-desugar AST — `Runtime::parse` always desugars, so the command
  parses via `lex` + `PrattEngine` per top-level statement, then drives
  `RewriteEngine` with the kit's `.hnl` rewrite modules layered over the
  `desugar.*` native fallback table.
- Exit codes: `0` ok, `1` pass/emit errors, `2` usage or parse errors.

## Verification

```bash
cargo run --manifest-path xtask/Cargo.toml -- gate   # fmt → clippy → test → doc
```

<!-- myco:agent-awareness -->

## Agent awareness

- Session start / any work discussion: run `myco status --prompt`
  before proposing next steps — know the board first.
- User describes new work → offer to capture it (`myco todo add`,
  `issue new`, `proposal new`). Don't wait to be asked.
- Picking up a task: `myco resume <ID>` is the briefing.
- Wrapping up: `myco handoff <ID>` anything resumable; keep statuses
  honest (claimed → in-progress → blocked/done).

<!-- /myco:agent-awareness -->
