# AGENTS.md

Cerf is a cross-platform shell written in Rust. It is a single binary crate (no library
target): `src/main.rs` is the entrypoint and modules live under `src/parser`,
`src/engine`, and `src/builtins`.

## Commands

- Toolchain is pinned to **Rust 1.100.0**, edition 2024. CI uses exactly that version.
- CI order (run this before handing off): `cargo fmt --all -- --check` →
  `cargo clippy --all-targets --all-features -- -D warnings` → `cargo build` →
  `cargo test`. Clippy warnings fail CI.
- Run one test: `cargo test <name>`; integration test only:
  `cargo test --test engine_cli <name>`.
- Unit tests are inline `#[cfg(test)]` modules in `src/`; end-to-end tests in
  `tests/engine_cli.rs` spawn the binary through `CARGO_BIN_EXE_cerf` (no manual build
  needed). Some of those tests are Unix-only and assume `/etc/passwd`, `/bin/sh`, and a
  writable `TMPDIR`.
- CLI entrypoints: interactive REPL, `cerf -c '<script>'`, `cerf --version`, and the
  internal child mode `cerf --internal-builtin <name>` (do not call this directly).

## Adding or changing a builtin

- Builtin names are namespaced `<type>.<action>` (`dir.cd`, `io.echo`, `net.fetch`).
  Define a `COMMAND_INFO` const in `src/builtins/<area>/...` and add it to
  `BUILTINS` in `src/builtins/registry.rs`; otherwise `sys.type` reports it as missing.
- POSIX names (`cd`, `echo`, `ls`, …) are **default aliases**, not builtins. Add them to
  `init_default_aliases()` in `src/engine/state.rs`. Aliases cannot override builtins;
  `alias.set` rejects collisions.
- A builtin registered in `registry.rs` but whose runner takes `&mut ShellState` runs
  in-process. If it needs correct pipes/redirects/signals, it must be process-isolated:
  add it to `registry::is_process_isolated()` (and to the list in
  `tests/engine_cli.rs::all_network_commands_are_registered` if it is `net.*`). Isolated
  builtins run in a child via `--internal-builtin`, so mutations to shell state are lost.
- Background builtins launch a child and receive launch-time state through the
  `CERF_INTERNAL_STATE` snapshot; keep that path working when touching serialization in
  `src/engine/state.rs`.
- The generic in-process builtin path does not attach stdin/stdout redirects (it warns
  for a few names and silently ignores them for others). Prefer process isolation for
  any new builtin that must honor pipes or redirections.

## Shell language quirks

- Control flow uses braces, not POSIX `then`/`fi`:
  `if c { … } elif c { … } else { … }`, `for x in … { … }`, `while c { … }`,
  `loop { … }`, `func name { … }`.
- A trailing `,` is an explicit line continuation. The parser also continues input on
  unbalanced braces or a trailing `|`, `&&`, `||` (`parser::is_incomplete`).
- `-c` mode returns `0` for empty/comment-only input, `2` for syntax errors, and the
  pipeline's last command status for successful parses.
- Cross-platform code is required: job control and process spawning have separate
  `#[cfg(unix)]` / `#[cfg(windows)]` implementations, and CI builds on Linux, macOS, and
  Windows. Verify platform-specific changes in both paths where possible.

## Releases

- Version lives in both `Cargo.toml` and `release.toml` `[metadata].version`; bump them
  together. A `v<version>` tag triggers the release workflow, and
  `.github/workflows/release-plan.nu` fails if the tag does not match
  `release.toml`.
- Packaging/publishing is driven by the Nushell scripts and templates in
  `.github/workflows/`; normal code changes rarely need those files.
- `INSTALLATION.md` is generated from `.github/workflows/templates/Registry.template.md`
  during release. Edit the template, not the generated file.
