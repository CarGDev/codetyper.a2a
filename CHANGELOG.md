# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project aims to follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [2.1.14] - 2026-10-25

Claude Code becomes a full runtime for the hub: its agents are woken by the dispatcher, its usage and messages are
visible, and `init` sets everything up in one command.

### Added

- **Ollama models in opencode.** `init` (opencode) probes the local Ollama, `$OLLAMA_HOST` and every
  `--ollama-url <url>` host (remembered in `<config>/a2a/ollama.json`) with read-only `GET /api/tags` and
  `POST /api/show`, and writes one provider per host (`ollama`, `ollama-<host>`) into `opencode.jsonc`: only
  tool-capable models, embedding models are listed but not added. Idempotent, user edits kept, backup
  `opencode.jsonc.bak-a2a-ollama`, an unreachable host is one line, `--no-ollama` skips it.
- **Claude Code starts on the orchestrator.** `init` (plain or `--claude`) sets `"agent": "orchestrator"` in
  `<claude dir>/settings.json` when there is no `agent` key and the orchestrator agent is installed; another agent you
  chose is kept with a note, a rerun changes nothing, `init --no-default-agent` skips it.
- **A2A pane for Claude Code.** `oc messages --json [--since <iso>] [--limit N]` prints the latest task messages
  (task, from, to, state, text, ts), newest first and read-only. The mod `a2a-pane` (`plugins/claude-a2a/`) polls it
  with `oc status --json` and draws a pane beside the chat (dispatcher dot, tasks coloured by state with
  `from -> to`, the last messages of each, a highlighted block for `input-required` questions with option buttons and
  an input that run `oc answer`), a one-line band above the prompt when the pane is not placed, and `/a2a` to toggle
  it. `init --claude` writes it to a local marketplace folder (`<claude dir>/a2a-marketplace/`) and enables it itself
  by running `claude plugin marketplace add` and `claude plugin install a2a-pane@codetypera2a --scope user --config
  exe=<binary>` (skipped when done, `claude plugin update` when the version changed, a failure is one line with the
  manual commands).
- **`init` is one command that finishes the job.** Plain `codetypera2a init` (screen, `--yes`, no terminal) gives
  the same complete Claude Code setup as `init --claude`; `--claude` / `--opencode` only limit it. Reruns refresh
  files the user did not edit through `<claude dir>/.a2a-manifest.json` (edited files are kept and named, `--force`
  replaces them, files from an install made before the manifest are replaced once with the old copy kept as
  `<file>.bak-a2a`, an up-to-date install writes 0 files) and repair our status line and hooks when the binary moved.
- **Claude Code in tmux.** A `SessionStart` hook (`oc statusline --hook ensure`) starts the dispatcher and the
  `a2a-status` window inside tmux (silent, detached, nothing outside tmux). Turns run by the Claude backend stream
  `stream-json` into `<db>.claude-runs/<session>.log` and get a tmux window per agent that follows them (`oc follow`;
  `DISPATCH_AUTO_OPEN=0` disables it; closing it never cancels the turn).
- **Claude Code agents are woken by the dispatcher.** A `Backend` trait (`src/dispatcher/backend.rs`) separates the
  dispatcher's rules from the runtime; `OpencodeBackend` is the unchanged default and `ClaudeBackend`
  (`claude_backend.rs`) runs a headless `claude -p --agent <name>` turn per wake (`--session-id <uuid>` first,
  `--resume` after, in the project folder, `--allowedTools` from the agent's `tools:` plus `mcp__a2a__*`, prompts
  with `mcp__a2a__*` names). Choose with `DISPATCH_BACKEND` and `DISPATCH_BACKEND_<AGENT>`; `CLAUDE_BIN` overrides the
  binary. A failed turn fails the task with `agent_error:`; `oc cancel` stops a running turn; `oc status` counts
  Claude turns. The dispatcher records no usage for these turns (the transcript ingester already does). Database
  schema v5 adds `sessions.backend` (NULL = opencode; backup `<db>.bak-v4`); the hub's `register_agent` no longer
  retires a Claude session.
- **Results for interactive Claude Code sessions.** `init --claude` adds a `UserPromptSubmit` hook
  (`oc statusline --hook inbox`) that puts the requester's finished results into the next prompt (marking them seen)
  and the footer shows `N results waiting`. Existing hooks are never overwritten.
