import { expect, test } from 'claude-code/testing'

const PANE = {
  plugin: 'a2a-pane',
  component: 'Pane',
  requestId: 'a2a',
  props: {
    title: 'A2A',
    isFocused: false,
    bodyColumns: 60,
    placement: 'dock',
    scroll: { offset: 0, bodyRows: 30 },
    view: {},
  },
} as const

const STATUS = {
  dispatcher: true,
  tasks: [
    { id: 'aaaaaaaa1', short: 'aaaaaaaa', from: 'orchestrator', to: 'code-reviewer', state: 'input-required', question: { text: 'Which branch?\n1) main\n2) dev', options: ['main', 'dev'] } },
    { id: 'bbbbbbbb1', short: 'bbbbbbbb', from: 'orchestrator', to: 'test-runner', state: 'working', question: null },
  ],
}
const MESSAGES = {
  messages: [
    { task: 'aaaaaaaa1', short: 'aaaaaaaa', from: 'code-reviewer', to: 'orchestrator', text: 'Which branch?', ts: '2026-01-01T00:00:02.000Z' },
    { task: 'bbbbbbbb1', short: 'bbbbbbbb', from: 'orchestrator', to: 'test-runner', text: 'run the suite', ts: '2026-01-01T00:00:01.000Z' },
  ],
}

function stubEngine(on: Parameters<Parameters<typeof test>[1]>[1], calls: string[][]) {
  on('command.register', () => ({ value: { command: 'a2a' } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  on('clock.every', () => ({ value: {} }))
  on('process.run', (_, e) => {
    calls.push([...e.argv])
    const out = e.argv.includes('messages') ? MESSAGES : e.argv.includes('status') ? STATUS : null
    return { value: { exitCode: 0, stdout: out ? JSON.stringify(out) : 'answered', stderr: '' } }
  })
}

test('the pane draws the tasks, their messages and the question that waits', async ($, on) => {
  const calls: string[][] = []
  stubEngine(on, calls)
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })

  // The first poll ran `<exe> oc status --json` and `<exe> oc messages --json`.
  expect(calls.some(c => c.join(' ') === 'codetypera2a oc status --json')).toBe(true)

  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect((await ui.find({ type: 'Text', text: /dispatcher running/ }))?.text).toContain('running')
  expect(await ui.find({ type: 'Text', text: /orchestrator -> code-reviewer/ })).toBeTruthy()
  expect(await ui.find({ type: 'Text', text: /run the suite/ })).toBeTruthy()
  expect(await ui.find({ type: 'Text', text: /Waiting for you/ })).toBeTruthy()
  await ui.unmount()
})

test('pressing an option answers the task through oc answer', async ($, on) => {
  const calls: string[][] = []
  stubEngine(on, calls)
  on('ui.toast', () => ({ value: null }))
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })

  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  await ui.press({ plugin: 'a2a-pane', key: 'aaaaaaaa1-o1' })
  expect(calls.some(c => c.join(' ') === 'codetypera2a oc answer aaaaaaaa1 dev')).toBe(true)
  await ui.unmount()
})
