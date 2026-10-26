# Security policy

## Supported versions

Security fixes go into the latest release. Older releases are not patched; upgrade with
`brew update && brew upgrade codetypera2a`, or rebuild from `master`.

| Version | Supported |
| ------- | --------- |
| latest release (currently 2.1.x) | yes |
| older releases (1.x and earlier) | no |

## Reporting a vulnerability

Please **do not open a public issue** for a security problem.

- Email **carlos@cargdev.io** with the subject `codetyper.a2a security`, or
- use GitHub's private vulnerability reporting on <https://github.com/CarGDev/codetyper.a2a/security/advisories/new>.

Include what you found, the version (`codetypera2a --version`), your OS, how to reproduce it and, if you
can, the impact. Do not include real secrets or other people's data.

You can expect an answer within 5 working days. For a confirmed problem we agree on a fix and a
disclosure date with you (90 days at most unless it needs longer), release the fix, and credit you in
`CHANGELOG.md` if you want.

## What is in scope

- The hub, the dispatcher and the `oc` launcher: task handling, the SQLite database, process and tmux handling.
- `init`, which writes into your opencode configuration (`opencode.jsonc`, agents, plugin, MCP entries) and, with
  `--claude`, into your Claude Code configuration (`~/.claude/agents`, `~/.claude/commands`, and the `mcpServers` of
  `~/.claude.json`).
- The agent templates in `agents/` (permissions that are wider than their role needs).
- The opencode plugin in `plugins/a2a/`.

Out of scope: problems in opencode itself, in the model providers, or in the companion tools (`engram`,
`codegraph`, `context7`, `playwright`); report those to their own projects.

## Things to know

- Sessions the dispatcher opens use **your opencode permissions**; there is no sandbox. Agents run with the
  tools you give them, so review the `tools:` and `permission:` blocks of any agent you add.
- Task messages are stored in plain text in `~/.codetypera2a/hub.db`. Never put secrets in them.
- `init --force` and `models` rewrite files in your opencode config directory; a backup of
  `opencode.jsonc` is kept next to it (`opencode.jsonc.bak-a2a`, `opencode.jsonc.bak-a2a-models`).
- `init --claude` edits `~/.claude.json` in place (or `<dir>/.claude.json` with `CLAUDE_CONFIG_DIR`) and writes
  `.claude.json.bak-a2a` next to it first. Close Claude Code before running it, since Claude Code rewrites that file
  too. Agents installed into `~/.claude/agents` run with your Claude Code permissions, like any subagent.
- `init --claude` runs `claude plugin marketplace add <claude dir>/a2a-marketplace` and `claude plugin install
  a2a-pane@codetypera2a` (and `update`) by itself, with `CLAUDE_CONFIG_DIR` set to the dir it writes to. The plugin is
  the embedded pane mod, read from that local folder; it runs `<binary> oc status --json`, `oc messages --json` and, when you press an answer button, `oc answer`, and nothing else. The
  `SessionStart` hook starts `oc ensure` (dispatcher and tmux status window) when a session starts inside tmux.
  `init` records hashes of the files it wrote in `<claude dir>/.a2a-manifest.json` and replaces only files that still
  match them (files from an install made before the manifest are replaced once; the old copy is kept as
  `<file>.bak-a2a`).
- With `DISPATCH_BACKEND=claude` the dispatcher starts `claude -p` for the agents you route to it. Those turns cannot
  ask for permission, so they get only the tools of the agent's `tools:` line (`--allowedTools`) plus the hub's
  `mcp__a2a__*`; an agent file without a `tools:` line gets Read, Grep, Glob, Edit, Write and Bash. Review the
  `tools:` of an agent before routing it to Claude. The run state (`<db>.claude-runs/`) holds only session ids,
  folders and pids.
- The `UserPromptSubmit` hook reads the hub and, to mark results as seen, writes `tasks.requester_seen_at`; it adds
  the last words of each finished task to your prompt, so treat agent output as untrusted text like any tool result.
