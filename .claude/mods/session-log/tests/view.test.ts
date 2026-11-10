import { expect, test } from 'claude-code/testing'

import { key, linksFromBody } from '../hooks/github'
import { toMarkdown } from '../hooks/view'
import type { GhMetaMap, SessionLog } from '../types'

const REPO = 'acme/app'

test('PR 本文から closes と refs を見分ける', async () => {
  const links = linksFromBody('feat: x (#10)\n\nCloses #12\nfixes: #13\n関連 #14\nacme/other#99', 10)
  expect(links).toEqual([
    { number: 12, rel: 'closes' },
    { number: 13, rel: 'closes' },
    { number: 14, rel: 'refs' },
  ])
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
      status: 'merged', links: [{ number: 12, rel: 'closes' }], fetchedAt: 0,
    },
    [key(REPO, 12)]: {
      kind: 'issue', repo: REPO, number: 12, title: '課題', url: 'https://github.com/acme/app/issues/12',
      status: 'completed', links: [{ number: 30, rel: 'closes' }], fetchedAt: 0,
    },
  }
  const md = toMarkdown(data, meta)
  expect(md).toContain('- 🟣 Merged **[#30](https://github.com/acme/app/pull/30)** 修正')
  expect(md).toContain('  - → closes [#12](https://github.com/acme/app/issues/12) 課題（✅ Closed）')
  expect(md).toContain('  - ← closes [#30](https://github.com/acme/app/pull/30) 修正（🟣 Merged）')
  // 未取得の Issue は取得中、PR が無ければその旨
  expect(md).toContain('- … 取得中 **[#20](https://github.com/acme/app/issues/20)** 別件\n  - _対応 PR なし_')
})
