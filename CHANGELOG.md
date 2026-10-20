# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project aims to follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [1.1.1] - 2026-10-19

### Added

- **GitHub Actions.** `ci.yml` runs `cargo test` and `codetypera2a eval` on Linux and macOS for every
  push to `master` and every pull request. `release.yml` runs when a `v*` tag is pushed: it checks the
  tag against the `Cargo.toml` version, creates the GitHub release, computes the tarball sha256 and
  updates `url` and `sha256` in the Homebrew tap. It needs the repository secret `TAP_GITHUB_TOKEN`.

### Fixed

- **A stale binary path no longer breaks the `a2a` MCP server.** After moving from `cargo install` to
  Homebrew (or any reinstall that moves the binary), `opencode.jsonc` and `a2a/codetypera2a.path`
  kept pointing at the old path and opencode failed with `NotFound: ChildProcess.spawn`. The tool now
  repairs itself:
  - `codetypera2a init` replaces only the `command` of an existing `a2a` entry when its path no
    longer exists (other keys are kept, no `--force` needed, so your edited agents are not touched).
  - `oc` (including the `opencode` alias) does the same check before starting, backs up
    `opencode.jsonc` to `opencode.jsonc.bak-a2a`, rewrites `codetypera2a.path` when it is dead, and
    prints one `oc: ...` line when it changed something. `oc help` and `oc --version` stay
    side-effect free.
  - A config with comments is not rewritten: the corrected command is printed instead.
  - A path that still exists, or a plain command name on `PATH`, is left alone.

## [1.1.0] - 2026-10-19

### Added

- **`codetypera2a eval`**: replays a fixed set of deterministic scenarios against the real hub and
  dispatcher with a fake opencode (no model, no network) and prints `PASS`/`FAIL`/`SKIP` per
  scenario; exits non-zero on any failure. Scenarios: routing correctness, hop cap, no duplicate
  nudges with two dispatchers on one database, and a reviewer task with a seeded bug routed to
  `code-reviewer` (plus a static check that the reviewer template is read-only). Grading real
  model output is a `SKIP` until a live mode exists. `--list` prints the scenario names and a
  name argument runs one.
- **Run tracing**: an `events` table records who sent each task, when it was claimed and delivered,
  state changes, hop-cap, no-session and stall events. `oc trace <task-id|prefix>` prints the timeline
  with offsets and total duration; `oc stats` summarizes tasks, deliveries and average duration per
  agent. Tokens and cost are stored as optional fields and shown as `n/a` because opencode usage is not
  read yet. Recording never breaks a dispatcher tick.
