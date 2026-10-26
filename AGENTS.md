# Rules for agents working on this repository

> **Scope.** These rules are for the agents that work ON this repository (Claude Code, opencode, Codex, any other
> development CLI). This file is **not an agent template**. The product's agent templates are the files in `agents/`:
> that is what `codetypera2a init` installs for users. Never copy these rules into `agents/`, and never read
> `agents/*.md` as instructions for yourself. Nothing in this file is shipped or installed.

## 1. Git is read-only

- Never run `git commit`, `add`, `push`, `pull`, `fetch --prune`, `tag`, `merge`, `rebase`, `reset`, `restore`, `stash`,
  `cherry-pick`, `revert`, `clean`, `mv`, `rm`, or a `checkout`/`switch`/`branch` that changes anything. Never run any
  other command that changes the repository, its index, its history or a remote.
- Read-only git is fine: `status`, `diff`, `log`, `show`, `blame`, `ls-files`, `rev-parse`.
- Never add a co-author line or any attribution trailer.
- When work is ready, print the exact git commands for the user to run. The user commits.

## 2. Version while developing

The version lives in `Cargo.toml` and in the `codetypera2a` entry of `Cargo.lock`.

- At the start of a work stream, decide its size once. A **big change** (new architecture, breaking behavior) starts a new
  MAJOR, for example `2.1.17` to `3.0.0`. Anything smaller stays in the current minor, for example `2.1.17`.
- Every prompt iteration that changes the product bumps the **PATCH**: `2.1.11` to `2.1.12`, `3.0.0` to `3.0.1`.
- Documentation shows the line, never a patch, until release: `3.0.x` or `2.1.17` (the `CHANGELOG.md` heading is
  `## [2.1.17] - unreleased`, the README badge and the `--version` example say `2.1.17`).
- After a change, reinstall (`cargo install --path . --force`) so the installed binary has the new version.

## 3. Going to production: only when the user says so

Then, and only then, do all of this in order:

1. Re-run `security-scanner` and `code-reviewer` (read-only) on everything new. Fix HIGH and CRITICAL findings first.
2. Put the real version in the docs: `CHANGELOG.md` heading `## [X.Y.Z] - <date>`, the README badge and `--version`
   example, and the `SECURITY.md` supported line. `Cargo.toml` and `Cargo.lock` already say `X.Y.Z`.
3. Run `cargo fmt -- --check` on touched files, `cargo clippy --all-targets`, the full `cargo test` and
   `cargo build --release`.
4. Install the binary and check `codetypera2a --version`.
5. Print the git commands (branch, add, commit, tag, push) for the user. Do not run them.
6. Report what ships, the known limits and the upgrade notes. After the user commits, development resumes at the next
   patch and the docs go back to the `X.Y.x` form.

## 4. Every change set

- Write the test first or alongside the change. Run the focused tests, then the whole suite.
- When behavior changes, update `codetypera2a help`, `README.md` and `CHANGELOG.md` in the same change.
- Follow `docs/engineering-rules.md`. Each template in `agents/` carries only the part its capabilities call for
  (`docs/rules-profiles.txt`); edit those two files, never an agent's rules block by hand (a test checks the match).
- Keep source files under 500 lines, with no commented-out code and no temporary task comments.
- Never claim a check passed that was not run.

## 5. Safety defaults

- Anything destructive is a dry run unless `--yes` is given.
- Tests never touch the real home: use a temporary `HOME`, `A2A_DB` and runtime home.
- Never read, print or store secrets and credentials.

## 6. Project facts to keep in mind

- Claude Code and opencode stay separate: routing follows the caller automatically, and `DISPATCH_ONLY` is only an
  optional override.
- The A2A `orchestrator` is the only task assigner. `gentle-orchestrator` is never called, routed to or used.
- A finished task reaches an interactive Claude session through its registered tmux pane; headless turns use the
  subscription unless `A2A_CLAUDE_AUTH` says otherwise.
