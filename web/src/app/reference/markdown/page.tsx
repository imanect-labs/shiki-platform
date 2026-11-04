"use client";

/// 回答本文の Markdown ギャラリー（デザイン確認・回帰スクショ用・issue #1015）。
///
/// 認証・LLM 不要（middleware は /reference を除外）。実際の `Markdown` に固定の回答を流し、
/// 組版（見出し・余白）・数式・日本語の約物に隣接する太字・引用番号チップの改行を確かめる。
import * as React from "react";

import { Markdown } from "@/components/prompt-kit/markdown";

/// 業務の回答の典型（見出し・箇条書き・表・手順・注意書き・引用番号）。
const BUSINESS = `ご質問の「経費精算の締め日と承認フロー」について、社内規程と最新の通達をもとに整理しました。結論から言うと、**月末締め・翌月 5 営業日までに申請**が原則で、金額によって承認者が変わります[1](#cite-1)。

## 締め日と申請期限

- **締め日**: 毎月末日。月をまたぐ出張は帰着日の月で計上します。
- **申請期限**: 翌月 5 営業日まで。期限を過ぎた申請は部長の理由書が必要です[2](#cite-2)。
- **例外**: 期末月（3 月・9 月）は経理の締めが早まるため、翌月 3 営業日までに短縮されます。

## 承認フロー

金額に応じて承認者が増えます。いずれもワークフロー上で \`経費申請\` テンプレートを使ってください。

| 金額 | 一次承認 | 最終承認 | 目安の日数 |
| --- | --- | --- | --- |
| 5 万円未満 | 課長 | 課長 | 1〜2 日 |
| 5 万円以上 30 万円未満 | 課長 | 部長 | 3 日 |
| 30 万円以上 | 部長 | 経理部長 | 5 日 |

### 申請の手順

1. 領収書を撮影し、ドライブの「経費/2026」フォルダに保存する。
2. ワークフローで申請を作成し、領収書を添付する。
3. 承認後、翌月 25 日の給与と一緒に振り込まれる。

> 注意: 2026 年 10 月の通達で、タクシー代は事前申請が必須になりました[3](#cite-3)。事前申請の無いものは原則として精算できません。

不明点があれば、経理部の窓口（内線 4120）に問い合わせてください。`;

/// 約物（「」（））に隣接する太字。CommonMark の既定では閉じない（#1015 の報告例）。
const CJK_BOLD = `論文の核心である**「具体的にどうやって検索しているのか（メカニズム）」**や**「結論・考察」**についての詳細が不足しておりました。

#### ① 文書構造の階層化（Indexing）

DecisionRAGの決定モデルは、候補となる複数のパスに対して**「どのパスが正解に近いか」という確率分布（スコア）を一度に算出**します。

この研究の最大の意義は、**「検索（Retrieval）」というタスクを、「言語モデルによる分類・選択（Classification/Decision）」というタスクに置き換えた**ことにあります。`;

/// 数式。LLM が書く `\\( \\)`・`\\[ \\]` と `$$`。単独の `$` は金額として残る。
const MATH = `カテゴリをたどる（例：歴史 \\(\\rightarrow\\) 日本史 \\(\\rightarrow\\) 江戸時代）。各ノードでは子 \\(c_i\\) のスコアを softmax で正規化します。

\\[
p(c_i \\mid q) = \\frac{\\exp(s(q, c_i))}{\\sum_{j=1}^{n} \\exp(s(q, c_j))}
\\]

行内の $$E = mc^2$$ と、別行立ての式:

$$
\\mathcal{L} = -\\sum_{t=1}^{T} \\log p_\\theta(y_t \\mid y_{<t}, x)
$$

金額の $5 と $10 は数式にしない。コードの中の \`\\(x\\)\` もそのまま。`;

/// 数式の区切りが構造（リスト・引用・表・コード）の中にある場合。構造を壊さず、コードは原文のまま。
const MATH_EDGE = `- 式は次のとおり:
  \\[
  a^2+b^2
  \\]
- 次の項目

1. 一
2. 式 \\(x^2\\)
3. 三

> \\[E=mc^2\\]
> とする

> \\[
> x = 1
> \\]

| 式 | 値 |
| --- | --- |
| \\(x^2\\) | y |
| z | w |

隣り合う式 \\(a\\)\\(b\\) と、\`\` \`\\(x\\)\` \`\` と、文中の角括弧 \\[注1\\]・\\[config_file\\]。

BRE のグループは \`\\(\` で始める。式 \\(p\\) と \\(q\\) を比べる。PID は $$ で取れる。

> \`\`\`
> code

引用の後の \\(y^2\\)

\`\`\`bash
find . \\\\
  \\( -name '*.log' -o -name '*.tmp' \\) -delete
echo \\(z\\) $$
\`\`\`

大きさの上限 \\(\\rule{500em}{1em}\\) と、箱の外への重ね描き \\(\\rlap{\\text{はみ出す文字はみ出す文字はみ出す文字}}\\)。`;

function Cell({ id, title, children }: { id: string; title: string; children: React.ReactNode }) {
  return (
    <section data-testid={`markdown-${id}`} className="min-w-0">
      <h2 className="mb-3 text-[12px] font-medium text-muted-foreground">{title}</h2>
      {children}
    </section>
  );
}

export default function MarkdownGalleryPage() {
  return (
    <div className="min-h-screen bg-background">
      <div className="mx-auto flex w-full max-w-3xl flex-col gap-12 px-4 py-8">
        <Cell id="business" title="業務の回答（見出し・箇条書き・表・手順・注意書き・引用番号）">
          <Markdown>{BUSINESS}</Markdown>
        </Cell>
        <Cell id="cjk-bold" title="約物に隣接する太字">
          <Markdown>{CJK_BOLD}</Markdown>
        </Cell>
        <Cell id="math" title="数式">
          <Markdown>{MATH}</Markdown>
        </Cell>
        <Cell id="math-edge" title="数式の区切りが構造の中にある場合">
          <Markdown>{MATH_EDGE}</Markdown>
        </Cell>
      </div>
    </div>
  );
}
