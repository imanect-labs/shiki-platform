import { expect, test } from 'claude-code/testing'

import { key, linksFromBody, unwrap } from '../hooks/github'
import { ghEntries, mergePrompts } from '../hooks/register'
import { toMarkdown } from '../hooks/view'
import type { GhMetaMap, PromptEntry, SessionLog } from '../types'

const REPO = 'acme/app'

test('PR 本文から closes と refs を見分け、別リポジトリはそのリポジトリの番号として扱う', async () => {
  const links = linksFromBody(
    'feat: x (#10)\n\nCloses #12\nfixes: #13\n関連 #14\nCloses acme/other#4\nsee acme/other#99',
    REPO,
    10,
  )
  expect(links).toEqual([
    { repo: REPO, number: 12, rel: 'closes' },
    { repo: REPO, number: 13, rel: 'closes' },
    { repo: 'acme/other', number: 4, rel: 'closes' },
    { repo: REPO, number: 14, rel: 'refs' },
    { repo: 'acme/other', number: 99, rel: 'refs' },
  ])
})

const p = (...texts: string[]): PromptEntry[] => texts.map((text, i) => ({ index: i + 1, text }))

test('プロンプトは追記し、圧縮で古い分が落ちても消さない', async () => {
  // 普通: 記録が丸ごと残っていて 1 件増えた
  expect(mergePrompts(p('a', 'b'), p('a', 'b', 'c'))).toEqual(p('a', 'b', 'c'))
  // 圧縮後: 先頭が落ち、末尾が重なる
  expect(mergePrompts(p('a', 'b', 'c'), p('c', 'd'))).toEqual(p('a', 'b', 'c', 'd'))
  // 同じ文面を 2 回送っても重複扱いにしない
  expect(mergePrompts(p('ok'), p('ok', 'ok'))).toEqual(p('ok', 'ok'))
})

test('ステータスと Issue⇔PR の関係を両方向に出す', async () => {
  const data: SessionLog = {
    prompts: [],
    issues: [
      { id: 'i1', kind: 'issue', number: 12, title: '課題', url: null, repo: REPO },
      { id: 'i2', kind: 'issue', number: 20, title: '別件', url: null, repo: REPO },
    ],
    prs: [{ id: 'p1', kind: 'pr', number: 30, title: '修正', url: null, repo: REPO }],
  }
  const meta: GhMetaMap = {
    [key(REPO, 30)]: {
      kind: 'pr', repo: REPO, number: 30, title: '修正', url: 'https://github.com/acme/app/pull/30',
      status: 'merged',
      links: [
        { repo: REPO, number: 12, rel: 'closes' },
        { repo: REPO, number: 1, rel: 'refs' },
      ],
      fetchedAt: 0,
    },
    [key(REPO, 12)]: {
      kind: 'issue', repo: REPO, number: 12, title: '課題', url: 'https://github.com/acme/app/issues/12',
      status: 'completed', links: [{ repo: REPO, number: 30, rel: 'closes' }], fetchedAt: 0,
    },
    // 本文の「手順 #1」のような実在しない番号
    [key(REPO, 1)]: {
      kind: 'issue', repo: REPO, number: 1, title: '', url: 'https://github.com/acme/app/issues/1',
      status: 'unavailable', links: [], fetchedAt: 0,
    },
  }
  const md = toMarkdown(data, meta)
  expect(md).toContain('- 🟣 Merged **[#30](https://github.com/acme/app/pull/30)** 修正')
  expect(md).toContain('  - → closes [#12](https://github.com/acme/app/issues/12) 課題（✅ Closed）')
  expect(md).toContain('  - ← closes [#30](https://github.com/acme/app/pull/30) 修正（🟣 Merged）')
  expect(md).not.toContain('#1](')
  // 未取得の Issue は取得中、PR が無ければその旨
  expect(md).toContain('- … 取得中 **[#20](https://github.com/acme/app/issues/20)** 別件\n  - _対応 PR なし_')
})

test('1 回の Bash で作った Issue と PR を両方拾い、grep の引数は作成扱いしない', async () => {
  const out = [
    'https://github.com/acme/app/issues/40',
    'Creating pull request for feat into main',
    'https://github.com/acme/app/pull/41',
  ].join('\n')
  const got = ghEntries('t1', 'Bash', { command: 'gh issue create -t a -b b && gh pr create --fill' }, out)
  expect(got.map(g => [g.kind, g.number])).toEqual([
    ['issue', 40],
    ['pr', 41],
  ])
  expect(ghEntries('t2', 'Bash', { command: 'grep -rn "gh pr create" .' }, out)).toEqual([])
})

test('構造化応答の包み（{ method, issue }）から本体を取り出す', async () => {
  expect(unwrap({ method: 'get', issue: { number: 3, state: 'closed' } })).toEqual({ number: 3, state: 'closed' })
  expect(unwrap({ number: 5 })).toEqual({ number: 5 })
})
