import type { GhEntry, GhMeta, GhMetaMap, GhRelation, GhStatus, SessionLog } from '../types'

import { key } from './github'

const LABEL: Record<GhStatus, string> = {
  open: '🟢 Open',
  completed: '✅ Closed',
  not_planned: '⚪ Not planned',
  draft: '📝 Draft',
  merged: '🟣 Merged',
  closed: '🔴 Closed',
  unavailable: '❔ 取得できず',
}

const REL: Record<GhRelation, string> = { closes: 'closes', refs: 'refs' }

function clip(s: string, n: number): string {
  const one = s.replace(/\s+/g, ' ').trim()
  return one.length > n ? `${one.slice(0, n - 1)}…` : one
}

function ref(repo: string, number: number, meta: GhMeta | undefined): string {
  const url = meta?.url || `https://github.com/${repo}/issues/${number}`
  return `[#${number}](${url})`
}

function status(meta: GhMeta | undefined): string {
  return meta ? LABEL[meta.status] : '… 取得中'
}

type Rel = { repo: string; number: number; rel: GhRelation; meta: GhMeta | undefined }

/** PR が言及する Issue（PR 同士の言及と、取得できなかった単なる言及は除く） */
function issuesOfPr(pr: GhEntry, meta: GhMetaMap): Rel[] {
  if (pr.number === null) return []
  const m = meta[key(pr.repo, pr.number)]
  if (!m) return []
  return m.links
    .map(l => ({ ...l, meta: meta[key(l.repo, l.number)] }))
    .filter(l => l.meta?.kind !== 'pr')
    .filter(l => !(l.rel === 'refs' && l.meta?.status === 'unavailable'))
}

/** Issue に紐づく PR: セッションで作った PR の言及 ＋ GitHub 上のクローズ予定 PR */
function prsOfIssue(issue: GhEntry, data: SessionLog, meta: GhMetaMap): Rel[] {
  if (issue.number === null) return []
  const out = new Map<string, Rel>()
  for (const pr of data.prs) {
    if (pr.number === null) continue
    const link = meta[key(pr.repo, pr.number)]?.links.find(
      l => l.repo === issue.repo && l.number === issue.number,
    )
    if (link) {
      out.set(key(pr.repo, pr.number), {
        repo: pr.repo,
        number: pr.number,
        rel: link.rel,
        meta: meta[key(pr.repo, pr.number)],
      })
    }
  }
  for (const l of meta[key(issue.repo, issue.number)]?.links ?? []) {
    const k = key(l.repo, l.number)
    const prev = out.get(k)
    out.set(k, {
      repo: l.repo,
      number: l.number,
      rel: prev?.rel === 'closes' || l.rel === 'closes' ? 'closes' : 'refs',
      meta: meta[k],
    })
  }
  return [...out.values()]
}

/** 取得が必要な参照先（セッションの PR が言及する Issue・Issue のクローズ予定 PR） */
export function linkedTargets(data: SessionLog, meta: GhMetaMap): { repo: string; number: number }[] {
  const out: { repo: string; number: number }[] = []
  for (const g of [...data.prs, ...data.issues]) {
    if (g.number === null) continue
    for (const l of meta[key(g.repo, g.number)]?.links ?? []) out.push({ repo: l.repo, number: l.number })
  }
  return out
}

function ghLine(g: GhEntry, meta: GhMetaMap): string {
  const m = g.number === null ? undefined : meta[key(g.repo, g.number)]
  const num = g.number === null ? '#?' : ref(g.repo, g.number, m)
  return `- ${status(m)} **${num}** ${clip(m?.title || g.title, 80)}`
}

function relLine(r: Rel, from: string, arrow: string): string {
  const title = r.meta?.title ? ` ${clip(r.meta.title, 60)}` : ''
  const label = r.repo === from ? ref(r.repo, r.number, r.meta) : `${r.repo}${ref(r.repo, r.number, r.meta)}`
  return `  - ${arrow} ${REL[r.rel]} ${label}${title}（${status(r.meta)}）`
}

export function toMarkdown(data: SessionLog, meta: GhMetaMap): string {
  const prs =
    data.prs.length === 0
      ? '_まだ作成していません_'
      : data.prs
          .map(pr => {
            const rels = issuesOfPr(pr, meta)
            const lines = rels.map(r => relLine(r, pr.repo, '→'))
            return [ghLine(pr, meta), ...(rels.length ? lines : ['  - _関連 Issue なし_'])].join('\n')
          })
          .join('\n')

  const issues =
    data.issues.length === 0
      ? '_まだ作成していません_'
      : data.issues
          .map(issue => {
            const rels = prsOfIssue(issue, data, meta)
            const lines = rels.map(r => relLine(r, issue.repo, '←'))
            return [ghLine(issue, meta), ...(rels.length ? lines : ['  - _対応 PR なし_'])].join('\n')
          })
          .join('\n')

  const prompts =
    data.prompts.length === 0
      ? '_まだありません_'
      : data.prompts.map(p => `${p.index}. ${clip(p.text, 200)}`).join('\n')

  return [
    `### Pull Request（${data.prs.length}）`,
    prs,
    `### Issue（${data.issues.length}）`,
    issues,
    `### プロンプト（${data.prompts.length}）`,
    prompts,
  ].join('\n\n')
}
