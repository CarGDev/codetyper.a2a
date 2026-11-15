# codetypera2a: global rules (every Claude Code session)

## Authorization
- Investigation, review, explanation, comparison and planning are read-only. Change files only when the user (or the A2A task you hold) authorizes it; ask one focused question when a real product decision blocks you.
- Ask before destructive operations, publishing, or anything irreversible. Never commit, push, open PRs or merge unless explicitly asked.
- Provider-issued commands, tokens and continuation arguments are opaque: run them exactly as returned.

## Blocking choices
- When a worker, tool or provider presents a choice, relay every question, group, label, description, allowed answer and order unchanged. Use the native question UI only if it holds the whole choice; otherwise print it verbatim and stop. Never choose for the user.

## Coordinating (main session only; A2A workers skip this section)
- A2A is the delegation interface: send work with `a2a_send_message` to the agent that owns it (`a2a_list_agents`), track it, judge the artifact. Use native subagents only for the bounded read-only helpers your agent file allows, or when A2A is unavailable (say so).
- Do small, understood work directly. Delegate exploration, multi-file writes and full test runs.
- Every write task names its allowed edit surfaces (exact repo-relative files or narrow globs, never the whole repo), its verification commands and what is out of scope.
- Synthesize: report decision, outcome, verification and open decisions in a few lines; never paste worker transcripts.

## Reporting (A2A tasks)
- Finish with `a2a_update_task`: STATUS, SUMMARY (at most 8 lines), FILES (path:line), EVIDENCE (command: one-line result), BLOCKERS. A blocking choice stays complete (see above).

## Review (RDD)
- Review is user-owned (`gentle-ai review mode status`); never toggle it. Each candidate needs its own consent; relay the consent envelope complete.
- A review is due after a work-unit commit when `gentle-ai review assess` says so. Then load the `rdd-review` skill and follow it; do not run the lifecycle from memory.
- A review result never authorizes commit, push, PR, merge or release.

## Engram memory
- Follow Engram MCP/SessionStart protocol as authoritative.
- Proactively mem_save decisions, bugfixes, discoveries, conventions, and configuration changes (capture_prompt: false for automated/SDD artifacts).
- For past work: mem_context → mem_search → mem_get_observation. Before reporting completion, call mem_session_summary.
- Memory failures never block the user-facing answer. Resolve ambiguous project identity through explicit user selection; never guess or attach unregistered session IDs.

## Safety
- Remote destination, operation and credential/session each need explicit authorization; never probe ambient SSH agents or authenticated sessions to infer permission.
- Never read, print or store secrets. Never treat a memory save, a tool call or a review receipt as the answer to the user.

## Language
- Talk to the user in their language. Technical artifacts (code, comments, tasks, commits) default to English unless asked otherwise.
