import type { EngineInterface } from 'claude-code'

import type { GhLink, GhMeta, GhStatus } from '../types'

// 「Closes #12」「fixes: #3」「Resolves owner/repo#4」など、GitHub が自動クローズに使う書き方。
const CLOSES =
  /\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\b\s*:?\s*(?:([\w.-]+\/[\w.-]+))?#(\d+)/gi
// それ以外の「#12」「owner/repo#12」の言及（参照扱い）。
const MENTION = /(?:^|[^\w/&.-])(?:([\w.-]+\/[\w.-]+))?#(\d+)\b/g

export function key(repo: string, number: number): string {
  return `${repo}#${number}`
}

/** PR 本文から他の Issue・PR への関係を拾う（自身は除く。別リポジトリはそのリポジトリの番号として扱う）。 */
export function linksFromBody(body: string, repo: string, self: number): GhLink[] {
  const out = new Map<string, GhLink>()
  const put = (r: string | undefined, n: string | undefined, rel: GhLink['rel']) => {
    const target = r ?? repo
    const number = Number(n)
    if (!Number.isFinite(number) || number <= 0) return
    if (target === repo && number === self) return
    const k = key(target, number)
    if (rel === 'refs' && out.has(k)) return
    out.set(k, { repo: target, number, rel })
  }
  for (const m of body.matchAll(CLOSES)) put(m[1], m[2], 'closes')
  for (const m of body.matchAll(MENTION)) put(m[1], m[2], 'refs')
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
  const sc = unwrap(obj(res.structuredContent))
  if (sc.number !== undefined) return sc
  const text = res.content.map(b => b.text ?? '').join('')
  try {
    return unwrap(obj(JSON.parse(text)))
  } catch {
    return null
  }
}

/** 構造化応答は `{ method: "get", issue: {...} }` のように本体を包むことがあるので取り出す。 */
export function unwrap(j: Record<string, unknown>): Record<string, unknown> {
  if (j.number !== undefined) return j
  for (const k of ['issue', 'pull_request', 'pullRequest']) {
    const inner = obj(j[k])
    if (inner.number !== undefined) return inner
  }
  return j
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

function closedByLinks(j: Record<string, unknown>, repo: string): GhLink[] {
  const refs = obj(j.closed_by_pull_requests).references
  if (!Array.isArray(refs)) return []
  return refs.flatMap(r => {
    const o = obj(r)
    const number = Number(o.number)
    if (!Number.isFinite(number) || number <= 0) return []
    const rr = o.repository
    const named = typeof rr === 'string' ? rr : str(obj(rr).full_name) || str(obj(rr).nameWithOwner)
    return [{ repo: named || repo, number, rel: 'closes' as const }]
  })
}

/** 取得に失敗した参照先。同じ間隔で再試行され、表示では「取得できず」になる。 */
export function unavailable(repo: string, number: number, kind: 'issue' | 'pr'): GhMeta {
  return {
    kind,
    repo,
    number,
    title: '',
    url: `https://github.com/${repo}/issues/${number}`,
    status: 'unavailable',
    links: [],
    fetchedAt: Date.now(),
  }
}

/**
 * Issue と PR はどちらの番号でも issue_read で読める（PR も Issue の一種）。
 * 種類が分からない参照先は issue_read で判定し、PR だったときだけ pull_request_read を足す
 * （merged / draft は pull_request_read にしか無い）。
 */
export async function fetchMeta(
  $: EngineInterface,
  repo: string,
  number: number,
  kind: 'issue' | 'pr' | 'unknown',
): Promise<GhMeta> {
  const [owner, name] = repo.split('/')
  const fallback = unavailable(repo, number, kind === 'pr' ? 'pr' : 'issue')
  if (!owner || !name) return fallback

  if (kind !== 'pr') {
    const j = await callJson($, 'issue_read', {
      method: 'get',
      owner,
      repo: name,
      issue_number: number,
    })
    if (j === null) return fallback
    const isPr = j.pull_request !== undefined || /\/pull\/\d+/.test(str(j.html_url))
    if (!isPr) {
      return {
        kind: 'issue',
        repo,
        number,
        title: str(j.title),
        url: str(j.html_url),
        status: issueStatus(j),
        links: closedByLinks(j, repo),
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
  if (j === null) return { ...fallback, kind: 'pr' }
  return {
    kind: 'pr',
    repo,
    number,
    title: str(j.title),
    url: str(j.html_url),
    status: prStatus(j),
    links: linksFromBody(`${str(j.title)}\n${str(j.body)}`, repo, number),
    fetchedAt: Date.now(),
  }
}
