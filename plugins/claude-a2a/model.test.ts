import { expect, test } from 'claude-code/testing'

import { clip, parse, sorted, summary } from './hooks/model'

const task = (short: string, state: string) => ({ id: short, short, from: 'orchestrator', to: 'x', state, question: null })

test('open and waiting tasks sort first and the band summarises them', () => {
  const tasks = [task('c', 'completed'), task('w', 'working'), { ...task('q', 'input-required'), question: { text: 'Which?\n1) a', options: ['a'] } }]
  expect(sorted(tasks).map(t => t.short)).toEqual(['q', 'w', 'c'])
  const line = summary({ isDispatcherUp: true, tasks, messages: [], error: null })
  expect(line).toBe('a2a ● 2 open · 1 waiting: q x: Which?')
  expect(summary({ isDispatcherUp: true, tasks: [task('c', 'completed')], messages: [], error: null })).toBeNull()
})

test('clip keeps the first line and an unreadable output keeps the last good data', () => {
  expect(clip('abcdefghij\nsecond', 5)).toBe('abcd…')
  const last = { isDispatcherUp: true, tasks: [task('w', 'working')], messages: [], error: null }
  const next = parse('not json', '{}', last)
  expect(next.tasks).toEqual(last.tasks)
  expect(next.error).toMatch(/not readable/)
})