- **`oc stats --model` per tool, with Claude Code usage.** The table is now split into one titled table per source
  (`opencode`, `claude code`), each with its own total and footnote; empty sources are skipped. Database schema v4
  adds `events.source` (NULL, the dispatcher's rows, reads as opencode), `events.ref` (unique per source), their
  indexes and an `ingest_offsets` table (backup `<db>.bak-v3`; a v2 file upgrades in one open). A new ingester
  (`src/launcher/claude_usage.rs`) reads the Claude Code transcripts under `$CLAUDE_CONFIG_DIR` or `~/.claude`
  (subagent transcripts too), one usage event per message id (the largest output wins when a message is streamed
  over several lines), from the byte offset where the last run stopped, tolerating bad JSON, a half-written last
  line and vanished files. The `claude code` table lists `in` (non-cached input), `out` and `cache r/w`
  separately, as Claude Code's `/usage` does (schema v6 adds `events.tokens_cache_read`/`tokens_cache_write`; backup
  `<db>.bak-v5`; the derived `claude-code` rows of v4/v5 are dropped and rebuilt from the transcripts on the next
  ingest). Cost is an estimate that prices cache reads at 0.1x and 5-minute / 1-hour cache writes at 1.25x / 2x of the
  input price (transcript `cache_creation` split), and stays blank for unlisted models; the `tasks` column of this
  table is `sessions`. Transcripts do not hold every call Claude Code bills (for example side queries), so figures can
  be lower than `/usage`. `oc stats --model` runs it first (a failure is a warning on stderr) and
  `oc stats --ingest` runs only it and prints the new rows.
- **Claude Code footer status line.** `init --claude` also adds a `statusLine` and `SubagentStart` / `SubagentStop`
  hooks to `settings.json` (only when absent, backup `settings.json.bak-a2a`, reruns change nothing). `oc statusline`
  prints one line: dispatcher up/down, open hub tasks (from>to) and the subagents Claude Code is running, tracked by
  `oc statusline --hook start|stop` in `<config>/a2a/statusline-subagents/` (`A2A_STATUSLINE_DIR` overrides; entries
  older than 10 minutes are ignored). It reads the hub read-only and always exits 0. Code: `src/launcher/statusline.rs`,
  `src/targets/settings.rs`.
- **Authenticated task assignment (opt-in).** When `<runtime home>/auth/trusted-keys` exists (lines of
  `<kid> <64 hex Ed25519 public key>`, at most two for rotation; optional `auth/revoked-jti`), only the orchestrator
  holding a valid signed credential may create tasks or act as a task's requester (reply, cancel). The trusted runtime
  hands the hub a one-shot 0600 credential file (`A2A_CREDENTIAL_FILE`, read once, then deleted); the hub verifies
  signature, key id, issuer, audience, exact agent `orchestrator`, lifetime (15 minutes at most, 60 s skew), replay and
  revocation, and on every call re-checks expiry and that the orchestrator session is live in the same project.
  `from`, `_meta` and environment values never establish identity; denials are stable categories with no secrets and
  create no rows. Assignees keep claiming and updating under the existing ownership rules. With no keys file nothing
  changes; with keys but no valid credential task creation is denied. A task can never be sent to
  `gentle-orchestrator`.
- **Callbacks reach an interactive Claude Code session in tmux.** The `SessionStart` hook records the session's pane
  (`<runtime home>/claude-runs/<session>.pane`, validated ids, never for headless runs: they carry `A2A_HEADLESS`); when
  a task you sent changes state, the Claude backend types the one-line nudge into that pane (only while it is not
  mid-turn, otherwise it retries) instead of running a headless turn, so you no longer have to ask for status.
