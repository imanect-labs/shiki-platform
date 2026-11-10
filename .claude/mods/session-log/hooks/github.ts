import type { EngineInterface } from 'claude-code'

import type { GhLink, GhMeta, GhStatus } from '../types'

// 「Closes #12」「fixes: #3」「Resolves owner/repo#4」など、GitHub が自動クローズに使う書き方。
const CLOSES = /\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\b\s*:?\s*(?:[\w.-]+\/[\w.-]+)?#(\d+)/gi
// それ以外の「#12」の言及（参照扱い）。
const MENTION = /(?:^|[^\w/&])#(\d+)\b/g

export function key(repo: string, number: number): string {
  return `${repo}#${number}`
}

/** PR 本文から Issue への関係を拾う（自身の番号は除く）。 */
export function linksFromBody(body: string, self: number): GhLink[] {
  const out = new Map<number, GhLink>()
  for (const m of body.matchAll(CLOSES)) {
    const n = Number(m[1])
    if (n !== self) out.set(n, { number: n, rel: 'closes' })
  }
  for (const m of body.matchAll(MENTION)) {
    const n = Number(m[1])
    if (n !== self && !out.has(n)) out.set(n, { number: n, rel: 'refs' })
  }
  return [...out.values()]
}

function obj(v: unknown): Record<string, unknown> {
  return typeof v === 'object' && v !== null ? (v as Record<string, unknown>) : {}
}

function str(v: unknown): string {
  return typeof v === 'string' ? v : ''
}

async function callJson(
  $: EngineInterface,
  tool: string,
  args: Record<string, unknown>,
): Promise<Record<string, unknown> | null> {
  const res = await $.mcp.call('github', tool, args)
  if (res.isError) return null
  if (res.structuredContent !== undefined) return obj(res.structuredContent)
  const text = res.content.map(b => b.text ?? '').join('')
  try {
    return obj(JSON.parse(text))
  } catch {
    return null
  }
}

function issueStatus(j: Record<string, unknown>): GhStatus {
  if (str(j.state) !== 'closed') return 'open'
  return str(j.state_reason) === 'not_planned' ? 'not_planned' : 'completed'
}

function prStatus(j: Record<string, unknown>): GhStatus {
  if (j.merged === true || str(j.merged_at) !== '') return 'merged'
  if (str(j.state) === 'closed') return 'closed'
  return j.draft === true ? 'draft' : 'open'
}

/**
 * Issue と PR はどちらの番号でも issue_read で読める（PR も Issue の一種）。
 * kind が分からない参照先は issue_read の `pull_request` 有無で判定する。
 */
export async function fetchMeta(
  $: EngineInterface,
  repo: string,
  number: number,
  kind: 'issue' | 'pr' | 'unknown',
): Promise<GhMeta | null> {
  const [owner, name] = repo.split('/')
  if (!owner || !name) return null

  if (kind !== 'pr') {
    const j = await callJson($, 'issue_read', {
      method: 'get',
      owner,
      repo: name,
      issue_number: number,
    })
    if (j === null) return null
    const isPr = j.pull_request !== undefined || /\/pull\/\d+/.test(str(j.html_url))
    if (!isPr) {
      const closedBy = obj(j.closed_by_pull_requests).references
      const links: GhLink[] = Array.isArray(closedBy)
        ? closedBy
            .map(r => Number(obj(r).number))
            .filter(n => Number.isFinite(n) && n > 0)
            .map(n => ({ number: n, rel: 'closes' as const }))
        : []
      return {
        kind: 'issue',
        repo,
        number,
        title: str(j.title),
        url: str(j.html_url),
        status: issueStatus(j),
        links,
        fetchedAt: Date.now(),
      }
    }
  }

  const j = await callJson($, 'pull_request_read', {
    method: 'get',
    owner,
    repo: name,
    pullNumber: number,
  })
  if (j === null) return null
  const text = `${str(j.title)}\n${str(j.body)}`
  return {
    kind: 'pr',
    repo,
    number,
    title: str(j.title),
    url: str(j.html_url),
    status: prStatus(j),
    links: linksFromBody(text, number),
    fetchedAt: Date.now(),
  }
}
