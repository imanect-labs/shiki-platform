import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register, SessionMessage } from 'claude-code'

import type { GhEntry, PromptEntry, SessionLog } from '../types'

const PANE = 'session-log'
const TITLE = 'セッション履歴'
const EMPTY: SessionLog = { prompts: [], issues: [], prs: [] }
const log = atom({ plugin: 'session-log', key: 'log' } as const, EMPTY, {
  shape: 'log-1',
})

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

function ghMarkdown(list: GhEntry[]): string {
  if (list.length === 0) return '_まだ作成していません_'
  return list
    .map(g => `- **#${g.number ?? '?'}** ${g.title}${g.url ? ` — [${g.repo}](${g.url})` : ''}`)
    .join('\n')
}

function toMarkdown(data: SessionLog): string {
  const prompts =
    data.prompts.length === 0
      ? '_まだありません_'
      : data.prompts
          .map(p => {
            const one = p.text.replace(/\s+/g, ' ')
            return `${p.index}. ${one.length > 200 ? `${one.slice(0, 199)}…` : one}`
          })
          .join('\n')
  return [
    `### Pull Request（${data.prs.length}）`,
    ghMarkdown(data.prs),
    `### Issue（${data.issues.length}）`,
    ghMarkdown(data.issues),
    `### プロンプト（${data.prompts.length}）`,
    prompts,
  ].join('\n\n')
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    const ran = await next(e)
    await $.command.register({
      name: 'session-log',
      description: 'このセッションのプロンプト・Issue・PR 一覧を表示する',
    })
    await refresh($)
    void $.ui.open({ id: PANE, title: TITLE })
    return ran
  })

  // ペインを置けない画面（スマホ版など）でも見られるよう、一覧を Markdown で返す。
  on('command.run', { command: 'session-log' }, async $ => {
    await refresh($)
    void $.ui.open({ id: PANE, title: TITLE })
    return { text: toMarkdown(await read($, log)) }
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
    }
    return ran
  }).catch(($, e, next) => next(e))

  on('turn.complete', async ($, e, next) => {
    const ran = await next(e)
    await refresh($)
    return ran
  })

  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const { Box, Text, Link } = $.ui.resolve(e)
    const data = await read($, log)
    const cols = Math.max(20, (e.props.bodyColumns ?? 60) - 2)
    const clip = (s: string, n: number) =>
      s.length > n ? `${s.slice(0, Math.max(1, n - 1))}…` : s

    const ghRows = (list: GhEntry[], empty: string) =>
      list.length === 0 ? (
        <Text dimColor>{empty}</Text>
      ) : (
        list.map(g => (
          <Box key={g.id} flexDirection="column" marginBottom={1}>
            <Text>
              <Text bold>#{g.number ?? '?'}</Text> {clip(g.title, cols - 8)}
            </Text>
            {g.url !== null ? (
              <Link href={g.url} label={`${g.repo} ↗`} />
            ) : (
              <Text dimColor>{g.repo}</Text>
            )}
          </Box>
        ))
      )

    return (
      <Box flexDirection="column" paddingX={1}>
        <Text bold size="heading">
          Pull Request（{data.prs.length}）
        </Text>
        <Box flexDirection="column" marginBottom={1}>
          {ghRows(data.prs, 'まだ作成していません')}
        </Box>

        <Text bold size="heading">
          Issue（{data.issues.length}）
        </Text>
        <Box flexDirection="column" marginBottom={1}>
          {ghRows(data.issues, 'まだ作成していません')}
        </Box>

        <Text bold size="heading">
          プロンプト（{data.prompts.length}）
        </Text>
        {data.prompts.length === 0 && <Text dimColor>まだありません</Text>}
        {data.prompts.map(p => (
          <Box key={`p${p.index}`} flexDirection="row" marginBottom={1}>
            <Text dimColor mono>
              {String(p.index).padStart(2, ' ')}.{' '}
            </Text>
            <Box flexDirection="column" flexShrink={1}>
              <Text>{clip(p.text.replace(/\s+/g, ' '), cols * 3)}</Text>
            </Box>
          </Box>
        ))}
      </Box>
    )
  })
}
