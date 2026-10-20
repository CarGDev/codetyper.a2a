---
description: Resume an existing plan at its next incomplete phase.
agent: plan
---

Resume an existing plan.

Arguments: `$ARGUMENTS`

Parse the arguments as `<project-path-or-slug>` and, if given, a specific
plan slug. If only a project is given and it has more than one plan under
`.opencode/plans/`, list them and ask which one to resume.

Then:
1. Read whatever files already exist under `.opencode/plans/<slug>/`.
2. Show me a short summary of what's already done and which phase comes
   next.
3. Resume at that phase (still pausing for my approval after propose/spec/
   design/tasks if those gates haven't been passed yet).

Resumption is entirely file-presence based.
