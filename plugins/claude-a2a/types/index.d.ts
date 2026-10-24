export type Question = { text: string; options: string[] }

export type Task = {
  id: string
  short: string
  from: string
  to: string
  state: string
  question: Question | null
}

export type Message = {
  task: string
  short: string
  from: string
  to: string
  text: string
  ts: string
}

export type Snapshot = {
  isDispatcherUp: boolean
  tasks: Task[]
  messages: Message[]
  error: string | null
}

declare module 'claude-code' {
  interface PluginState {
    'a2a-pane': { snapshot: Shaped<Snapshot> }
  }
}
