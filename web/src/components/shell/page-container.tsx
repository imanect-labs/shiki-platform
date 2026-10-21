import * as React from "react";

import { cn } from "@/lib/utils";

/// ページ本文の共通コンテナ（最大幅・余白を揃える）。任意で見出し＋説明＋主操作を表示する。
///
/// タイトルバーは廃止したので、ページ名は本文の見出し（h1）として 1 回だけ出す。
/// - スマホ: h1 は 26px で本文の先頭（シェルが浮いたボタンの分の上余白を入れる）。
///   `actions`（作成ボタン等の主ボタン）は見出し行の右に置き、説明は見出しの下に全幅で置く。
/// - PC: 従来どおり見出し＋説明、右に操作（PC のバーにも現在地が出るが、ここは本文の見出し）。
export function PageContainer({
  title,
  description,
  actions,
  className,
  children,
}: {
  title?: string;
  description?: React.ReactNode;
  actions?: React.ReactNode;
  className?: string;
  children: React.ReactNode;
}) {
  return (
    <div className={cn("mx-auto w-full max-w-5xl px-4 pb-8 pt-1 md:px-8 md:py-8", className)}>
      {title ? (
        <div className="mb-5 md:mb-6">
          <div className="flex items-center justify-between gap-3 md:items-start md:gap-4">
            <h1 className="min-w-0 text-[26px] font-bold leading-tight tracking-tight md:text-xl md:font-semibold">
              {title}
            </h1>
            {actions ? <div className="flex shrink-0 items-center gap-2">{actions}</div> : null}
          </div>
          {description ? (
            <p className="mt-1 text-sm text-muted-foreground">{description}</p>
          ) : null}
        </div>
      ) : null}
      {children}
    </div>
  );
}
