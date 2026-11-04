"use client";

/// 回答本文の組版の比較（デザイン確認用・一時的）。?v=current|a|b|c で案を切り替える。
import * as React from "react";

import { Markdown } from "@/components/prompt-kit/markdown";
import { cn } from "@/lib/utils";

const SAMPLE = `ご質問の「経費精算の締め日と承認フロー」について、社内規程と最新の通達をもとに整理しました。結論から言うと、**月末締め・翌月 5 営業日までに申請**が原則で、金額によって承認者が変わります[1](#cite-1)。

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

const USER_TEXT = "経費精算の締め日と、承認フローを教えて";

type Variant = { id: string; title: string; note: string; className?: string };

/// 案 A〜C の共通: 段落・リスト・見出しの前後に余白をとり、日本語に合う行間にする。
const SPACING =
  "leading-[1.8] [&_li]:leading-[1.8] [&_p]:my-3.5 [&_ul]:my-3.5 [&_ol]:my-3.5 [&_ul]:space-y-1.5 [&_ol]:space-y-1.5 " +
  "[&_h2]:mt-9 [&_h2]:mb-3 [&_h3]:mt-7 [&_h3]:mb-2 [&_div:has(>table)]:my-5 [&_blockquote]:my-5";

const VARIANTS: Record<string, Variant> = {
  current: { id: "current", title: "現状", note: "15px・行間 1.625・段落 10px・見出しの前 16px" },
  a: {
    id: "a",
    title: "案 A: 余白で区切る",
    note: "文字は現状のまま。行間 1.8、段落・リスト・見出しの前後の余白を広げてまとまりを分ける",
    className: SPACING,
  },
  b: {
    id: "b",
    title: "案 B: 見出しで階層をつける",
    note: "案 A ＋ 見出し 2 に下線・大きめ、見出し 3 に色の縦線。本文の色を少し落として太字と見出しを浮かせる",
    className: cn(
      SPACING,
      "text-foreground/85 [&_strong]:text-foreground [&_li]:marker:text-primary/60",
      "[&_h2]:border-b [&_h2]:border-border [&_h2]:pb-2 [&_h2]:text-[18px] [&_h2]:font-bold [&_h2]:text-foreground",
      "[&_h3]:border-l-[3px] [&_h3]:border-primary [&_h3]:pl-2.5 [&_h3]:text-[15px] [&_h3]:leading-snug [&_h3]:text-foreground",
      "[&_blockquote]:rounded-r-lg [&_blockquote]:border-l-[3px] [&_blockquote]:border-amber-500/60 [&_blockquote]:bg-amber-500/5 [&_blockquote]:py-2 [&_blockquote]:pr-3 [&_blockquote]:text-foreground/85",
    ),
  },
  c: {
    id: "c",
    title: "案 C: 読み物型",
    note: "16px・行間 1.9・字間 0.02em・1 行を約 38 字に制限。長文の回答を落ち着いて読ませる",
    className: cn(
      SPACING,
      "max-w-[38em] text-[16px] leading-[1.9] tracking-[0.02em] [&_li]:leading-[1.9] [&_p]:my-4",
      "[&_h2]:mt-10 [&_h2]:text-[19px] [&_h2]:tracking-normal [&_h3]:mt-8 [&_h3]:text-[16.5px] [&_h3]:tracking-normal",
      "[&_table]:text-[14px] [&_table]:tracking-normal",
      "[&_blockquote]:rounded-lg [&_blockquote]:border-l-0 [&_blockquote]:bg-muted/60 [&_blockquote]:px-4 [&_blockquote]:py-3 [&_blockquote]:text-foreground/80",
    ),
  },
};

export default function AnswerTypographyPage() {
  const [v, setV] = React.useState<string>("current");
  React.useEffect(() => {
    const q = new URLSearchParams(window.location.search).get("v");
    if (q && VARIANTS[q]) setV(q);
  }, []);
  const variant = VARIANTS[v] ?? VARIANTS.current!;
  return (
    <div className="min-h-screen bg-background">
      <div className="mx-auto flex w-full max-w-3xl flex-col gap-6 px-4 py-8">
        <header data-testid="variant-label" className="rounded-lg border border-dashed border-border px-3 py-2">
          <p className="text-[13px] font-semibold">{variant.title}</p>
          <p className="text-[12px] text-muted-foreground">{variant.note}</p>
        </header>
        <div className="flex justify-end">
          <div className="max-w-[85%] rounded-2xl bg-secondary px-4 py-2.5 text-[15px] leading-relaxed text-secondary-foreground">
            {USER_TEXT}
          </div>
        </div>
        <div className="text-[15px] leading-relaxed">
          <Markdown className={variant.className}>{SAMPLE}</Markdown>
        </div>
      </div>
    </div>
  );
}
