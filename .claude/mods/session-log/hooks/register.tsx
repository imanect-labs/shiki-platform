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
  shape: 'meta-2',
})
// GitHub の状態を取り直す間隔（turn ごとに毎回叩かない）
const STALE_MS = 60_000
// 1 回の同期で参照先（セッション外の Issue・PR）を取りに行く上限
const LINKED_LIMIT = 20

const GH_URL = /https:\/\/github\.com\/([\w.-]+\/[\w.-]+)\/(issues|pull)\/(\d+)/g
// コマンドの先頭か、;・&&・|・( の直後に置かれた gh だけを作成とみなす（grep の引数などは除く）
const GH_CLI_CREATE = /(?:^|[;&|(]\s*)gh\s+(issue|pr)\s+create\b/m

// 人が送ったものではない、ハーネスが差し込むメッセージの書き出し
const AUTO_PREFIXES = [
  '<task-notification>',
  '<command-name>',
  '<command-message>',
  '<local-command-caveat>',
  '<local-command-stdout>',
  '<local-command-stderr>',
  'Base directory for this skill:',
  'This session is being continued from a previous conversation',
]

function cleanPrompt(text: string): string | null {
  const stripped = text.replace(/<system-reminder>[\s\S]*?<\/system-reminder>/g, '').trim()
  if (stripped === '') return null
  if (AUTO_PREFIXES.some(p => stripped.startsWith(p))) return null
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

function createKind(tool: string, input: Record<string, unknown>): GhEntry['kind'] | null {
  if (tool === 'mcp__github__issue_write' && str(input.method) === 'create') return 'issue'
  if (tool === 'mcp__github__create_pull_request') return 'pr'
  if (tool === 'Bash') {
    const m = GH_CLI_CREATE.exec(str(input.command))
    if (m) return m[1] === 'pr' ? 'pr' : 'issue'
  }
  return null
}

/** 作成系の呼び出しとその出力から 1 件作る。出力に作成先の URL が無ければ作成扱いにしない。 */
function ghEntry(
  id: string,
  tool: string,
  input: Record<string, unknown>,
  output: string,
): GhEntry | null {
  const kind = createKind(tool, input)
  if (kind === null) return null
  const want = kind === 'pr' ? 'pull' : 'issues'
  // gh は作成した URL を最後に出す。途中に出てくる別の URL は拾わない。
  const urls = [...output.matchAll(GH_URL)].filter(m => m[2] === want)
  const url = urls[urls.length - 1]
  if (!url) return null
  if (tool === 'Bash' && /already exists/i.test(output)) return null
  return {
    id,
    kind,
    number: Number(url[3]),
    title: str(input.title) || '(タイトル不明)',
    url: url[0],
    repo: url[1] ?? '',
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
      if (entry !== null) (entry.kind === 'pr' ? prs : issues).push(entry)
    }
  }
  return { prompts, issues, prs }
}

/**
 * 会話記録は圧縮（/compact）や件数上限で古い分が落ちるため、置き換えずに追記する。
 * プロンプトは保存済みの末尾と新しい集計の先頭が重なる位置を探し、その後ろだけ足す。
 */
export function mergePrompts(stored: PromptEntry[], fresh: PromptEntry[]): PromptEntry[] {
  const a = stored.map(p => p.text)
  const b = fresh.map(p => p.text)
  let overlap = 0
  // 記録が丸ごと残っている（保存済みが新しい集計の先頭と一致する）のが普通の場合
  if (b.length >= a.length && a.every((t, i) => t === b[i])) {
    overlap = a.length
  } else {
    for (let k = Math.min(a.length, b.length); k > 0; k--) {
      if (a.slice(a.length - k).every((t, i) => t === b[i])) {
        overlap = k
        break
      }
    }
  }
  const texts = [...a, ...b.slice(overlap)]
  return texts.map((text, i) => ({ index: i + 1, text }))
}

function mergeGh(stored: GhEntry[], fresh: GhEntry[]): GhEntry[] {
  const out = new Map<string, GhEntry>()
  const k = (g: GhEntry) => (g.number === null ? g.id : key(g.repo, g.number))
  for (const g of [...stored, ...fresh]) if (!out.has(k(g))) out.set(k(g), g)
  return [...out.values()]
}

function mergeLog(cur: SessionLog, fresh: SessionLog): SessionLog {
  return {
    prompts: mergePrompts(cur.prompts, fresh.prompts),
    issues: mergeGh(cur.issues, fresh.issues),
    prs: mergeGh(cur.prs, fresh.prs),
  }
}

// 集計の失敗で本来の処理（ツール実行・プロンプト送信）を止めない。
async function refresh($: EngineInterface): Promise<void> {
  try {
    const fresh = collect(await $.session.messages())
    await update($, log, cur => mergeLog(cur ?? EMPTY, fresh))
  } catch {
    // 次のイベントで再集計される
  }
}

async function addEntry($: EngineInterface, entry: GhEntry): Promise<void> {
  try {
    await update($, log, cur => {
      const base = cur ?? EMPTY
      return entry.kind === 'pr'
        ? { ...base, prs: mergeGh(base.prs, [entry]) }
        : { ...base, issues: mergeGh(base.issues, [entry]) }
    })
  } catch {
    // turn.complete の再集計で拾われる
  }
}

type Target = { repo: string; number: number; kind: 'issue' | 'pr' | 'unknown' }

function dedupe<T extends { repo: string; number: number }>(list: T[]): T[] {
  const seen = new Set<string>()
  return list.filter(t => {
    const k = key(t.repo, t.number)
    if (seen.has(k)) return false
    seen.add(k)
    return true
  })
}

async function syncOnce($: EngineInterface, force: boolean): Promise<void> {
  const data = await read($, log)
  const known = await read($, meta)
  const now = Date.now()
  const fresh: GhMetaMap = {}
  const due = (k: string) => {
    const cur = known[k]
    return force || !cur || now - cur.fetchedAt >= STALE_MS
  }
  const run = (targets: Target[]) =>
    Promise.all(
      targets.map(async t => {
        const m = await fetchMeta($, t.repo, t.number, t.kind).catch(() => null)
        if (m) fresh[key(t.repo, t.number)] = m
      }),
    )

  const own: Target[] = dedupe(
    [...data.prs, ...data.issues]
      .filter(g => g.number !== null && g.repo !== '')
      .map(g => ({ repo: g.repo, number: g.number as number, kind: g.kind })),
  )
  await run(own.filter(t => due(key(t.repo, t.number))))

  // 1 段だけ参照先も取る。取得済みで新しいものを除いてから上限で切る。
  const ownKeys = new Set(own.map(t => key(t.repo, t.number)))
  const linked: Target[] = dedupe(linkedTargets(data, { ...known, ...fresh }))
    .filter(t => !ownKeys.has(key(t.repo, t.number)) && due(key(t.repo, t.number)))
    .slice(0, LINKED_LIMIT)
    .map(t => ({ ...t, kind: known[key(t.repo, t.number)]?.kind ?? 'unknown' }))
  await run(linked)

  const entries = Object.entries(fresh)
  if (entries.length === 0) return
  await update($, meta, cur => {
    const next: GhMetaMap = { ...(cur ?? {}) }
    // 後から始まった古い取得で上書きしない
    for (const [k, m] of entries) {
      const prev = next[k]
      if (!prev || prev.fetchedAt <= m.fetchedAt) next[k] = m
    }
    return next
  })
}

// 同期は 1 本ずつ。走っている間に来た要求は、終わった後に 1 回だけまとめて走らせる。
let syncing: Promise<void> | null = null
let rerun: { force: boolean } | null = null

function syncGithub($: EngineInterface, force: boolean): Promise<void> {
  if (syncing) {
    rerun = { force: force || (rerun?.force ?? false) }
    return syncing
  }
  syncing = (async () => {
    try {
      let next: { force: boolean } | null = { force }
      while (next) {
        rerun = null
        await syncOnce($, next.force).catch(() => undefined)
        next = rerun
      }
    } finally {
      syncing = null
    }
  })()
  return syncing
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    const ran = await next(e)
    await $.command.register({
      name: 'session-log',
      description: 'このセッションのプロンプト・Issue・PR 一覧を表示する',
    })
    // 再開した長いセッションで最初のプロンプトを待たせないよう、集計は待たない
    void refresh($).then(() => syncGithub($, false))
    void $.ui.open({ id: PANE, title: TITLE })
    return ran
  })

  // ペインを置けない画面（スマホ版など）でも見られるよう、一覧を Markdown で返す。
  on('command.run', { command: 'session-log' }, async $ => {
    await refresh($)
    await syncGithub($, true)
    void $.ui.open({ id: PANE, title: TITLE })
    return { text: toMarkdown(await read($, log), await read($, meta)) }
  })

  // 作成系の呼び出しだけ、その結果から直接 1 件足す（会話記録は読まない）。
  on('tool.call', async ($, e, next) => {
    const ran = await next(e)
    const input = e as unknown as Record<string, unknown>
    if (ran.deny === undefined && ran.isError !== true && createKind(e.tool, input) !== null) {
      const out = `${str(ran.text)}\n${textOf(ran.result)}`
      const entry = ghEntry(e.tool_use_id, e.tool, input, out)
      if (entry !== null) {
        await addEntry($, entry)
        void syncGithub($, true)
      }
    }
    return ran
  }).catch(($, e, next) => next(e))

  on('turn.complete', async ($, e, next) => {
    const ran = await next(e)
    // サブエージェントのターンでは集計しない
    if (e.agentId === undefined) {
      await refresh($)
      void syncGithub($, false)
    }
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
