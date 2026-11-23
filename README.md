# ext-transform

`hnl-ext-transform` — `hnl transform` as a dynamic `hnl` extension.

An `ext-*` crate: the transform runner packaged as a
`libhnl_ext_transform.{so,dylib,dll}` shared library for the dumb `hnl` host.
The host discovers it via `--ext-dir`, `$HANDL_EXT_DIR`, `~/.handl/ext/`, or a
dev `target/debug/` next to the `hnl` binary.

## Commands

```text
hnl transform <pass> <file.hnl> [--pipeline a,b,c] [--plan] [--trace]
              [--stats] [--emit tree|json|source] [--write]
hnl transform <rules.hnl> <program.hnl>   — two-file categorical mode
```

Runs a named pass (declared in `lang.transforms`) over the RAW, pre-desugar
AST — `Runtime::parse` always desugars, so the command parses via
`lex` + `PrattEngine` per top-level statement, then drives `RewriteEngine`
with the kit's `.hnl` rewrite modules layered over the `desugar.*` native
fallback table. `src/transform_categorical.rs` is a verbatim copy of
`ext-eval`'s module — its `run`/`pipeline_value` surface serves the two-file
mode here.

Exit codes: `0` ok, `1` pass/emit errors, `2` usage or parse errors.

## Development

```bash
cargo run --manifest-path xtask/Cargo.toml -- gate   # fmt → clippy → test → doc
```

`xtask/` provides the gate tasks (`fmt`, `clippy`, `test`, `doc`, `check`,
`gate`); `workflows/gate.yaml` is the per-push gate — the `handl-gates` bundle's rust campaign (fmt → clippy → test → doc), run by bupha/the fleet runner via `peregrin campaign run`. The crate
builds a `cdylib` + `rlib`; `cargo test` links the `rlib` statically.

```sh
cargo build        # target/debug/libhnl_ext_transform.so
HANDL_EXT_DIR=$PWD/target/debug ../host-cli/target/debug/hnl transform desugar probe.hnl
```

Deps are git deps on `branch = "main"`; locally the workspace-root
`.cargo/config.toml` patches them to sibling checkouts (dev-only, never
committed).

## License

MIT.
