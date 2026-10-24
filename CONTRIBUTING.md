# Contributing

Thanks for helping. This is a small Rust project; the notes below keep changes easy to review.

By taking part you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md). Security problems go to
[SECURITY.md](SECURITY.md), not to a public issue. Questions: [carlos@cargdev.io](mailto:carlos@cargdev.io).

## Ways to contribute

- Report a bug or propose an idea (see [Reporting problems](#reporting-problems)).
- Improve an agent template, a slash command or the docs.
- Add a test for behavior that has none.
- Pick up an issue and say so in it, so two people do not do the same work.

## Setup

Requirements: Rust 1.85 or newer (edition 2024), and opencode v2 (developed against 2.0.32) if you
want to try it end to end. macOS or Linux.

```sh
git clone https://git.cargdev.io/CarGDev/codetyper.a2a.git
cd codetyper.a2a
cargo build
cargo test                  # unit tests + the hub end-to-end test (tests/e2e.rs)
bun test tests/panel_logic.test.ts   # the opencode panels' logic (needs bun)
cargo run -- help
```

a2a itself needs tmux; the tests do not (they set `A2A_NO_TMUX_CHECK=1`), so `cargo test` runs anywhere.

## Layout

```
src/main.rs            command dispatch (also runs as `oc` when invoked under that name)
src/cli.rs             `help`, `init` (agents, commands, plugin, MCP entries, shell alias)
src/hub/               MCP server (server.rs), SQLite state (db.rs), project identity, e2e test
src/dispatcher/        wakes sessions through the opencode v2 API
src/launcher/oc.rs     the `oc` launcher
src/opencode.rs        the opencode API calls (sessions, agents, tabs file)
src/sys.rs             process helpers (`opencode api`, tmux, pid checks)
src/model_guard.rs     model policy checks
src/models.rs          `models` screen, per-agent model overrides (also written into the agent files)
src/stats_view.rs      `oc stats` table (sorted, coloured, blank when unknown)
src/launcher/panel.rs  `oc status --json` and `oc answer`, the data the opencode panels read
agents/ commands/ plugins/ a2a/   templates and docs that `init` installs (embedded at build time)
plugins/a2a/           tabs plugin (tui.ts) and panels (panels.tsx, panel-logic.ts)
Formula/               Homebrew formula
```

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
  the tests in `src/cli.rs` (use a temp `--dir`); dispatcher routing can be checked with a stub
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
