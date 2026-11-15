---
name: rdd-review
description: Native gentle-ai receipt-driven review lifecycle. Load ONLY when a review is due - after a work-unit commit where `gentle-ai review assess` returns review_due=true, or when the user asks to start, continue or recover a gentle-ai review. Not for planning, coding, ordinary tests or code-reviewer tasks.
---

# rdd-review: run a native gentle-ai review

## Before you start
- `gentle-ai review mode status` is read-only. If RDD is disabled, stop: no review, no retry, no re-enable.
- Risk comes only from `gentle-ai review assess --cwd <repo> --json`. Never invent risk, PASS, receipts or approval; a failed assessment never lowers risk. Passive documentation needs only a structural readback.
- Finish every source-mutating formatter or generator before START. Any byte, path or mode change after START invalidates the receipt.

## Verification by tier (delegated writes)
- RDD on: the writer runs the named checks; the native review is the independent check.
- RDD off or unknown: passive = structural readback; medium = writer verification (independent if the writer ran on a small model); high or unassessable = writer verification plus an independent verifier.
- The parent re-runs one reported command before delivery.

## Lifecycle
1. Preflight: run the selectorless STATUS (`gentle-ai review status --cwd <repo> --contract gentle-ai.review-integration/v2 --agent claude-code --next-transition`). It returns one START.
2. START: run it exactly as returned. If it returns a consent envelope, relay it complete (every group, label, effect and allowed answer; translate prose, never the answer tokens) and run only the invocation the human picks. A decline is for this candidate only.
3. Stay bound: keep the returned repository, lineage, target and revision. Run only the returned `next_transition` (`execute`: run it; `collect`: satisfy the named inputs with their exact capture operations, then STATUS again; `stop`: run nothing). Never rebuild a command from prose.
4. Capture: run each returned `review.capture-result` exactly as returned; several reviewer slots in one collect run concurrently in provider order. Never build reviewer prompts, never add `--input`, never read the live worktree instead of the provider-bound frozen trees.
5. Recovery: after an empty, malformed or failed capture, re-run the bound STATUS and relaunch only a slot it re-offers.
6. Correction: at most one bounded correction, only through the returned `status_continuation`.
7. Acknowledge: run the exact `review.acknowledge-approved` once; report the burn from its envelope.

## Stops and forecasts
- Relay any `forecast` complete; it is informational.
- A `stop` ends the transition and never approves delivery. Follow its reason code's continuation from the installed gentle-ai contract; if none applies, report the decision needed.

## Never
- Toggle RDD, skip consent, or treat a review outcome as permission to commit, push, open a PR, merge or release.
