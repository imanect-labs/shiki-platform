export type PromptEntry = { index: number; text: string }
export type GhEntry = {
  id: string
  kind: 'issue' | 'pr'
  number: number | null
  title: string
  url: string | null
  repo: string
}
export type SessionLog = { prompts: PromptEntry[]; issues: GhEntry[]; prs: GhEntry[] }

declare module 'claude-code' {
  interface PluginState {
    'session-log': { log: Shaped<SessionLog> }
  }
}
