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

/** GitHub 上の現在の状態。issue: open / completed / not_planned、pr: draft / open / merged / closed */
export type GhStatus = 'open' | 'completed' | 'not_planned' | 'draft' | 'merged' | 'closed'
/** PR → Issue の関係。closes は「Closes #N」等の自動クローズ、refs はそれ以外の言及 */
export type GhRelation = 'closes' | 'refs'
export type GhLink = { number: number; rel: GhRelation }
/** `${repo}#${number}` をキーにした取得結果 */
export type GhMeta = {
  kind: 'issue' | 'pr'
  repo: string
  number: number
  title: string
  url: string
  status: GhStatus
  /** PR なら本文で言及している番号、Issue ならクローズ予定の PR（closes のみ） */
  links: GhLink[]
  fetchedAt: number
}
export type GhMetaMap = Record<string, GhMeta>

declare module 'claude-code' {
  interface PluginState {
    'session-log': { log: Shaped<SessionLog>; meta: Shaped<GhMetaMap> }
  }
}
