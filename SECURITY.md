# Security policy

## Supported versions

Security fixes go into the latest release. Older releases are not patched; upgrade with
`brew update && brew upgrade codetypera2a`, or rebuild from `master`.

| Version | Supported |
| ------- | --------- |
| latest release (currently 1.4.x) | yes |
| older releases | no |

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
- `init`, which writes into your opencode configuration (`opencode.jsonc`, agents, plugin, MCP entries).
- The agent templates in `agents/` (permissions that are wider than their role needs).
- The opencode plugin in `plugins/a2a/`.

Out of scope: problems in opencode itself, in the model providers, or in the companion tools (`engram`,
`codegraph`, `context7`, `playwright`); report those to their own projects.

## Things to know

- Sessions the dispatcher opens use **your opencode permissions**; there is no sandbox. Agents run with the
  tools you give them, so review the `tools:` and `permission:` blocks of any agent you add.
- Task messages are stored in plain text in `~/.config/opencode/a2a/hub.db`. Never put secrets in them.
- `init --force` and `models` rewrite files in your opencode config directory; a backup of
  `opencode.jsonc` is kept next to it (`opencode.jsonc.bak-a2a`, `opencode.jsonc.bak-a2a-models`).
