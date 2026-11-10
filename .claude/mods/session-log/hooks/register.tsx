import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register, SessionMessage } from 'claude-code'

import type { GhEntry, GhMetaMap, PromptEntry, SessionLog } from '../types'

import { fetchMeta, key } from './github'
import { linkedTargets, toMarkdown } from './view'

const PANE = 'session-log'
const TITLE = 'セッション履歴'
const EMPTY: SessionLog = { prompts: [], issues: [], prs: [] }
const log = atom({ plugin: 'session-log', key: 'log' } as const, EMPTY, {
  shape: 'log-1',
})
const meta = atom({ plugin: 'session-log', key: 'meta' } as const, {} as GhMetaMap, {
  shape: 'meta-1',
})
// GitHub の状態を取り直す間隔（turn ごとに毎回叩かない）
const STALE_MS = 60_000

const GH_URL = /https:\/\/github\.com\/([\w.-]+\/[\w.-]+)\/(issues|pull)\/(\d+)/
const GH_CLI_CREATE = /\bgh\s+(issue|pr)\s+create\b/

// プロンプト本文からハーネスが差し込む付加情報を取り除く。
function cleanPrompt(text: string): string | null {
  const stripped = text
    .replace(/<system-reminder>[\s\S]*?<\/system-reminder>/g, '')
    .trim()
  if (stripped === '') return null
  // スキル本文・タスク通知などの自動投入メッセージは人のプロンプトではない。
  if (stripped.startsWith('<')) return null
  if (stripped.startsWith('Base directory for this skill:')) return null
  return stripped
}

function str(v: unknown): string {
  return typeof v === 'string' ? v : ''
}

function textOf(v: unknown): string {
  if (v === undefined || v === null) return ''
  if (typeof v === 'string') return v
  try {
    return JSON.stringify(v)
  } catch {
    return ''
  }
}

function ghEntry(
  id: string,
  tool: string,
  input: Record<string, unknown>,
  output: string,
): GhEntry | null {
  let kind: GhEntry['kind'] | null = null
  if (tool === 'mcp__github__issue_write' && str(input.method) === 'create') kind = 'issue'
  else if (tool === 'mcp__github__create_pull_request') kind = 'pr'
  else if (tool === 'Bash') {
    const m = GH_CLI_CREATE.exec(str(input.command))
    if (m) kind = m[1] === 'pr' ? 'pr' : 'issue'
  }
  if (kind === null) return null

  const url = GH_URL.exec(output)
  const repo =
    url?.[1] ?? [str(input.owner), str(input.repo)].filter(Boolean).join('/')
  return {
    id,
    kind,
    number: url ? Number(url[3]) : null,
    title: str(input.title) || '(タイトル不明)',
    url: url ? url[0] : null,
    repo,
  }
}

function collect(messages: readonly SessionMessage[]): SessionLog {
  const prompts: PromptEntry[] = []
  const issues: GhEntry[] = []
  const prs: GhEntry[] = []
  for (const m of messages) {
    if (m.role === 'user') {
      if (m.toolResults && m.toolResults.length > 0) continue
      const text = cleanPrompt(m.text)
      if (text !== null) prompts.push({ index: prompts.length + 1, text })
      continue
    }
    for (const use of m.toolUses) {
      if (use.isError === true) continue
      const output = `${str(use.text)}\n${textOf(use.result)}`
      const entry = ghEntry(use.tool_use_id, use.tool, use.input, output)
      if (entry === null) continue
      // 結果がまだ無い（実行中）か URL が取れないものは成功扱いにしない。
      if (entry.url === null) continue
      ;(entry.kind === 'pr' ? prs : issues).push(entry)
    }
  }
  return { prompts, issues, prs }
}

// 集計の失敗で本来の処理（ツール実行・プロンプト送信）を止めない。
async function refresh($: EngineInterface): Promise<void> {
  try {
    const messages = await $.session.messages()
    const next = collect(messages)
    await update($, log, () => next)
  } catch {
    // 次のイベントで再集計される
  }
}

// セッションの Issue・PR と、その参照先の状態を GitHub から取り直す。
async function syncGithub($: EngineInterface, force: boolean): Promise<void> {
  try {
    const data = await read($, log)
    const known = await read($, meta)
    const now = Date.now()
    const fresh: GhMetaMap = {}
    const want = new Map<string, { repo: string; number: number; kind: 'issue' | 'pr' | 'unknown' }>()
    const add = (repo: string, number: number, kind: 'issue' | 'pr' | 'unknown') => {
      const k = key(repo, number)
      const cur = known[k]
      if (!force && cur && now - cur.fetchedAt < STALE_MS) return
      if (!want.has(k)) want.set(k, { repo, number, kind })
    }
    for (const g of [...data.prs, ...data.issues]) {
      if (g.number !== null && g.repo !== '') add(g.repo, g.number, g.kind)
    }
    const fetchAll = async () => {
      await Promise.all(
        [...want.entries()].map(async ([k, t]) => {
          const m = await fetchMeta($, t.repo, t.number, t.kind).catch(() => null)
          if (m) fresh[k] = m
        }),
      )
      want.clear()
    }
    await fetchAll()
    // 1 段だけ参照先（PR が言及する Issue・Issue のクローズ予定 PR）も取る。上限 20 件。
    for (const t of linkedTargets(data, { ...known, ...fresh }).slice(0, 20)) add(t.repo, t.number, 'unknown')
    await fetchAll()
    if (Object.keys(fresh).length > 0) await update($, meta, cur => ({ ...cur, ...fresh }))
  } catch {
    // GitHub に届かなくても一覧自体は出す
  }
}

async function markdown($: EngineInterface): Promise<string> {
  return toMarkdown(await read($, log), await read($, meta))
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    const ran = await next(e)
    await $.command.register({
      name: 'session-log',
      description: 'このセッションのプロンプト・Issue・PR 一覧を表示する',
    })
    await refresh($)
    void syncGithub($, false)
    void $.ui.open({ id: PANE, title: TITLE })
    return ran
  })

  // ペインを置けない画面（スマホ版など）でも見られるよう、一覧を Markdown で返す。
  on('command.run', { command: 'session-log' }, async $ => {
    await refresh($)
    await syncGithub($, true)
    void $.ui.open({ id: PANE, title: TITLE })
    return { text: await markdown($) }
  })

  on('prompt.submit', async ($, e, next) => {
    const ran = await next(e)
    await refresh($)
    return ran
  }).catch(($, e, next) => next(e))

  on('tool.call', async ($, e, next) => {
    const ran = await next(e)
    if (
      e.tool === 'mcp__github__issue_write' ||
      e.tool === 'mcp__github__create_pull_request' ||
      e.tool === 'Bash'
    ) {
      await refresh($)
      if (e.tool !== 'Bash') void syncGithub($, false)
    }
    return ran
  }).catch(($, e, next) => next(e))

  on('turn.complete', async ($, e, next) => {
    const ran = await next(e)
    await refresh($)
    void syncGithub($, false)
    return ran
  })

  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const { Box, Markdown } = $.ui.resolve(e)
    const text = toMarkdown(await read($, log), await read($, meta))
    return (
      <Box flexDirection="column" paddingX={1}>
        <Markdown text={text} />
      </Box>
    )
  })
}
