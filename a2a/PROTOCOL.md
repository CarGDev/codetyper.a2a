# A2A between agents (the `a2a` hub)

Every agent works in its own opencode session, shown as a tab in the opencode you already have
open (switch tabs with Ctrl+Tab), so agents cannot call each other directly. They talk through a
shared hub: an MCP server registered as `a2a` in `opencode.jsonc` (`codetypera2a hub`).
Its semantics follow the A2A spec (AgentCard, Task lifecycle, Message + Parts, Artifact,
contextId). State is one SQLite file (`~/.config/opencode/a2a/hub.db`), shared by every
opencode window.

## Tools (shown to you as `a2a_<name>`)

| Tool                      | Use                                                                                          |
| ------------------------- | -------------------------------------------------------------------------------------------- |
| `register_agent`          | publish/update your AgentCard (all `agents/*.md` are auto-registered)                        |
| `list_agents`             | every agent opencode has (built-in, yours, project, plugin) + registered; filter by text. Any listed agent can be an assignee |
| `send_message`            | no `taskId`: create a Task for `to` (or `any`). With `taskId`: reply on it                   |
| `inbox`                   | `as:"assignee"` (default): work waiting for you. `as:"requester"`: updates on tasks you sent |
| `get_task` / `list_tasks` | read a task (status, history, artifacts) / filter by contextId, state, from, to              |
| `claim_task`              | take a task before working on it; atomic (one winner). Required for tasks sent to `any`      |
| `update_task`             | assignee only: ask a question, finish, attach an artifact; optional `expectedVersion`        |
| `cancel_task`             | requester or assignee                                                                        |

## Task lifecycle

`submitted` -> `working` -> `completed` | `failed` | `rejected` | `canceled`.
Paused: `input-required`, `auth-required`. Terminal states are immutable; to continue, create a
new task with `referenceTaskIds`. A requester reply on an `input-required` task sets it back to
`submitted`.

## Conventions

- Your agent name = your agent file name (`orchestrator`, `mentor`, ...). Always pass it as `agent`/`from`.
- **Start of work:** `inbox {agent, directory}`. If a task is for you: `claim_task {id, agent}`. If it fails, someone else has the task: stop, do not work on it
  (several agents may see the same `any` task; only the claim winner may update it).
- **Delegate:** `send_message {from, to, text, contextId, directory}`; keep the returned `id`, then END YOUR TURN.
  Results arrive as an `A2A:` prompt in the same session.
- **Finish:** `update_task {state:"completed"|"failed"|"input-required", text, artifact:{name, text|data|url}}`.
  Put the deliverable in the artifact, not in a long message.
- **Always report back** to the session the task came from (`fromSession`, shown on the task): finish with
  `update_task` (never answer only in your own tab). The hub wakes the requester there.
- Use one `contextId` per user goal so `list_tasks {contextId}` shows the whole thread.
- Always pass `directory:"<your cwd>"` so tasks stay scoped to your project.
- Never put secrets or tokens in messages. Keep messages short; reference file paths for large output.
- `brain` only advises on routing and writes prompts; it never executes tasks.

## Automatic wake-ups (dispatcher)

Agents do not poll. The dispatcher (`codetypera2a dispatcher`, a plain process with no model)
checks the hub every 3 s and talks to the OpenCode server API:

- task `submitted` (new, or reopened by a reply) -> wakes the assignee in ITS OWN session:
  `A2A: you are agent "x". Task <id> is waiting ...` (`POST /api/session/{id}/prompt`,
  `delivery: queue`). The first time an agent gets work in a project the dispatcher creates its
  session (`POST /api/session`, agent set) and registers it; later tasks reuse it.
- Tabs: the dispatcher appends a line to `tabs.jsonl` (next to `hub.db`) whenever it hands an agent
  work. The TUI plugin installed by `init` (`plugins/a2a/tui.ts`) tails that file and opens the
  session as a tab (`context.ui.tabs.open`, focus unchanged). Needs session tabs enabled
  (default; `cli.json` `tabs`).
- task `input-required | auth-required | completed | failed | rejected` -> wakes the requester
  once, in the session the task was sent from (the orchestrator's tab):
  `A2A: task <id> that you sent to "x" is now <state> ...`.
- A session that is running a turn is never interrupted; the wake-up waits until it is idle.
- One nudge per (task, status message). On start, already-finished tasks are baselined so old
  results never wake anyone. A stalled task is retried once, then marked `failed`.
- **Requester session:** the hub detects the session a tool call comes from (the one running a
  turn in that folder) and stores it as `from_session`; else a session the requester registered;
  else the newest session open in that folder. If a session cannot be opened the task gets a
  `no_session` message (state `input-required`) and the requester is woken.
- Starting: the hub starts the dispatcher itself when opencode loads the `a2a` MCP server, and
  `oc` (or `opencode` once `codetypera2a init --shell` aliased it) does too. It is single-instance
  (pidfile `<hub.db>.dispatcher.pid`); log `<hub.db>.dispatcher.log`.

## Same project = same root

Folders match by project, not exact path: same `git rev-parse --show-toplevel`. So `/path` and
`/path/src` reach each other; two different repos never match. A folder outside git is its own
project. `send_message {directory, fromDirectory}`: `directory` is the TARGET project,
`fromDirectory` is your own cwd (pass it for cross-project tasks).

## Hop cap

Per `contextId`, `DISPATCH_MAX_HOPS` (default 40) agent-to-agent nudges. At the cap the
dispatcher posts one `hop_cap:` message on the newest open task (state `input-required`) and wakes
the requester. To go on, reply on that task with text containing `continue`, or run
`oc continue <contextId>`; each grant adds another `DISPATCH_MAX_HOPS`. To stop, cancel the task.

## Working patterns (orchestrator)

- **Verify before done:** a finished change is reviewed by `code-reviewer` and tested by `test-runner`
  (parallel, read-only, not the implementer); findings go back once, then the user decides.
- **Parallel probes:** for uncertain work, the same task with different approaches goes to 2-3
  different agents at once (separate sessions run in parallel), read-only; the orchestrator compares
  the artifacts and applies one.
- **Lessons:** at the end of a context the orchestrator saves at most one evidence-backed lesson to
  Engram (`a2a/lessons/<project>`); `brain` reads them when routing.

## Commands

- `oc` / `opencode` (aliased): run opencode here with the dispatcher; in tmux a detached
  `a2a-status` window follows the dispatcher live.
- `oc status` dispatcher + open tasks, `oc watch` live view, `oc dispatch` dispatcher in this pane,
  `oc stop`, `oc agent <name>` switch this folder's session to an agent, `oc continue <ctx>`.
- `codetypera2a check-models` lints models against `a2a/model-policy.json`; `codetypera2a e2e-test`
  runs the hub test (19 checks, two processes).

## Fallback (if the `a2a_*` tools are not available)

File mailbox: `<project>/.opencode/a2a/<task_id>/NNN-<from>-to-<to>.md` with a header
(`task_id, seq, from, to, status, payload_path, created`) then SUMMARY / FILES / EVIDENCE /
OPEN ISSUES / SUGGESTED NEXT. Tell the user the fallback is in use.
