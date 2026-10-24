import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register } from 'claude-code'

import { EMPTY, clip, isWaiting, lastOf, parse, sorted, style, summary } from './model'

const PANE = 'a2a'
const POLL_MS = 4000
const LINES_PER_TASK = 3

let isPolling = false

// Change the tag when the snapshot fields change: it declines what an earlier version stored.
const snapshot = atom({ plugin: 'a2a-pane', key: 'snapshot' } as const, EMPTY, { shape: 'snapshot-1' })

type Dollar = EngineInterface

const oc = (exe: string, ...args: string[]) => [exe, 'oc', ...args]

async function poll($: Dollar, exe: string) {
  if (isPolling) return
  isPolling = true
  try {
    const [status, messages] = await Promise.all([
      $.process.run(oc(exe, 'status', '--json'), { timeoutMs: 8000 }),
      $.process.run(oc(exe, 'messages', '--json', '--limit', '60'), { timeoutMs: 8000 }),
    ])
    const failed = [status, messages].find(r => r.exitCode !== 0)
    await update($, snapshot, last =>
      failed ? { ...last, error: clip(failed.stderr || `exit ${failed.exitCode}`, 80) } : parse(status.stdout, messages.stdout, last),
    )
  } catch (e) {
    await update($, snapshot, last => ({ ...last, error: clip(String(e), 80) }))
  } finally {
    isPolling = false
  }
}

async function answer($: Dollar, exe: string, task: string, text: string) {
  const out = await $.process.run(oc(exe, 'answer', task, text), { timeoutMs: 15000 })
  $.ui.toast(out.exitCode === 0 ? out.stdout.trim() : `answer failed: ${clip(out.stderr, 80)}`)
  await poll($, exe)
}

export const register: Register = (on, options) => {
  const exe = String(options.exe ?? 'codetypera2a')
  on('session.start', async ($, e, next) => {
    await $.command.register({ name: 'a2a', description: 'Show or hide the A2A pane (tasks, messages, questions)' })
    $.clock.every(POLL_MS, () => poll($, exe))
    void poll($, exe)

    return next(e)
  })

  on('command.run', { command: 'a2a' }, async $ => {
    const isOpen = (await $.ui.panes()).some(p => p.id === PANE)
    if (isOpen) {
      await $.ui.close({ id: PANE })

      return { text: 'A2A pane closed.' }
    }
    const opened = await $.ui.open({ id: PANE, title: 'A2A' })

    return { text: opened.isPlaced ? 'A2A pane opened.' : 'A2A pane waits for a wider terminal; the line above the prompt shows the summary.' }
  })

  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const table = $.ui.resolve(e)
    const { Box, Button, Text } = table
    // Mobile has no Input: there the options and the `oc answer` line are the way.
    const Input = 'Input' in table ? table.Input : null
    const s = await read($, snapshot)
    const width = Math.max(20, e.props.bodyColumns)
    const room = Math.max(1, (e.viewport?.rows ?? 24) - 4)

    return (
      <Box flexDirection="column">
        <Text color={s.isDispatcherUp ? 'success' : 'error'}>
          {s.isDispatcherUp ? '● dispatcher running' : '○ dispatcher stopped'}
        </Text>
        {s.error && <Text color="warning">{clip(s.error, width)}</Text>}
        {s.tasks.length === 0 && <Text dimColor>No tasks in the last hour.</Text>}
        {sorted(s.tasks).slice(0, room).map(t => {
          const st = style(t.state)

          return (
            <Box flexDirection="column" marginTop={1}>
              <Text color={st.color}>
                {st.symbol} {t.short} {clip(`${t.from} -> ${t.to}`, width - 12)} <Text dimColor>{t.state}</Text>
              </Text>
              {lastOf(s.messages, t.id, LINES_PER_TASK).map(m => (
                <Text dimColor>
                  {'  '}
                  {clip(`${m.from}: ${m.text}`, width - 2)}
                </Text>
              ))}
              {isWaiting(t) && t.question && (
                <Box flexDirection="column" borderStyle="round" borderColor="error" paddingX={1}>
                  <Text bold color="error">
                    Waiting for you
                  </Text>
                  <Text>{clip(t.question.text, width * 3)}</Text>
                  {t.question.options.map((o, i) => (
                    <Button key={`${t.id}-o${i}`} label={`${i + 1}) ${clip(o, width - 8)}`} onPress={() => answer($, exe, t.id, o)} />
                  ))}
                  {Input && (
                    <Input
                      key={`${t.id}-in`}
                      placeholder={`answer ${t.short}…`}
                      submitLabel="Send"
                      onSubmit={(text: string) => (text.trim() ? answer($, exe, t.id, text) : undefined)}
                    />
                  )}
                  <Text dimColor>or: oc answer {t.short} &lt;text&gt;</Text>
                </Box>
              )}
            </Box>
          )
        })}
      </Box>
    )
  })

  // Fallback: a one-line band above the prompt while the pane is not placed (narrow terminal or closed).
  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    const line = summary(await read($, snapshot))
    const isPlaced = (await $.ui.panes()).some(p => p.id === PANE && p.isPlaced)
    if (e.props.hasSurvey || isPlaced || !line) return next(e)
    const { Box, Text } = $.ui.resolve(e)

    return (
      <Box>
        <Text color="warning">{clip(line, e.props.bodyColumns)}</Text>
      </Box>
    )
  })
}
