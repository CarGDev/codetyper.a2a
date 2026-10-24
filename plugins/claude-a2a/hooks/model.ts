import type { Message, Snapshot, Task } from '../types'

// One row per task state: the symbol and theme colour its row is drawn with.
export const STATES: Record<string, { symbol: string; color: string }> = {
  submitted: { symbol: '○', color: 'subtle' },
  working: { symbol: '●', color: 'warning' },
  'input-required': { symbol: '?', color: 'error' },
  'auth-required': { symbol: '!', color: 'error' },
  completed: { symbol: '✓', color: 'success' },
  failed: { symbol: '✗', color: 'error' },
  canceled: { symbol: '–', color: 'inactive' },
  rejected: { symbol: '–', color: 'inactive' },
}
const FALLBACK = { symbol: '·', color: 'subtle' }
const WAITING = ['input-required', 'auth-required']
const OPEN = ['submitted', 'working', ...WAITING]

export const EMPTY: Snapshot = { isDispatcherUp: false, tasks: [], messages: [], error: null }

export const style = (state: string) => STATES[state] ?? FALLBACK
export const isWaiting = (t: Task) => WAITING.includes(t.state)
export const isOpen = (t: Task) => OPEN.includes(t.state)

/** First line of `text`, cut to `max` cells with an ellipsis. */
export function clip(text: string, max: number): string {
  const line = (text.split('\n')[0] ?? '').trim()
  return line.length <= max ? line : `${line.slice(0, Math.max(1, max - 1))}…`
}

/** The last `n` messages of one task, oldest first (the feed is newest first). */
export function lastOf(messages: Message[], task: string, n: number): Message[] {
  return messages.filter(m => m.task === task).slice(0, n).reverse()
}

/** Open tasks first (waiting ones on top), then the rest; each group keeps the hub's order. */
export function sorted(tasks: Task[]): Task[] {
  const rank = (t: Task) => (isWaiting(t) ? 0 : isOpen(t) ? 1 : 2)

  return [...tasks].sort((a, b) => rank(a) - rank(b))
}

/** Parses the two CLI outputs; any failure becomes `error` and keeps the last good data. */
export function parse(status: string, messages: string, last: Snapshot): Snapshot {
  try {
    const s = JSON.parse(status)
    const m = JSON.parse(messages)

    return { isDispatcherUp: Boolean(s.dispatcher), tasks: s.tasks ?? [], messages: m.messages ?? [], error: null }
  } catch (e) {
    return { ...last, error: `oc output not readable: ${String(e).slice(0, 80)}` }
  }
}

/** The one-line summary the band shows when no pane is placed. */
export function summary(s: Snapshot): string | null {
  if (s.error) return `a2a: ${s.error}`
  const open = s.tasks.filter(isOpen)
  const waiting = s.tasks.filter(isWaiting)
  if (open.length === 0) return null
  const head = `a2a ${s.isDispatcherUp ? '●' : '○'} ${open.length} open`
  const first = waiting[0]
  if (!first) return head
  const ask = first.question ? clip(first.question.text, 60) : first.state

  return `${head} · ${waiting.length} waiting: ${first.short} ${first.to}: ${ask}`
}