- **CI:** the uninstall/reinstall integration test sets `SHELL` itself, so it no longer depends on the runner's shell.
- **Claude turns no longer fail with "Credit balance is too low".** Headless `claude -p` turns start without
  `ANTHROPIC_API_KEY` when your `claude.ai` subscription is logged in, so they use it instead of an API key left in
  the shell. `A2A_CLAUDE_AUTH=auto|subscription|api` chooses (default `auto`: subscription if `claude auth status` shows
  one, else the key, else Claude's own behaviour); the choice and the credential each turn really used (`apiKeySource`)
  are written to the dispatcher log.
- **The tmux wake-up no longer garbles your input box.** The nudge is pasted in one piece (bracketed paste) instead of typed
  key by key, is never sent while the input box already holds text, waits 8 s between two nudges to the same pane, and
  clears its own text if it was not submitted.
- **The interactive Claude session is registered.** The `SessionStart` hook registers it as the project's orchestrator
  (session, folder, tmux pane), so a finished task finds its requester and the nudge is typed into that pane.
  `list_agents` for a Claude caller no longer runs opencode.
- **`AGENTS.md` and `CLAUDE.md`.** Standing rules for the agents that work on this repository (read-only git, the version
  rule while developing, the go-to-production routine, safety defaults). Not an agent template and not shipped; `CLAUDE.md`
  imports it for Claude Code, opencode reads `AGENTS.md`.
- **Engineering rules, by what each agent can do.** The mandatory Code Quality, Clean Architecture, Security and
  Engineering Standards live in `docs/engineering-rules.md`, split by `<!-- rule:ID -->` markers into chunks (the numbered
  sections, code style and secure dispatch per language, design principles). `docs/rules-profiles.txt` composes profiles
  and gives each agent only what it can use: writers, reviewers, debuggers and test writers get the code rules, and only
  the languages they work in (the TypeScript agents no longer carry Go, Java or SQL); `architect`,
  `software-architecture-expert` and `plan` get short design, contract, security and definition-of-done chunks with no code
  examples; `security-scanner` the security and dispatch chunks; `test-runner` static analysis and testing; `brain`,
  `mentor`, `orchestrator`, `summarizer`, `doc-lookup`, `git-historian` and `pr-commenter` none. The orchestrator instead
  requires every implementation task to say "follow your rules, run the formatter, linter and tests, report real
  results". A test fails when an agent's block differs from its profile. `codetypera2a update` refreshes installed agents.
- **Agents are told who their orchestrator is.** The assignee nudge now names the requester and says to report only
  with `update_task`, never by creating a task; `gentle-orchestrator` is refused as a target and hidden from
  `list_agents`.
- **`codetypera2a uninstall` and `reinstall`.** Rust only. `uninstall` deletes everything, for good and with no backups:
  the agent and command files `init` installed in `~/.claude` and the opencode dir (never files you wrote), our entries in `opencode.jsonc`, `~/.claude.json` and
  `settings.json`, the shell block, the runtime homes and databases (legacy included), our dispatchers, the tmux window
  and the binary with `oc`. `reinstall` does the same except the binary, runs `init --yes` and verifies there is no live
  dispatcher, no legacy database and an empty database. Dry run unless `--yes`.
- **Runtimes stay apart by themselves.** The dispatcher only asks a runtime that has open work, routed by each
  task's context (a Claude session's tasks are `claude`, set by its `A2A_RUNTIME=claude` MCP entry), so a Claude-only
  install never contacts opencode and needs no setting.
- **`DISPATCH_ONLY=claude|opencode`.** Optional hard override: runs one runtime only: the other is never built, asked, woken or started, and new
  tasks are routed to the chosen one whatever the caller or per-agent override says. Unset keeps both.
- **`oc status --json` `diagnostics`.** An additive, read-only block: owner (`canonical`/`legacy`), whether the guard
  is enforced, open counts, oldest open age and 24-hour `stall_retry` / `stall_failed` counts. Existing keys are unchanged.

### Changed

- The live-legacy migration refusal now reads `... migration refused (owner=legacy-live, destination_touched=false; ...)`;
  the prefix and the check order are unchanged.
- New dependency: `ed25519-dalek`.

### Known limits (Claude Code)

- An interactive Claude Code session is never woken by the dispatcher (a headless `claude -p` cannot inject into it).
  It gets results on its next prompt through the `UserPromptSubmit` hook and the `N results waiting` footer, so an
  orchestrator that must react the moment a task finishes still works best in opencode.
- Agents run on Claude only when you route them there (`DISPATCH_BACKEND` / `DISPATCH_BACKEND_<AGENT>`); the default
  stays opencode.
- The `plan` agent and the `/plan-*` commands still look for plans in `.opencode/plans/`.
- Claude Code cost in `oc stats` is an estimate from the repo's price table (blank for models it does not list).

## [2.0.0] - 2026-10-24

The major version is for one breaking change: `codetypera2a init` is no longer opencode-only (see Changed).

### Added

