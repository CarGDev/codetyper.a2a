<!-- markdownlint-disable-next-line MD041 -->
<a id="top"></a>

<div align="center">

<img src="images/a2a_banner.png" alt="codetyper.a2a banner" width="100%">

<img src="images/a2a_logo.png" alt="codetyper.a2a logo" width="120">

<h1>codetyper.a2a</h1>

<p><strong>An agent-to-agent hub for opencode v2: every agent in its own tab, one orchestrator in charge.</strong></p>

<p>
<a href="https://github.com/CarGDev/codetyper.a2a/releases"><img src="https://img.shields.io/github/v/release/CarGDev/codetyper.a2a?style=for-the-badge&label=release" alt="Latest release"></a>
<img src="https://img.shields.io/badge/version-1.4.0-2ac3de?style=for-the-badge" alt="Version 1.4.0">
<img src="https://img.shields.io/badge/Rust-1.85%2B-f74c00?style=for-the-badge&logo=rust&logoColor=white" alt="Rust 1.85+">
<img src="https://img.shields.io/badge/macOS%20%C2%B7%20Linux-7aa2f7?style=for-the-badge" alt="Platform">
<a href="LICENSE"><img src="https://img.shields.io/badge/License-Apache_2.0-blue.svg?style=for-the-badge" alt="License: Apache 2.0"></a>
</p>

<p>
<a href="#use"><strong>Use</strong></a> &bull;
<a href="#install"><strong>Install</strong></a> &bull;
<a href="a2a/PROTOCOL.md"><strong>Protocol</strong></a> &bull;
<a href="CHANGELOG.md"><strong>Changelog</strong></a> &bull;
<a href="https://cargdev.io/"><strong>Website</strong></a> &bull;
<a href="https://cargdev.io/blog"><strong>Blog</strong></a>
</p>

<br/>

<p>
Your agents work one at a time and report only to you. A single long session carries everything,
forgets it on compaction, and has no second opinion.
<strong>codetyper.a2a gives each agent its own session, a task board to talk through, and a dispatcher
that wakes the right one.</strong> An orchestrator plans, routes and delegates; the result comes back
to the session that asked. One Rust binary, no Node.
</p>

<sub><strong>WORKS WITH</strong> <a href="https://opencode.ai">opencode v2</a> &middot; speaks the task model of the
<a href="https://github.com/a2aproject/A2A">Agent2Agent (A2A) protocol</a> &middot; macOS and Linux &middot; needs tmux</sub>

</div>

---

## Use

<p align="center">
  <img src="images/oc-running.gif" alt="opencode running with a2a: the orchestrator sends tasks, agents open as tabs, the status tab follows along" width="100%">
</p>

Start a tmux session first (a2a needs tmux), then run opencode in it:

```sh
tmux
opencode        # or: oc
```

Whenever opencode starts, the tabs plugin runs `oc ensure`: the dispatcher is started if needed and a
detached `a2a-status` tab opens with the live status (outside tmux it stops with a message instead). No alias needed
(`codetypera2a init --shell` only adds `alias opencode='oc'`, which does the same through `oc`).

opencode opens on the `orchestrator`. Ask it for something; it plans with `plan`, routes with
`brain`, sends tasks with `a2a_send_message` and ends its turn. The dispatcher opens the
assignee's own session (first time only) and the tabs plugin shows it as a new tab; when the
agent finishes, the orchestrator's tab is woken with the result. Switch tabs with `Ctrl+Tab`.

