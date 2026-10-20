---
description: Start a new phased plan (explore -> propose -> spec -> design -> tasks) for a task, with approval gates.
agent: plan
---

Start a new plan.

Arguments: `$ARGUMENTS`

Parse the arguments as `<project-path-or-slug> <task description>` (the
first token/path identifies the project, the rest is the description). If
it's ambiguous which project is meant, ask before proceeding.

Then:
1. Derive a short kebab-case plan slug from the description.
2. Confirm with me: project, plan slug, and a one-line restatement of the
   task, before creating any files.
3. Create `.opencode/plans/<slug>/` in that project and begin at the
   explore phase, following the full flow in your instructions (pausing for
   my approval after propose/spec/design/tasks).