- **Claude Code as an install target.** `codetypera2a init --claude` converts the embedded agent and slash-command
  templates to Claude Code's format and installs them into `~/.claude` (21 agents in `agents/`, 3 commands in
  `commands/`), then registers the MCP servers `a2a`, `codegraph`, `context7`, `engram` and `playwright` in the
  user-scope MCP file (`~/.claude.json`, backed up first as `~/.claude.json.bak-a2a`). Existing entries are never
  overwritten, except that a dead `a2a` binary path is repaired. The same source files feed both tools, so there
  is nothing extra to maintain. `$CLAUDE_CONFIG_DIR` and `--claude-dir <path>` choose another config dir (its MCP
  file is then `<dir>/.claude.json`). The converter maps the model to a tier (`sonnet`, `haiku`, `opus`; `gpt-*`
  becomes `inherit`), `tools:` to Claude Code tool names (`a2a_*` becomes `mcp__a2a`) and the colour to a name
  Claude Code accepts, and drops `mode` and `steps`.
- **Target picker.** On a terminal, `init` opens a multi-select screen (detected tools are checked, tools it cannot
  set up yet are listed as "coming soon": Codex CLI, Cursor, Gemini CLI, Kiro, VS Code Copilot Chat). New flags
  `--claude`, `--opencode`, `--all` and `--yes` skip the screen. Code lives in `src/targets/` (`mod.rs`,
  `convert.rs`, `claude.rs`, `picker.rs`).
- `oc stats --model` (also `--models`): the usage table per model instead of per agent, with tokens in/out, a share
  bar, cost, turns, tasks and agents that used each model. Events without a model are grouped as `unknown`. Plain
  `oc stats` is unchanged.
- **Database schema v3:** `events.model` is now a real column with the index `idx_events_model (kind, model)`, so
  usage can be tracked and queried per model. The first run of a 2.0 binary migrates the hub database in one
  transaction: it copies the file to `hub.db.bak-v2`, adds the column and fills it from the model saved in each
  existing usage event (rows without one stay empty and show as `unknown`). The dispatcher writes the model into
  the column (and still into `detail`). Older binaries keep working on a v3 file but do not fill the column;
  `oc stats --model` still attributes those rows from `detail`.

### Changed

- **BREAKING:** `init` without flags shows the picker on a terminal. Without a terminal, or with `--yes`, it sets up
  every tool it detects, which now includes Claude Code, instead of opencode only (opencode is still the fallback
  when nothing is detected). Scripts that relied on the old behaviour should pass `--opencode`.
- `update` and `init --md-only` still refresh only opencode's markdown; add `--claude` to refresh Claude Code's too.
- Claude Code agents differ from opencode's in three ways: only the 3 `plan-*` commands are embedded, so only those
  are converted; opencode's `permission.task` allow-list becomes `Agent(a, b)` in `tools:` (it limits an agent
  started with `claude --agent`); and the codegraph, engram and context7 tools are always added to each agent's
  `tools:`, because Claude Code gives an agent only the tools it lists.
- `help` and the README describe both tools.

### Not ported yet

- The dispatcher still wakes opencode sessions only. Agents that send a task and wait for the result (the
  `orchestrator`, `brain`) install for Claude Code but only work fully with opencode for now; Claude Code can
  already call the hub's `a2a_*` tools (as `mcp__a2a__*`).
- The live status UI (tabs plugin, `a2a-status` panel) has no Claude Code version yet; `oc status` and `oc watch`
  still work from a shell. Both are planned for the next phase.

## [1.4.0] - 2026-10-23

### Fixed

- `oc stats` printed `~$-0.0000` for an unpriced total and `n/a` for free opencode models; the total is now `~$0.0000` and `-free` models count as $0.

### Added

