"use client";

/// 自動実行の起動を見送り続けていることの表示（GAP-67）。
///
/// スケジュールや、ファイル・テーブルの変更で起動するはずの実行を、サーバーが見送ったとき（定義を読めない・オンにした
/// 人の権限が外れた・オンにしている版に実行できない設定がある）に出す。理由と直し方の文はサーバーが作る（内部の文や
/// ほかの人の情報を含まない）。自動実行が動くか、オンにし直す・オフにすると消える。

import { AlertTriangle } from "lucide-react";

import { cn } from "@/lib/utils";
import { formatDateTime } from "@/lib/format";
import type { LaunchSkip } from "@/lib/workflow-api";

/// いつから何回見送ったか（例: 2026/11/02 09:00 から 3 回見送りました。最後は 2026/11/02 11:00 です。）。
function launchSkipWhen(skip: LaunchSkip): string {
  if (skip.count <= 1) return `${formatDateTime(skip.lastAt)} に見送りました。`;
  return `${formatDateTime(skip.firstAt)} から ${skip.count.toLocaleString("ja-JP")} 回見送りました。最後は ${formatDateTime(skip.lastAt)} です。`;
}

/// 自動実行の設定のダイアログの先頭に出す枠（理由といつから何回か）。
///
/// 直し方は、ダイアログの操作（オンにし直す）で直るものでは言わない。オンにしている版に実行できない設定がある
/// （`not_ready`）ときは、エディターで直して保存しないと直らないので、サーバーの直し方の文をそのまま出す（PC では
/// バッジのツールチップを読まずにダイアログを開ける）。
export function LaunchSkipNotice({ skip, className }: { skip: LaunchSkip; className?: string }) {
  return (
    <div
      role="status"
      data-testid="launch-skip-notice"
      className={cn("flex items-start gap-2 rounded-md border p-2.5 text-xs text-foreground/80", className)}
      style={{
        borderColor: "color-mix(in oklab, var(--season-autumn) 45%, transparent)",
        backgroundColor: "color-mix(in oklab, var(--season-autumn) 10%, transparent)",
      }}
    >
      <AlertTriangle className="mt-0.5 size-4 shrink-0" aria-hidden />
      <div className="min-w-0 space-y-0.5 leading-relaxed">
        <p className="font-medium text-foreground">自動実行を見送っています</p>
        <p>{skip.message}</p>
        {skip.code === "not_ready" ? <p data-testid="launch-skip-fix">{skip.fix}</p> : null}
        <p className="text-muted-foreground">{launchSkipWhen(skip)}</p>
      </div>
    </div>
  );
}