- **`codetypera2a --version`** (also `-V` and `version`), and `oc --version`.
- **Homebrew tap** [CarGDev/homebrew-codetypera2a](https://github.com/CarGDev/homebrew-codetypera2a):
  `brew install CarGDev/codetypera2a/codetypera2a` builds the tagged `v1.1.0` release from source with
  no `cargo` or clone needed. The formula test runs `--version`, `eval routing` and `oc help`.
- Crate metadata (`description`, `license`, `repository`, `homepage`) in `Cargo.toml`.

### Changed

- README: the Homebrew install section now uses the tap instead of the git.cargdev.io steps.

## [1.0.0] - 2026-10-19

First stable release: the Rust port of the Node.js prototype, with the hub, dispatcher, tabs plugin,
`oc` launcher, agent templates and model policy considered stable. The design follows the harness
engineering practices described in Google Research's
[harness2](https://github.com/google-research/harness2) and the
[awesome-harness-engineering](https://github.com/ai-boost/awesome-harness-engineering) list; the
README ("Harness engineering") maps each practice to the part of this project that implements it.

### Added

- **Harness engineering documentation**: README section mapping context, tools, guardrails,
  verification, memory, orchestration and observability to the matching features, and the
  harness2 patterns (propose, probe, compose; evidence-backed lessons) to the orchestrator rules.

- **A2A hub** (`codetypera2a hub`): MCP server over stdio with the A2A task lifecycle, messages with
  parts, artifacts, `contextId` grouping and a shared SQLite database. Tools: `register_agent`,
  `list_agents`, `send_message`, `inbox`, `get_task`, `list_tasks`, `update_task`, `cancel_task`.
- **Dispatcher** (`codetypera2a dispatcher`): wakes agents through the opencode v2 server API. Each
  agent gets its own session per project, created once and reused; results go back to the session
  the task was sent from (`from_session`, detected by the hub). Hop cap, stall retry, `no_session`
  reporting, single instance via a pidfile.
- **Tabs plugin** (`plugins/a2a/tui.ts`): opens each agent's session as an opencode session tab.
- **`oc` launcher**: runs opencode with the dispatcher; `oc status`, `watch`, `dispatch`, `stop`,
  `agent`, `continue`. Inside tmux it opens a detached `a2a-status` window that follows the
  dispatcher live. Also available as an `opencode` alias (`init --shell`).
- **`init`**: installs the agent templates, slash commands (`/plan-new`, `/plan-status`,
  `/plan-continue`), `PROTOCOL.md`, the model policy and the tabs plugin into the opencode config
  directory, registers the MCP servers `a2a`, `codegraph`, `context7`, `engram` and `playwright`
  (existing entries are never overwritten, a backup is kept) and sets `orchestrator` as the default
  agent. Options: `--force`, `--dir`, `--no-link`, `--shell`, `--tools`.
- **Agent templates** (`agents/`): 21 generic agents, including `orchestrator`, `plan`, `brain` and
  `architect`. Large work (more than ~400 changed lines) always goes brain -> plan -> architect ->
  plan (tasks) -> implementers; `architect` turns the spec into an architecture and work units of at
  most ~400 lines.
  Every agent must report back through `a2a_update_task` to the session a task came from.
- Orchestrator working patterns (ideas from Google Research's harness2): **independent verification**
  of every code change by `code-reviewer` and `test-runner`, **parallel probes** for uncertain work
  (2-3 different agents, read-only, compare artifacts), and **evidence-backed lessons** saved to Engram
  (`a2a/lessons/<project>`) that `brain` reads when routing.
- `a2a_list_agents` returns every agent opencode knows (built-in, user, project and plugin agents),
  and any of them can be an assignee.
- `check-models`: lints models against `model-policy.json` (accepts both the `agent` and `agents`
  config keys).
- Hub end-to-end test (`codetypera2a e2e-test`, also run by `cargo test`).
- Homebrew formula (`Formula/codetypera2a.rb`, builds from source with `--HEAD`).
- Documentation: `README.md`, `a2a/PROTOCOL.md`, `CONTRIBUTING.md`.

### Changed

- The hub starts the dispatcher itself when opencode loads it and restarts it every 15 seconds if it
  dies, so `oc` is optional.
- `init` registers the path the user invoked (for example `/opt/homebrew/bin/codetypera2a`) instead of
  the resolved target, so the MCP entry survives `brew upgrade`.
- Starting plain `opencode` now opens the dispatcher status tab in tmux: the tabs plugin runs the new
  `oc ensure` (dispatcher + status tab, no opencode) on every TUI start, using the binary path `init`
  records in `a2a/codetypera2a.path`. The `opencode` alias is optional.
- **Models by default.** Every agent template carries the model of its role (`claude-haiku-5.5` for
  routing, commands and lookups, `claude-sonnet-5.5` for everyday coding, `claude-opus-5.5` for the
  architect, `gpt-6.1-sol` for the reviewers). `model-policy.json` is rewritten to match: the old Opus deny
  and GPT blocks are gone, 19 Copilot models are allowed in three tiers (cheap, sonnet, deep), the stale
  agent names are dropped, and only the cheap agents are "read-only". Tests keep templates, tiers and
  policy in sync. `init` reports template models your opencode does not have; the models screen shows
  each agent's real current model and flags missing ones. Checked against a real opencode: all 21 resolve.
- **`codetypera2a models`** (and a screen at the end of `init`, `--no-models` to skip): a terminal UI to set
  each agent's model from the models opencode actually has, with role-based recommendations (cheap for
  routing and commands, balanced for everyday coding, deep for design, another model family for
  reviewers) and published Copilot prices. Saved as `agents.<name>.model` in `opencode.jsonc`; also
  `--recommended` and `--show`. The dispatcher now passes the agent's model when it opens a session
  (opencode sessions keep their own model and ignored the agent's). Warns when a pick violates
  `model-policy.json`.
- **Every nudge is delivered exactly once.** Two dispatchers could run at the same moment (the pidfile was
  created empty and filled afterwards, so a second dispatcher read it as stale and deleted it), and each
  one sent the same task to the agent, then crashed the tick on `UNIQUE constraint failed: nudges.key`.
  The pidfile now appears atomically with its pid inside, and a nudge is claimed in the database
  (`INSERT OR IGNORE`) before anything is sent, released again if nothing was delivered, so even two
  dispatchers cannot both deliver it. Covered by `tests/dispatcher_once.rs`.
- Agents no longer open Playwright unless they need a browser: every template except `playwright-tester`,
  `pr-reviewer`, `pr-commenter` and `code-reviewer` denies `"playwright_*"` (opencode v2 allows all MCP tools
  by default). `code-reviewer` keeps it but may use it only for web development (UI code), view-only,
  against a dev server it was given; never for backend, CLI, library or config changes.
- `code-reviewer`, `security-scanner` and `git-historian` get a read-only shell (`git diff`, `show`, `log`,
  `status`, `blame`, `ls-files`, `rev-parse`, `ls`; no pipes, redirects or chaining) through a
  `permission: bash` block, since `bash: false` also denied `git diff`. A test keeps the deny-all-first
  shape. Checked against the rules opencode resolves.
- The agent list is retried when opencode answers a not-yet-loaded folder with an empty list (it was
  treated as "no agents", which could fail a valid task with `no_agent`).
- `oc --help` (and `-h`, `help`) shows oc's own help and starts nothing; before, it ran opencode's help and
  started the dispatcher and the tmux status tab as a side effect. `oc` now reports whether the status
  tab was opened, was already open, or tmux is not in use.
- `opencode api` calls read their output through a temp file: opencode truncates large responses
  (the agent list is ~330 KB) when stdout is a pipe.

### Removed

- Company-specific and provider-specific content from the original prototype (agents, models, docs and
  the wiki integration), and the `sdd-*` skill dependency: `plan` now runs its own explore, propose,
  spec, design and tasks phases with approval gates.
- Creating one tmux window and session per agent (the prototype's `oc open`): agents are tabs in
  the opencode you already have open.