- opencode panels (`plugins/a2a/panels.tsx`, loaded by the tabs plugin and optional): a sidebar task table with a progress bar and the hop-cap marker, a banner above the prompt when a task waits for you or a context hit the hop cap, a footer status (dispatcher, open tasks, today's cost and tokens), a system notification and sound when a task needs you, completes or fails, and the commands `/a2a-answer` (pick one of the numbered options of an `input-required` task), `/a2a-continue` and `/a2a-cancel`. Each agent has its own colour (`color:` in its file).
- `oc status --json [--dir <folder>]` and `oc answer <task> <text>`: what the panels read and do.
- `.update.sh <version>` checks the version, changelog and clean tree, then pushes master and the tag.
- Executor agents (`code-debugger`, `test-case-generator`, `owasp-guardian`, `react-frontend-expert`, `typescript-mcp-expert`, `playwright-tester`, `api-design-architect`) get an "Act, don't deliberate" rule: start with the first concrete action and hand real design or tradeoff questions to `architect`, `mentor` or `brain` instead of deliberating in their own turn.
- A dispatcher started through the `oc` launcher (`oc dispatch`) was not recognised as one, so its pidfile looked stale and a second dispatcher took over (two "dispatcher up" lines per start, tasks delivered twice, `oc status` blind to it). It is now recognised under both names.
- The orchestrator told `plan` and `architect` to write plans under `~/.config/opencode/plans`, which they cannot write; plans now go in `.opencode/plans/<slug>/` of the project.
- `git-historian` reported "access denied" for `git -C <dir>` and piped commands; it is told to run plain git commands, and `git shortlog` / `git rev-list` are allowed.
- a2a needs tmux: `oc` (and the plugin's `oc ensure`) refuses to start the dispatcher and opencode when tmux is not installed or the pane is not inside tmux, and says what to do. opencode's own flags and plain subcommands (`models`, `run`, `--version`, ...) still run without it.
- The sidebar progress bar is a solid filled bar over a thin rule with the count on the right (`██████──────  6.0 / 12`), instead of `[####----]`.
- `plan` and `architect` may write to `~/.config/opencode/plans/<project>/<number>/` (Engram first, files when Engram is unavailable); the orchestrator uses that location and the small-chunks rule (work units of at most 400 changed lines).
- `oc stats` and `oc trace` show token counts with `k`, `M` and `B` suffixes (`770.8k`, `2.3M`).
- `codetypera2a init --md-only` (alias `codetypera2a update`) refreshes only the agent, command and PROTOCOL markdown; `opencode.jsonc`, MCP entries, the plugin and the models screen are left alone, and saved models are put back into the agent files.
- `oc stats` prints a coloured table sorted from the agent that used the most tokens to the least, with a share bar per agent, red failure counts and a total row. Values that are not known are left blank instead of `n/a`. Colour is only used on a terminal (and not with `NO_COLOR`).
- The project is now licensed under the Apache License 2.0 (was MIT): `LICENSE`, `NOTICE` and `Cargo.toml` updated.
- Added `CODE_OF_CONDUCT.md` (Contributor Covenant 2.1) and `SECURITY.md`; `CONTRIBUTING.md` covers the small-chunks rule, the Code of Conduct, licensing and releasing.
- The README is restructured: header with badges, "Use" first, A2A background, community and author sections, and the screenshots and demo GIF.
- `models` and `init --force` now write the chosen model into the installed agent files too. opencode resolves an agent's model from its file before `opencode.jsonc`, so a model picked in `models` was ignored and the file's template model was used.
- Delivery rule for every agent that writes files (the executors plus `software-architecture-expert` and `pr-commenter`) and for the orchestrator: work goes plan - design - task - implementation, and one task is at most 400 changed lines unless it is a migration that cannot be split. An agent that lacks the plan, design or task list asks for it with `input-required` instead of inventing it.
- The orchestrator states plans as a table (agent, goal, depends on, done when, state, progress bar) and sends every A2A task in a fixed shape (GOAL, CONTEXT, SCOPE, STEPS, ACCEPTANCE, LIMITS, REPORT BACK) so the receiver can start without asking.
- The orchestrator can call the read-only subagents `code-reviewer`, `doc-lookup`, `git-historian`, `security-scanner`, `summarizer` and `test-runner` through the Task tool (`permission.task` allowlist); the subagents get `steps` caps.

## [1.3.3] - 2026-10-23

### Fixed

- Starting opencode through the `oc` name (the `alias opencode='oc'` from `init --shell`, or the `oc` symlink) opened `a2a-status` tmux windows endlessly (106 in one report). The window ran `oc oc watch` and the dispatcher was started as `oc dispatcher`; `oc` handed those words to opencode, which failed, closed the window and got it reopened by the next startup check. Under the `oc` name the commands are now `oc watch` and `oc dispatch`.
- The `a2a-status` window no longer tries to open another one from inside itself, and a short-lived lock lets only one of several simultaneous callers (hub, tabs plugin, `oc`) open it.

### Added

- Releases keep the version being replaced installable from Homebrew as `codetypera2a@<version>` (keg-only), so older versions stay available next to the latest.
- `security-scanner` agent: checklist of the OWASP Top 10:2025 categories and parameter hunting for the OWASP Top 25 Parameters vulnerability classes.

## [1.3.2] - 2026-10-21

### Fixed

- A task whose agent's turn fails (for example a provider `HTTP 403`) now fails at once with that error (`agent_error`) instead of sitting in `submitted` until the stall timeout.
- `oc stop` / `oc shutdown` left the dispatcher stopped for a few seconds only: the hubs opencode keeps running restarted it every 15 s. A `.stopped` marker now stops that until `oc`, `oc ensure` or a new hub starts it explicitly; detached children are also reaped so a stopped dispatcher is not left as a zombie.
- The exit cleanup no longer prints `N other opencode(s) still running` (its output could end up in the dispatcher log).

## [1.3.1] - 2026-10-21

### Added

- Exiting opencode ends the setup: when the last opencode started through `oc` (or the tabs plugin) exits, open tasks are canceled, running turns are interrupted, the sessions are retired, the dispatcher stops and the `a2a-status` tab closes. Other opencodes still running keep it alive; `A2A_KEEP_RUNNING=1` disables it; `oc shutdown [--force]` does it by hand.
- `oc cancel <--all | task... | --context <ctx> | --dir <folder>>`: cancel open tasks from the terminal, mark their pending deliveries dead, and interrupt the turns that are running for them (`--no-interrupt` to leave those alone).

### Fixed

- Old dispatchers survived upgrades and ran next to the new one: against the new schema they failed (`NOT NULL constraint failed: nudges.updated_at`) and each opened its own session, so every task spawned several orchestrator/agent tabs. Dispatchers now register their version; `oc`/`oc ensure` replace an older one (never a newer one), and `oc stop` ends every dispatcher of the database. Dispatchers from before this version cannot be fixed: run `pkill -f "codetypera2a dispatcher"` once.
- Delivery to a session whose folder no longer exists (a moved project) returned `HTTP 404` and was retried forever. Such sessions are retired and replaced, a task for a missing folder fails with `directory_missing`, and a failing delivery is retried 5 times with growing waits, then reported on the task as `delivery_failed` instead of looping.
- The dispatcher never opens an `orchestrator` session: there is one orchestrator per project, the user's own, and results return to it. The hub refuses a task sent to oneself or to `orchestrator`.

## [1.3.0] - 2026-10-21

### Added

- Hub database schema v2 (`a2a_hub.dbml`): `contexts`, agent status, task lineage/priority/version, session keys and leases, a nudge outbox with claim leases, event correlation ids. Migrated automatically from the old schema (a backup is kept next to the database as `<db>.bak-v1`); stop the running dispatcher (`oc stop`) before upgrading.
- `claim_task`: atomic task claim. Of several agents asking for the same task exactly one wins; `update_task` is refused for unclaimed `any` tasks and for non-owners, and takes an optional `expectedVersion`.
- Token and cost per agent: the dispatcher records what an assignee's session spent on each task (opencode's session totals), `oc stats` and `oc trace` show tokens and cost (an estimate from the published price table when opencode reports 0).

### Fixed

- Two agents could do the same `any` task and the second silently took it over, spending the tokens twice.

## [1.2.0] - 2026-10-20

### Added

- `oc open-project <dir> [--prompt <text>]`: opens a project in its own tmux tab running `oc` and can hand a task to that project's orchestrator.
- `oc status` prints each open task's `ctx=` and flags `HOP-CAPPED` contexts.

### Changed

- Orchestrator: no task, no `build`; no design, no tasks; no plan, no design.
- Orchestrator: route per request type (feature, debug only, debugger first, tests first), always ending with testing; plan/design/tasks persisted to Engram and `~/.config/opencode/plans/<project>/<slug>/`; `oc status` gate before every dispatch (answers waiting tasks, `oc continue` on hop caps); `db.dbml` step for the project's own SQL; opens other projects' tabs only when asked.

## [1.1.2] - 2026-10-20

### Changed

- CI and release workflows use `actions/checkout@v5` (Node 24), which removes the Node.js 20
  deprecation warning.
- First release cut by `release.yml`: pushing the tag creates the GitHub release and updates the
  Homebrew tap (`url` and `sha256`) automatically. No code changes since 1.1.1.

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
