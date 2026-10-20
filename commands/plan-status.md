---
description: List all plans in the current project and their current phase/progress.
agent: plan
---

Report plan status.

Arguments: `$ARGUMENTS`

If arguments name a specific project, report only that project's plans.
Otherwise, check the current project first; if none found and I haven't
specified scope, ask whether I want you to also scan other projects.

For each plan found under `.opencode/plans/*/`, report:
- Plan slug
- Which artifact files exist (explore/proposal/spec/design/tasks)
- Current phase: name of the next phase not yet produced, or, if
  `tasks.md` exists, how many of its checkboxes are checked (e.g. "tasks:
  3/7 done") or "complete" if all are checked.

Present it as a compact table, not prose.

Status is derived from `.opencode/plans/<slug>/*.md` file presence and
`tasks.md` checkboxes only.
