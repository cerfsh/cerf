# Changelog

All notable changes to Cerf are documented in this file.

> **Baseline: `v0.3.0`.**
> This changelog starts at **v0.3.0** (2026-11-24), which is the earliest release it
> documents in full. Releases before `v0.3.0` are summarized under
> [Pre-baseline history](#pre-baseline-history) for context only. Nothing is
> backfilled to per-commit detail, and entries are not tracked before the baseline.
> New work goes under `Unreleased` until the next tag.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Variables are expanded when a command runs instead of when it is parsed, so loop
  and function bodies, `if`/`while` conditions, and sourced scripts observe current
  values rather than the values captured at parse time.
- Single quotes and a backslash now suppress expansion, and each word is expanded
  exactly once (values containing `$` are no longer expanded recursively).
- Empty quoted words (`""` and `''`) parse as empty arguments instead of syntax
  errors.
- Redirects are applied for in-process builtins (`echo hi > out.txt`,
  `cat < in.txt`, `read x < in.txt`, `history > file`), which previously dropped
  them silently and used the terminal instead.
- `fg` marks a resumed job as running before waiting, so the first `fg` of a
  Ctrl-Z'd job waits for it instead of reporting `Stopped` and leaving it
  running in the background.
- Glob expansion no longer matches dotfiles: `*` skips hidden names while an
  explicit leading dot (`.env`, `.*`) still matches them.
- `fs.cat` copies standard input when no file operand is given, and treats `-`
  as standard input instead of trying to open a file named `-`.
- A background pipeline whose first element is skipped (assignment-only) records
  the first spawned child as its process group instead of pgid 0, so `kill %n`
  no longer signals the shell's own group. `kill` also refuses to signal pgid 0.
- `kill` understands the full signal table (names and numbers, including `-0`),
  lists signals with `-l`, and reports unknown signal names and job specs instead
  of silently sending `SIGTERM` or succeeding.
- `[ ... ]` works again: `[` is its own builtin that strips the closing `]`, and
  one- to four-argument `test` forms follow the POSIX argument-count rules.
- `wait` blocks until its jobs finish and returns their status. A bare numeric
  operand is now treated as a PID, while `%n` selects a job.
- `break` and `continue` return from the command list that contains them, so loops
  now stop or advance instead of swallowing them and running forever.
- Redirected control-flow blocks are executed in place with their redirects applied,
  instead of being re-serialized into a subprocess where commands were merged and
  their own redirects and assignments were lost.
- `fs.cp` refuses to copy a file onto itself (directly, through a hard link, or
  through a symlink) instead of truncating it to zero bytes and reporting success.
- `fs.rm` refuses `.`, `..`, and the filesystem root before deleting anything.
- `cerf -c` initializes signal handlers and ignores `SIGTTOU` while a child claims
  the terminal, so external commands and pipelines no longer hang when stdin is a
  controlling terminal.

### Security

- Variable contents are treated as data: they are no longer re-parsed as shell
  syntax, which closes a command-injection path through environment variables,
  `read`, and command output.

## [0.3.0] - 2026-11-24

The `net.*` release: a new network command family, process isolation for builtins,
and a properly status-reporting `-c` mode.

### Added

- **`net.*` builtin family** (9 commands), all process-isolated so they receive
  normal pipes, redirects, and terminal signals:
  - `net.fetch` — HTTP/HTTPS GET to stdout or atomically to a file.
  - `net.wfp` — wait until a TCP port accepts connections.
  - `net.http` — static file server with directory listings.
  - `net.listen` — TCP/UDP listener wired to stdin/stdout.
  - `net.socket` — TCP/UDP client wired to stdin/stdout.
  - `net.tunnel` — TCP/UDP port forwarder.
  - `net.whoami` — public, primary, and per-interface addresses.
  - `net.ip` — public IP address, with `-4`/`-6` family selection.
  - `net.dns` — DNS record resolver (A, AAAA, CNAME, MX, NS, TXT, PTR).
- **Process isolation for builtins.** `registry::is_process_isolated()` and
  `registry::run_isolated()` run selected builtins in a child process through the
  internal `cerf --internal-builtin <name>` mode, so they honor pipes and redirects.
  `builtin` and `command` forward to the isolated path when wrapping such a builtin.
- **Launch-time state snapshots** (`CERF_INTERNAL_STATE`) so background builtin
  children inherit the shell's variables and `set` options instead of an empty state.
- **End-to-end test suite** `tests/engine_cli.rs` (12 tests) covering script mode,
  isolated-builtin redirects, pipeline status, background job launching, and network
  command registration.
- **Prompt expansion.** `PS1`/`PS2` are now honored with `\u`, `\h`, `\w`, and `\$`
  escapes; the previously hardcoded `cf <cwd> > ` prompt is gone.
- **Script mode exit statuses.** `cerf -c` returns `0` for empty/comment-only input,
  `2` for syntax errors, and the pipeline's last command status otherwise.
- `parser::is_blank_or_comment_only()` and leading-comment skipping, so scripts may
  start with comments and blank lines.
- `AGENTS.md` contributor guide and a README section documenting the network
  commands and their safety caveats.
- New dependencies: `axum`, `tokio`, `reqwest`, `tower-http`, `url`,
  `hickory-resolver`; `tempfile` promoted from dev-only to a runtime dependency.

### Changed

- **MSRV raised to Rust `1.100.0`** (from `1.93.1`), pinned in `Cargo.toml`,
  `release.toml`, and CI.
- `cerf --version` now prints a `v`-prefixed version (`v0.3.0`).
- CI now treats `cargo fmt --check` and `cargo clippy -D warnings` as blocking;
  the previous `continue-on-error` escapes were removed.
- Release pipeline publishes real releases instead of drafts, marks stable tags
  `--latest`, always includes a Cargo install section, and builds the `aarch64`
  targets on native ARM runners.
- Registry docs templates accept APT/RPM/APK distro and codename substitutions.
- Dependency bumps: `dirs` 6 → 7, `rustyline` 17 → 19, `sysinfo` 0.38 → 0.40,
  `jwalk` 0.8 → 0.9, `nix` 0.31 → 0.32.
- `Job::pgid` is now Unix-only and `ProcessInfo` no longer carries a `name`.
- Added `.cargo/config.toml` to trim dev-profile debug info.

### Fixed

- `command not found` when any pipeline was used: builtins are now resolved to the
  shell binary rather than searched on `PATH`.
- `sys.clear` now clears scrollback, not just the visible screen.
- Background builtins no longer take over job control or the terminal, preserve
  explicit prefix assignments, and the shell waits only for foreground processes.
- Background jobs launched from an internal-builtin child get their own process
  group, so `kill %n`, `bg`, and `fg` resolve to a valid group.
- Redirects are honored for isolated builtins invoked through `builtin`/`command`.
- Windows background jobs are reaped and reported from the shell loop instead of a
  detached external-printer path.
- `net.fetch` timeouts run inside the network runtime, file output is atomic, broken
  pipes are treated as success, and empty `--output` values are rejected.
- `net.http` serves directory requests with `200` and returns `500` on listing
  failures instead of dropping the task.
- `net.listen` and `net.tunnel` back off and continue on transient `accept`/receive
  errors rather than exiting.
- `fs.df` handles the `Result` returned by disk enumeration instead of assuming
  success.

### Security

- `net.http` canonicalizes every request path and refuses symlink escapes, including
  the implicit `index.html` selected for directory requests.
- Listener builtins (`net.http`, `net.listen`, `net.tunnel`) bind `127.0.0.1` by
  default; `net.http` warns when serving on a non-loopback address.
- `net.fetch` accepts only `http`/`https` and rejects non-2xx responses.

### Removed

- The short-lived prompt manager module; prompt handling is back in `main.rs` with
  the escape expansion listed above.
- Unused `CommandNode` mutation helpers (`name_mut`, `args_mut`); `name()` is now
  test-only.

## Pre-baseline history

Summarized for context. These releases predate the changelog baseline and are not
tracked in detail.

- **0.2.0** (2026-03-19) — Filesystem builtins (`fs.*`: `cat`, `cp`, `mv`, `rm`,
  `mkdir`, `touch`, `stat`, `du`, `df`, `less`/`more`, and `ls` with `-l`, `-a`/`-A`,
  `-F`), `--version`/`CERF_VERSION`, and crates.io publishing via OIDC.
- **0.1.1** (2026-03-15) — Release/registry fixes; Cloudsmith repository moved to
  `codetease/tools`.
- **0.1.0** (2026-03-15) — Initial release: pipelines, redirections, logic operators,
  brace-style control flow, functions, variables, aliases, globbing, history,
  job control, and Windows/Unix support.

[Unreleased]: https://github.com/cerfsh/cerf/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/cerfsh/cerf/releases/tag/v0.3.0
