# Contributing

Thanks for helping. This is a small Rust project; the notes below keep changes easy to review.

Agents working on this repository (Claude Code, opencode, Codex) also follow [AGENTS.md](AGENTS.md); it is not an agent template.

By taking part you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md). Security problems go to
[SECURITY.md](SECURITY.md), not to a public issue. Questions: [carlos@cargdev.io](mailto:carlos@cargdev.io).

## Ways to contribute

- Report a bug or propose an idea (see [Reporting problems](#reporting-problems)).
- Improve an agent template, a slash command or the docs.
- Add a test for behavior that has none.
- Pick up an issue and say so in it, so two people do not do the same work.

## Setup

Requirements: Rust 1.85 or newer (edition 2024), and opencode v2 (developed against 2.0.32) or Claude Code
if you want to try it end to end. macOS or Linux.

```sh
git clone https://git.cargdev.io/CarGDev/codetyper.a2a.git
cd codetyper.a2a
cargo build
cargo test                  # unit tests + the hub end-to-end test (tests/e2e.rs)
bun test tests/panel_logic.test.ts   # the opencode panels' logic (needs bun)
cargo run -- help
cargo run -- e2e-test       # the hub end-to-end test on its own
cargo run -- eval           # deterministic dispatcher scenarios
```

a2a itself needs tmux; the tests do not (they set `A2A_NO_TMUX_CHECK=1`), so `cargo test` runs anywhere.

## Layout

```
src/main.rs            command dispatch (also runs as `oc` when invoked under that name)
src/cli.rs             `help`, `init` (agents, commands, plugin, MCP entries, shell alias; `init_opencode` / `init_claude`)
src/targets/           install targets: mod.rs (`TARGETS` table, detection, flags), convert.rs (opencode markdown to
                       Claude Code), claude.rs (installer with its content-hash manifest, MCP registration, the injectable `claude plugin` runner),
                       settings.rs (status line and hooks in `settings.json`, incl. SessionStart ensure),
                       picker.rs (the `init` screen)
src/hub/               MCP server (server.rs), SQLite state (db.rs), project identity, e2e test
src/dispatcher/        wakes sessions through the opencode v2 API
src/launcher/oc.rs     the `oc` launcher
src/launcher/statusline.rs  `oc statusline`, the Claude Code footer line and its subagent hooks
src/opencode.rs        the opencode API calls (sessions, agents, tabs file)
src/sys.rs             process helpers (`opencode api`, tmux, pid checks)
src/model_guard.rs     model policy checks
src/ollama.rs          Ollama discovery (`init --ollama-url`) and the opencode provider entries
src/models.rs          `models` screen, per-agent model overrides (also written into the agent files)
src/stats_view.rs      `oc stats` and `oc stats --model` tables (sorted, coloured, blank when unknown)
src/launcher/panel.rs  `oc status --json` and `oc answer`, the data the opencode panels read
agents/ commands/ plugins/ a2a/   templates and docs that `init` installs (embedded at build time)
plugins/a2a/           tabs plugin (tui.ts) and panels (panels.tsx, panel-logic.ts)
plugins/claude-a2a/    Claude Code pane mod `a2a-pane` (hooks/register.tsx, hooks/model.ts, types/index.d.ts, *.test.ts(x));
                       embedded by `src/cli.rs` (`CLAUDE_MOD`) and written by `src/targets/claude.rs` (`install_pane`), never to opencode
Formula/               Homebrew formula
```

## The Claude Code pane mod

Check it from the repo, with a throwaway config dir so your real `~/.claude` is not touched:

```
export CLAUDE_CONFIG_DIR=$(mktemp -d)
claude plugin validate plugins/claude-a2a    # manifest, hooks module and the $.state contract in types/
claude plugin test plugins/claude-a2a        # the *.test.ts(x) files, against the engine's own host
```

A module may pass `$` only to functions declared at the top level of the file (validate enforces it). Test files are
not shipped by `init`. Drawing in a real terminal cannot be tested here: try it with
`claude --plugin-dir plugins/claude-a2a` (set the `exe` option to your binary) and `/a2a`.

## Rules of the road

- **Work in small chunks: plan, design, task, implementation.** One change is one work unit of at most
  **400 changed lines** (additions plus deletions, generated files excluded) with its tests and docs. A
  migration that cannot be split without breaking the build is the only exception; say so in the PR. Never
  shrink a diff by deleting comments, blank lines or tests: split the work instead. For anything bigger
  than a small fix, write the plan, design and task list first (in the issue or the PR description).

- **Base opencode behavior on the v2 documentation** (<https://opencode.ai/v2/docs>) or on the API of
  your running server (`opencode api get /openapi.json`), not on memory. Note the doc page you used in
  the PR.
- **Tests must never touch your real environment.** Use temp dirs and the overrides the code supports:
  `A2A_DB`, `A2A_AGENT_DIR`, `A2A_POLICY`, `A2A_DISPATCHER_PID`, `OPENCODE_CONFIG_DIR`, `OPENCODE_BIN`
  (point it at a stub script), and `A2A_NO_DISPATCHER=1` so a test hub does not start a dispatcher.
  **Never run tmux commands against your real server from a test or script**: `$TMUX` wins over
  `TMUX_TMPDIR`, so clear it (`env -u TMUX -u TMUX_PANE`) and use a private socket (`tmux -L <name>`).
  Prefer unit-testing how a tmux command is built over running a live tmux.
- **Add a test with the change.** Hub behavior goes into `src/hub/e2etest.rs`; `init` behavior into
  the tests in `src/cli.rs` (use a temp `--dir`, or `--claude-dir` for Claude Code, and pass `--opencode` / `--claude`
  explicitly so a test never opens the picker or touches your real config); dispatcher routing can be checked with a stub
  `opencode` script (see how `OPENCODE_BIN` is used).
- **Keep `init` non-destructive**: existing files and MCP entries are skipped unless `--force`, and
  `opencode.jsonc` is backed up before it is rewritten.
- **Match the surrounding code.** Small functions, `Result<_, String>` errors with a clear message, no
  new dependency without a reason. Guard clauses and early returns instead of nested `if`/`else`; data
  tables instead of long `if`/`match` chains. Run `cargo build` with no warnings.

## Agent templates (`agents/`)

Templates ship to everyone, so keep them generic: no company, project or personal paths and no
provider-specific models. Each agent needs `"a2a_*": true` under `tools:`, an `## A2A` section with
the agent's name, and the **Report back (mandatory)** block (finish with `a2a_update_task`; the hub
returns the result to the session the task came from). Copy a small existing agent such as
`summarizer.md` as the starting point. Every template needs a `model:` line (`github-copilot/<id>`): the first choice of its role in
`tier_for()` in `src/models.rs` (cheap, balanced, deep or other-family), and the same id in
`model-policy.json` `agentDefaults`; a test fails if they drift apart or the policy would not allow it.
Keep agents **least-privilege**: sessions run with the user's
opencode permissions and no sandbox, so give each agent only the `tools:` it needs (read-only agents
set `write: false` and `edit: false`; use `permission:` to limit where it may write). In opencode v2 every
MCP tool is allowed by default, so deny the servers an agent does not need (for example
`"playwright_*": false`; only agents that really drive a browser leave Playwright on).

## Adding an install target

`init` knows its tools from the `TARGETS` table in `src/targets/mod.rs`; Claude Code and opencode are installable,
the others are listed as "coming soon". To make one installable:

1. Set `ready: true` on its entry in `TARGETS` (name, scope, the binaries and config dirs that detect it;
   `Probe` is injectable, so tests use a fake one).
2. Add its flag to `Flags::from_args` and the help text in `src/cli.rs`.
3. Write the installer next to `src/targets/claude.rs`. Convert from the embedded `agents/` and `commands/`
   (`src/targets/convert.rs` has the opencode to Claude Code mapping): one source of truth, no copies. Keep it
   non-destructive like the others: skip existing files unless `--force`, back up a config file before
   rewriting it, never overwrite an existing MCP entry.
4. Wire it into `init` in `src/cli.rs`, and add its row to the picker (no change to `picker.rs` is needed when
   the table is the only source).
5. Test it in a temp config dir (`--claude-dir`-style flag or an env override), never your real one: unit tests for
   the converter, an `init` test like `init_sets_up_claude_code_in_its_own_dir_and_leaves_opencode_alone`, and
   the picker state is a plain struct, so its keys are tested without a terminal.
6. Update the README (the target list and what the installer writes), `help`, and `CHANGELOG.md`.

## Changing the hub tools or protocol

Update `tools()` and the handler in `src/hub/server.rs`, the table in `a2a/PROTOCOL.md`, the e2e test
(and the expected tool count), and add a `CHANGELOG.md` entry.

## Commits and pull requests

- Use [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`, `docs:`,
  `test:`, `refactor:`), one logical change per commit, with its tests and docs.
- Keep pull requests focused (a few hundred changed lines at most; split bigger work) and describe
  what changed, why, and how you tested it.
- Update `CHANGELOG.md` under `[Unreleased]` for anything user-visible.
- Checklist: `cargo test` passes, no new warnings, docs updated (README, `oc help`, `a2a/PROTOCOL.md` when
  they change), no secrets or personal paths.
- **License:** the project is under the [Apache License 2.0](LICENSE). By sending a contribution you
  agree that it is licensed under the same terms (section 5 of the license).

## Releasing (maintainer)

Bump the version in `Cargo.toml`, `Cargo.lock` and `README.md`, add a `## [x.y.z]` section to
`CHANGELOG.md`, commit, then run `./.update.sh x.y.z`. It checks all of that and a clean working tree,
pushes `master`, creates the `vx.y.z` tag and pushes it; the release workflow updates the Homebrew tap and
keeps the previous version as a versioned formula.

## Reporting problems

Open an issue with your OS, `opencode --version`, `codetypera2a help | head -1`, what you ran, what you
expected and what happened. For dispatcher problems include the last lines of
`~/.config/opencode/a2a/hub.db.dispatcher.log` and the output of `oc status`.

## Adding an agent runtime

The dispatcher talks to agents only through the `Backend` trait (`src/dispatcher/backend.rs`): find or create a
session, say which sessions are running a turn, send a prompt, report a failure. `OpencodeBackend` and
`ClaudeBackend` (`claude_backend.rs`) are the two implementations; a new one is a line in `build_one` and in `NAMES`,
a value for `sessions.backend`, and tests with a fake binary like `tests/claude_dispatcher.rs` (it also runs a
throwaway `tmux -L <name>` server for the agent window; `tests/claude_init.rs` runs `init` against a fake `claude`
on PATH, so never against your real `~/.claude`). Usage stays with
whoever already counts it: a backend whose turns write transcripts that `oc stats` ingests must return `None` from
`usage`, or every turn is counted twice.

## Changing the database schema

The hub database (`src/hub/db.rs`) is shared by the hub, the dispatcher and `oc`, and several processes may open it at
once. A schema change is a new step in `migrate()`: bump `SCHEMA_VERSION`, copy the file to `<db>.bak-vN` first (best
effort), run the step inside the `BEGIN IMMEDIATE` transaction after re-reading `user_version`, and add the same
change to the schema constant so fresh databases match. Keep new columns nullable so an older binary can still write.
Update `a2a_hub.dbml` and `db.dbml`, and test a migration from a hand-built old file, a fresh file, a second open
(no-op) and a concurrent open. `migrate()` upgrades step by step (`upgrade_from`), so a v2 file reaches the
current version in one open; test the newest step from a file downgraded to the previous version, and that a v1 and a
v2 file still reach it. `events.tokens_cache_read`/`tokens_cache_write` (v6) also delete the derived `claude-code` rows and their `ingest_offsets` in the same step so the next ingest rebuilds them (`<db>.bak-v5`). `sessions.backend` (v5, NULL = opencode) is the model for a nullable column that needs no backfill: the step is one
`ALTER TABLE ... ADD COLUMN`, `upgrade_from` backs the file up as `<db>.bak-v4` and each older step still runs first.
A column or table that another tool's data fills (`events.source`/`ref`, `ingest_offsets`)
needs a unique key per source so a rerun of its reader inserts nothing.
