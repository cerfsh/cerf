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

The low-priority correctness sweep: argument, path, and file handling now
match the documented shell behavior, Windows console control is forwarded to
the foreground job, and `net.http` no longer has a check-then-open race.

### Changed

- Windows foreground commands and `sys.exec` children start in their own
  console process group, and a `SetConsoleCtrlHandler` forwards Ctrl-C to the
  foreground job instead of letting the default handler terminate the shell
  (#70).
- `net.http` streams files from an already-open handle instead of using
  `ServeDir`, so a path cannot be swapped between validation and use (#57).
- `fs.df` reads `statvfs` on Unix so reserved filesystem blocks are not
  reported as used (#59).

### Fixed

- The REPL incompleteness check only treats `#` as a comment at a word
  boundary, so `if true { echo a#b }` executes instead of waiting for more
  input (#72).
- Malformed invocations (`cerf -c`, unknown options, stray operands,
  `--version` with extras) report a usage error and exit 2, and operands after
  the `-c` script become positional parameters (#71).
- Windows job-object handles are closed when a job is dropped or a pipeline
  fails to start, instead of leaking one handle per command (#51).
- A leading unquoted `~` is expanded for ordinary arguments, assignment
  values, and glob patterns, and quoted tildes stay literal (#69).
- Unix `PATH` search skips non-executable files instead of letting them
  shadow a real executable later in the list (#68).
- `fs.du` reports allocated blocks rather than apparent size, counts a
  hard-linked inode once, and reports walk errors (#60).
- `net.fetch -o` writes through `/dev/null`, `/dev/stdout`, and symlinks
  instead of replacing the node with a regular file (#58).
- Huge `--timeout`, `--connect-timeout`, and `--idle-timeout` values are
  rejected with a validation error instead of panicking on `Instant`
  overflow (#56).
- `help NAME` exits 0 when `man` or `<cmd> --help` displayed help, and 127
  only when no help was found (#55).
- Function calls push a local variable scope, `local` shadows globals for the
  duration of the call, and `local` outside a function is an error (#54).
- An unterminated `${` is a bad-substitution syntax error instead of
  silently consuming the rest of the line (#53).
- `break` and `continue` require a word boundary, so commands such as
  `breakfast` and `continue.sh` parse as ordinary commands (#52).
- `source` and `eval` return 2 for syntax errors while blank/comment-only
  input remains a successful no-op (#50).
- `stat` reports symbolic links (including dangling links) by default and
  adds `-L` to dereference (#49).
- `ls` lists dangling symlinks, shows `link -> target` for `-l`, and no
  longer follows symlinked directories under `-R` (#48).
- `pushd` and `popd` refresh `$PWD` in shell state and the process
  environment (#47).
- Alias values honor double quotes and remove the quote characters, and
  keep literal backslashes inside them (#46).
- `printf` reuses its format for every argument and supports `%b` escapes,
  `%c`, floats, width, precision, and zero padding (#45).
- `type` exits 1 when any operand cannot be resolved (#44).


## [0.3.2] - 2026-11-27

The correctness follow-up: redirects, quoting, positional parameters, and job
control now behave the way the shell language describes, and the builtins no
longer panic on edge-case input.

### Changed

- The Windows installer places `cerf` in `$HOME/.local/bin` instead of
  `C:/bin/CodeTease`.

### Fixed

- Inline `#` comments are stripped outside quotes at a word boundary, so
  `echo hi # comment` no longer passes `#` and `comment` to the command (#34).
- Quoting is tracked per word segment, so partially quoted glob characters stay
  literal: `"file[1]".txt` names that file instead of matching `file1.txt`
  (#64).
- Redirections understand descriptor numbers and duplication: `2>file`,
  `2>>file`, arbitrary `N>`/`N<`, and `2>&1` now work, and `cmd >a >b` opens
  both files before leaving output on `b` (#66).
- `~` expansion no longer collapses `..` lexically, so redirects and command
  paths through symlinks resolve the way the kernel does (#65).
- `&` backgrounds `if`, `for`, `while`, `loop`, and function calls instead of
  running them synchronously; the job is registered and `[1] pid` is printed
  (#27).
- Function calls keep their arguments, prefix assignments, and redirections,
  and functions can be used as pipeline elements; arguments are available as
  the positional parameters for the duration of the call (#28).
- `exit N` uses its numeric status, a bare `exit` uses the last command's
  status, and the interactive REPL returns that status (#29).
- Positional parameters work: `$1`, `$#`, `$@`, and `$*` expand, `set --`
  populates them, and `shift` (including `shift 0`) updates them (#40).
- Prefix assignments (`IFS=: read a b`, `VAR=x builtin`) apply to in-process
  builtins and functions and are restored afterwards (#42).
- `read` splits on multi-byte `IFS` characters without panicking on a UTF-8
  boundary (#30).
- `touch` updates atime/mtime with `utimensat`, never blocks on FIFOs, and
  works on directories and read-only files (#31).
- `rm` unlinks dangling symlinks and symlinks to directories instead of
  reporting them missing or using `remove_dir` (#33).
- `declare -r name=value` creates the variable, and readonly variables can no
  longer be reassigned, unset, or changed through `export` (#35).
- Invalid environment names in `declare`, `export`, and `unset` are rejected
  with a diagnostic instead of panicking (#36).
- `export NAME=value` marks the shell variable as exported, so later
  assignments update the process environment as well (#37).
- A bare `exec` is a no-op, and exec'd programs start with default signal
  dispositions so Ctrl-C and Ctrl-Z work (#38).
- Bare `fg`/`bg` fall back to the newest live job when the remembered current
  job has exited (#39).
- `sys.command` runs externals through the normal spawn path, giving them
  signal defaults, a process group, and terminal hand-off (#41).
- Builtins report stdout write errors (including `EPIPE`) instead of panicking
  the shell, covering `io.echo`, `io.printf`, `fs.du`, `fs.df`, `fs.stat`, and
  the pager builtins (#43).
- The shell starts with non-UTF-8 environment variables and arguments by
  converting them lossily instead of panicking (#32).
- `--internal-builtin` only removes a state snapshot that carries Cerf's magic
  header, so an inherited `CERF_INTERNAL_STATE` can no longer delete an
  unrelated file (#67).
- The inline-comment regression test uses a temporary file instead of
  `/etc/passwd`, so the test suite passes on Windows.

## [0.3.1] - 2026-11-25

The correctness release: variable expansion, redirects, job control, and globbing
now behave the way the shell language describes.

### Changed

- CI skips runs for documentation-only and release-only changes.

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
- Redirected builtins run in-process on Windows by swapping the standard handles and
  restoring them afterwards, so `echo hi > out.txt` keeps shell state instead of
  forking a child.
- Glob results normalize Windows path separators before applying the dotfile rule, so
  patterns like `.*` no longer match `.` or `..` when the glob crate reports `.\`
  paths.
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

[Unreleased]: https://github.com/cerfsh/cerf/compare/v0.3.2...HEAD
[0.3.2]: https://github.com/cerfsh/cerf/releases/tag/v0.3.2
[0.3.1]: https://github.com/cerfsh/cerf/releases/tag/v0.3.1
[0.3.0]: https://github.com/cerfsh/cerf/releases/tag/v0.3.0