| Command                | What it does                                                        |
| ---------------------- | ------------------------------------------------------------------- |
| `oc help`              | oc's own help (opencode's is `opencode --help`)                      |
| `oc status`            | dispatcher state, running sessions, open tasks                      |
| `oc watch`             | live view of the dispatcher log (what the tmux window shows)        |
| `oc dispatch`          | run the dispatcher in this pane (foreground)                        |
| `oc stop`              | stop every dispatcher of this database (after an upgrade `oc` then starts the current one) |
| `oc continue <ctx>`    | allow more hops for a context that hit the hop cap                  |
| `oc trace <task>`      | timeline of one task (id or 8-character prefix): sender, claim, delivery, state changes, duration |
| `oc shutdown`          | end everything: cancel open tasks, interrupt running turns, retire sessions, stop the dispatcher (runs automatically when the last opencode exits; `A2A_KEEP_RUNNING=1` disables) |
| `oc cancel`            | cancel open tasks (`--all`, task ids, `--context`, `--dir`) and interrupt the turns working on them |
| `oc stats`             | coloured per-agent table, most tokens first, with a share bar: tasks, average duration (`3m 46s`), tokens in/out (`1.3M/13.1k`) and cost (opencode's figure, or an estimate from the price table when it reports 0); unknown values stay blank |
| `oc status --json`     | the same state as JSON (tasks, questions, progress, today's cost); `--dir <folder>` |
| `oc answer <task> <text>` | reply to a task that waits for you (`input-required`)           |
| `oc agent <name>`      | switch this folder's newest session to another agent                |
| `oc open-project <dir>` | open another project in its own tmux tab (`--prompt <text>` sends it to that orchestrator) |
| `oc ensure`            | start the dispatcher and the status tab without starting opencode   |
| `codetypera2a models`  | choose each agent's model (`--show`, `--recommended`)               |
| `codetypera2a init --md-only` | refresh only the agent/command markdown (alias `codetypera2a update`) |
| `codetypera2a eval`    | replay the deterministic scenarios (routing, hop cap, no duplicate nudges, seeded-bug reviewer); `--list` shows them |
| `codetypera2a --version` | print the version (also `-V`, `version`; `oc --version` too)      |
| `codetypera2a check-models` | lint models against `model-policy.json`                        |
| `codetypera2a e2e-test`     | hub end-to-end test (also run by `cargo test`)                 |

`oc stats` looks like this:

<p align="center">
  <img src="images/oc-stats.png" alt="oc stats: agents sorted by tokens, with share bars, cost and average duration" width="100%">
</p>

a2a needs tmux (one tab per agent): `oc` refuses to start when tmux is not installed or the pane is not
inside it. opencode's own flags and plain subcommands (`models`, `run`, `--version`) still work without it.

### Inside opencode (the panels plugin)

The plugin adds a few things to the opencode TUI (they come from `plugins/a2a/panels.tsx`; if they do not
show up, the tabs still work):

- **Sidebar:** a progress bar (`██████──────  6.0 / 12`) and one row per task with a state symbol, the
  agent in its own colour (the `color:` of its agent file) and a `⚠ hop cap` marker.
- **Footer:** `a2a ● N open · ~$cost · tokens today`.
- **Banner** above the prompt when a task waits for you or a context hit the hop cap.
- **Notifications:** a toast and, with the window in the background, a system notification and sound when
  a task needs you, finishes or fails.
- **Slash commands:** `/a2a-answer` (pick one of the numbered options of an `input-required` task, or
  write your own), `/a2a-continue` and `/a2a-cancel`.

### Plans and small chunks

The orchestrator makes work go plan, design, task, implementation, and each implementation task is one
work unit of at most 400 changed lines with its tests and docs (a migration that cannot be split is the
only exception). Plans are saved to Engram first (`a2a/plans/<project>/<slug>`); when Engram is not
available they are files in `~/.config/opencode/plans/<project>/<number>/` (`plan.md`, `design.md`,
`tasks.md`; `<number>` is the next free integer) or `.opencode/plans/<slug>/` in the project.

Protocol details: [`a2a/PROTOCOL.md`](a2a/PROTOCOL.md).

## How a task travels

<p align="center">
  <img src="images/a2a_flow.jpeg" alt="Flow of a task: user, orchestrator, dispatcher, hub, specialist agents and back" width="100%">
</p>

Like the [A2A protocol](https://github.com/a2aproject/A2A) it borrows from, a task has an id, a
`contextId`, messages made of parts, and a state (`submitted`, `working`, `input-required`, `completed`,
`failed`). The hub keeps them in SQLite and exposes them to agents as MCP tools (`a2a_*`), so no agent needs
to know how another one is built. The wire protocol of the A2A project (HTTP agent cards and JSON-RPC) is
not implemented here; this hub is the local, opencode-native version of the same idea.

## Harness engineering

codetyper.a2a is a small agent harness: everything around the model (prompts, tools, memory,
delegation, checks) is plain files you can read and edit. The design follows the practices in Google
Research's [harness2](https://github.com/google-research/harness2) and the
[awesome-harness-engineering](https://github.com/ai-boost/awesome-harness-engineering) list:

| Practice | Where it lives here |
| --- | --- |
| Context: give each agent only what it needs | One session per agent and project; tasks carry a message with parts and a `contextId`; `codegraph` and `engram` MCP servers for retrieval instead of reading whole trees |
| Tools: narrow, well-described interfaces | The hub's `a2a_*` MCP tools (`send_message`, `inbox`, `get_task`, `update_task`, ...); `playwright_*` denied unless an agent needs a browser |
| Guardrails: least privilege and bounded loops | `tools:` / `permission:` blocks per agent template; read-only shells for reviewers; hop cap per context (`oc continue` to allow more); `model-policy.json` lint |
| Verification: independent checks | Every code change is checked by `code-reviewer` and `test-runner`, and reviewers use a different model family than the author |
| Parallel probes (harness2) | For uncertain work the orchestrator sends 2-3 read-only probes to different agents and compares their artifacts before choosing |
| Evidence-backed lessons (harness2) | Lessons are saved to Engram (`a2a/lessons/<project>`) only with evidence, and `brain` reads them when routing |
| Memory and state | SQLite hub (`hub.db`) for tasks and nudges; Engram for long-term lessons |
| Orchestration: plan, decompose, hand off | `orchestrator` -> `brain` -> `plan` -> `architect` -> implementers -> tests, route chosen per request (debug only, debugger first, tests first), plans saved to Engram and `~/.config/opencode/plans/`; `oc open-project <dir>` opens another project in a tmux tab |
| Observability | `oc status`, `oc watch`, the live `a2a-status` tmux window and the dispatcher log; `oc trace <task>` for a per-task timeline and `oc stats` per agent |
| Evals | `codetypera2a eval` replays fixed scenarios (routing, hop cap, no duplicate nudges, seeded-bug reviewer) offline with a fake opencode; `cargo test` and CI can gate on its exit code |
| Human in the loop | Plan approval gates; the dispatcher reports back to the session the task came from |

## Install

### Requirements

- macOS or Linux (it uses `kill`, `ps`, symlinks and process groups; Windows is not supported).
- [opencode](https://opencode.ai) v2 (developed against 2.0.32). Check with `opencode --version`.
- A Rust toolchain (edition 2024, so Rust 1.85 or newer) to build it: `brew install rust` or
  [rustup](https://rustup.rs). SQLite is bundled, nothing else to install.
- `tmux`: a2a opens one tab per agent and the live `a2a-status` window in tmux, so `oc` and the plugin
  refuse to start without it (`brew install tmux`, then run opencode inside a `tmux` session).
- Optional: the companion MCP tools below.

### Option A: Homebrew

The formula lives in the tap [CarGDev/homebrew-codetypera2a](https://github.com/CarGDev/homebrew-codetypera2a).
Homebrew installs the Rust toolchain it needs to build, so you do not need `cargo` or a clone:

```sh
brew install CarGDev/codetypera2a/codetypera2a
codetypera2a --version        # codetypera2a 1.4.0
```

Update later with `brew update && brew upgrade codetypera2a`. To build the latest `master` instead of
the last release: `brew install --HEAD CarGDev/codetypera2a/codetypera2a`.

If you previously ran `cargo install codetypera2a`, remove it first (`cargo uninstall codetypera2a`),
otherwise `~/.cargo/bin` can shadow the Homebrew binary in your `PATH`. If the binary moved (for
example from cargo to Homebrew), you do not have to edit anything by hand: `codetypera2a init` or
any `oc` run repairs a dead `a2a` path in `opencode.jsonc` and `a2a/codetypera2a.path` by itself
(a backup is kept as `opencode.jsonc.bak-a2a`).

### Option B: download the repo

```sh
git clone https://git.cargdev.io/CarGDev/codetyper.a2a.git
cd codetyper.a2a
cargo install --path . --force        # installs codetypera2a into ~/.cargo/bin
```

Or without cloning: `cargo install --git https://git.cargdev.io/CarGDev/codetyper.a2a.git --force`.
Make sure `~/.cargo/bin` is on your `PATH`.

### Then, on either path

```sh
codetypera2a init --shell     # install the agents/commands/plugin/MCP entries; --shell adds alias opencode='oc'
```

Restart opencode (or run `opencode reload`), then check:

```sh
codetypera2a help             # the binary runs
oc status                     # dispatcher: running (it is started by the a2a MCP server)
```

`init` is safe to re-run (existing files are skipped unless `--force`). After upgrading the binary,
`codetypera2a init --md-only` (alias `codetypera2a update`) refreshes only the agent, command and
PROTOCOL markdown and keeps your saved models. Useful options:
`--tools` runs the companion tools' own opencode setup first (`engram setup opencode`,
`codegraph install`), `--no-link` skips the `oc` symlink, `--dir <path>` / `$OPENCODE_CONFIG_DIR`
targets another config dir.

### Companion tools (the other MCP servers `init` registers)

`init` only writes their config; install the programs themselves if you want them connected:

| Tool       | Install                                                                                      |
| ---------- | -------------------------------------------------------------------------------------------- |
| engram     | `brew install gentleman-programming/tap/engram` then `engram setup opencode`                 |
| codegraph  | `curl -fsSL https://raw.githubusercontent.com/colbymchenry/codegraph/main/install.sh \| sh` (or `npm i -g @colbymchenry/codegraph`), then `codegraph init` in each project |
| context7   | remote server, nothing to install (optional API key + skill: `npx ctx7 setup --opencode`)   |
| playwright | needs Node (`npx`)                                                                           |

### Uninstall

```sh
brew uninstall codetypera2a            # or: cargo uninstall codetypera2a
brew untap CarGDev/codetypera2a        # optional: remove the tap too
rm -f ~/.cargo/bin/oc                  # the oc symlink, if init created it
```

Then remove `mcp.servers.a2a` (and `default_agent` if you want your old one back; the previous file
is saved as `opencode.jsonc.bak-a2a`) from `~/.config/opencode/opencode.jsonc`, the block between
`# >>> a2a (codetypera2a) >>>` and `# <<< a2a (codetypera2a) <<<` from your shell rc, and the
installed `~/.config/opencode/{a2a,plugins/a2a}` folders.

`init` (safe to re-run; existing files are skipped unless `--force`):

| It installs                                | Where                                             |
| ------------------------------------------ | ------------------------------------------------- |
| agent templates (`agents/`)                | `~/.config/opencode/agents/`                      |
| slash commands (`commands/`)               | `~/.config/opencode/commands/` (`/plan-new` ...)  |
| tabs plugin (`plugins/a2a/tui.ts`)         | `~/.config/opencode/plugins/a2a/`                 |
| `PROTOCOL.md`, `model-policy.json`         | `~/.config/opencode/a2a/`                         |
| MCP servers `a2a`, `codegraph`, `context7`, `engram`, `playwright` + `default_agent` | `opencode.jsonc` (`mcp.servers`, backup kept; existing entries are never overwritten, except that a dead `a2a` binary path is repaired) |
| `oc` symlink                               | next to the binary                                |

Use `--dir <path>` / `$OPENCODE_CONFIG_DIR` for another config dir, `--no-link` to skip the symlink.

## Layout

```
agents/     agent templates installed by init
commands/   slash commands installed by init
plugins/    opencode TUI plugin (a2a tabs) installed by init
a2a/        PROTOCOL.md (installed)
src/hub/    MCP server (server.rs), SQLite state (db.rs), project identity, e2e test
src/dispatcher/  wakes sessions through the opencode v2 API
src/launcher/    `oc`
```

State: `~/.config/opencode/a2a/hub.db` (+ `.dispatcher.pid`, `.dispatcher.log`).
Env overrides: `A2A_DB`, `A2A_AGENT_DIR`, `A2A_POLICY`, `OPENCODE_BIN`, `DISPATCH_*`.

## Choosing models

```sh
codetypera2a models                 # terminal UI: pick each agent's model from what opencode has
codetypera2a models --recommended   # apply the suggestions below without the UI
codetypera2a models --show          # print the table, change nothing
```

<p align="center">
  <img src="images/codetyepra2a-models.png" alt="codetypera2a models: choose each agent's model from the models opencode has" width="100%">
</p>

`init` offers the same screen at the end (skip with `--no-models`; without a terminal it only prints
a hint). Keys: up/down move, `Enter` opens a filterable model list, `r` applies the recommendations,
`c` clears all overrides, `u` undoes, `s` saves, `q` quits. Choices are saved as `agents.<name>.model`
(`"provider/model"`, optional `#variant`) in `opencode.jsonc`, with a backup
(`opencode.jsonc.bak-a2a-models`); other settings are kept. A file with comments is not rewritten; the
snippet to add is printed instead. The dispatcher passes the agent's model when it opens a session
(a session keeps its own model and would otherwise ignore the agent's).

**Defaults.** Every agent template ships with the model of its role in its `model:` line (the table below),
so the agents work out of the box on GitHub Copilot. The screen shows each agent's real current model
(your `opencode.jsonc` override wins over the file's), marks one your opencode does not have in red, and
`init` prints which template models are missing on your machine. A choice you save overrides the file
and survives `init --force`.

**The table** (GitHub Copilot, token prices from the GitHub docs, USD per 1M tokens in/out, 2026-10;
for models you lack, the screen recommends the next best one you have, GitHub Copilot preferred):

| Role | Agents | Model | $ in / out |
| --- | --- | --- | --- |
| Light work: routing, running commands, summaries, lookups | `brain`, `test-runner`, `summarizer`, `doc-lookup`, `git-historian`, `pr-commenter`, `mentor` | `claude-haiku-5.5` (or `gpt-6-luna`) | 0.10 / 0.50 |
| Everyday coding and agentic work | `orchestrator`, `plan`, `build`, `code-debugger`, `owasp-guardian`, `security-scanner`, `test-case-generator`, `react-frontend-expert`, `typescript-mcp-expert`, `api-design-architect`, `software-architecture-expert`, `playwright-tester` | `claude-sonnet-5.5` (or `gpt-5.6-terra`) | 2 / 10 |
| Design, long-horizon reasoning | `architect` | `claude-opus-5.5` (or `gpt-6.1-sol`) | 4 / 20 |
| Reviewing | `code-reviewer`, `pr-reviewer` | `gpt-6.1-sol`, a different family than the Claude that wrote the code | 2 / 10 |

Agents you add yourself get no recommendation; pick any model or leave opencode's default. The very
top models (`claude-fable-5.1`, `gpt-6-astra`: 10 / 50) are never recommended; pick them by hand where
the extra cost is worth it.

**Policy.** `a2a/model-policy.json` (used by `check-models`) allows every model above, in three tiers
(`cheap`, `sonnet`, `deep`), and anything else is "unknown". The agents listed in `readOnlyAgents`
(`brain`, `mentor`, `summarizer`, `doc-lookup`, `git-historian`, `test-runner`, `pr-commenter`) must stay on a
`cheap` model. A pick the policy does not allow is still saved; the screen warns and `check-models` flags
it. `init` does not overwrite an existing policy file (add `--force`).

## Security notes

- Sessions the dispatcher opens use **your opencode configuration and permissions** (for example an
  allow-all `bash` rule); there is no sandbox. Agents run with whatever tools you give them.
- Keep agents least-privilege: the `orchestrator`, `plan` and `architect` templates cannot edit project
  files (plans go to `.opencode/plans/` or `~/.config/opencode/plans/`), and probes and reviews are read-only by instruction. Check the
  `tools:` and `permission:` blocks of any agent you add.
- Task messages are stored in a local SQLite file (`~/.config/opencode/a2a/hub.db`): never put secrets
  in them.

See [SECURITY.md](SECURITY.md) to report a vulnerability privately.

## Contributing

Contributions are welcome: bug reports, agent templates, tests and docs. Read
[CONTRIBUTING.md](CONTRIBUTING.md) for setup, tests and conventions (small work units of at most 400
changed lines, tests with the change) and the [Code of Conduct](CODE_OF_CONDUCT.md). What changed, release
by release, is in [CHANGELOG.md](CHANGELOG.md).

## Community

- Issues and ideas: <https://github.com/CarGDev/codetyper.a2a/issues>
- Questions and security reports: [carlos@cargdev.io](mailto:carlos@cargdev.io)
- Articles and notes: <https://cargdev.io/blog>

## About the author

<table>
<tr>
<td width="140" valign="top">
<img src="https://cargdev.io/cargdevpage/image/real_carlos.jpeg" width="120" alt="Carlos Gutierrez">
</td>
<td valign="top">

Built by [Carlos Gutierrez](https://cargdev.io/) (CarGDev), senior software engineer and AI enthusiast.
He builds tools around coding agents (codetyper for Neovim, this hub for opencode) and writes about it on
his [blog](https://cargdev.io/blog).

</td>
</tr>
</table>

<div align="center">

<a href="https://cargdev.io/"><img src="https://img.shields.io/badge/Website-cargdev.io-2ac3de?style=for-the-badge&logo=googlechrome&logoColor=white" alt="Website"></a>
<a href="https://cargdev.io/blog"><img src="https://img.shields.io/badge/Blog-cargdev.io%2Fblog-bb9af7?style=for-the-badge" alt="Blog"></a>
<a href="https://github.com/CarGDev"><img src="https://img.shields.io/badge/GitHub-CarGDev-181717?style=for-the-badge&logo=github&logoColor=white" alt="GitHub"></a>
<a href="mailto:carlos@cargdev.io"><img src="https://img.shields.io/badge/Email-carlos%40cargdev.io-7aa2f7?style=for-the-badge&logo=maildotru&logoColor=white" alt="Email"></a>

</div>

## License

[Apache License 2.0](LICENSE). Copyright 2026 Carlos Gutierrez. See [NOTICE](NOTICE).

<div align="center"><a href="#top">Back to top</a></div>
