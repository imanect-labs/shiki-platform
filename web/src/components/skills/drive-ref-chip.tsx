"use client";

/// ドライブ参照の青いラベル（#919）。スキル本文の driveRef ノードと、知識スコープの
/// フォルダ一覧で共用する。表示は ID ではなく**閲覧者本人の権限で引いたパス**で、
/// 引けないあいだは保存時のラベル、読めないと確定したら「アクセスできない項目」と出す。

import { AlertTriangle, FileText, Folder, X } from "lucide-react";
import * as React from "react";

import type { DriveRefKind } from "@/lib/skills/drive-ref";
import { useNodePath } from "@/lib/skills/node-path";
import { cn } from "@/lib/utils";

/// 青いラベルの配色（チャートの青トークン・ライト/ダーク共通で前景色と混ぜて読める濃さにする）。
const CHIP_TONE =
  "border-[color-mix(in_oklab,var(--chart-1)_45%,transparent)] bg-[color-mix(in_oklab,var(--chart-1)_13%,transparent)] text-[color-mix(in_oklab,var(--chart-1)_78%,var(--foreground))]";
const CHIP_TONE_MISSING = "border-border bg-muted text-muted-foreground";

export function DriveRefChip({
  kind,
  id,
  label,
  selected = false,
  onRemove,
  className,
}: {
  kind: DriveRefKind;
  id: string;
  /// 保存時のラベル（パスの解決前・解決できないときに出す）。
  label?: string;
  /// エディタでノードが選択されている（Backspace で消える状態）。
  selected?: boolean;
  /// 外すボタン（知識スコープの一覧で使う）。
  onRemove?: () => void;
  className?: string;
}) {
  const path = useNodePath(id);
  const missing = path === null;
  const shown = path ?? (label || (kind === "folder" ? "フォルダ" : "ファイル"));
  const Icon = missing ? AlertTriangle : kind === "folder" ? Folder : FileText;
  const title = missing
    ? `アクセスできない${kind === "folder" ? "フォルダ" : "ファイル"}${label ? `（${label}）` : ""}`
    : shown;

  return (
    <span
      className={cn(
        "inline-flex max-w-full items-center gap-1 rounded-md border px-1.5 py-px align-baseline text-[0.9em] font-medium leading-snug",
        missing ? CHIP_TONE_MISSING : CHIP_TONE,
        selected && "ring-2 ring-ring ring-offset-1 ring-offset-background",
        className,
      )}
      title={title}
      data-testid="drive-ref-chip"
      data-kind={kind}
      data-node-id={id}
    >
      <Icon className="size-[1em] shrink-0" aria-hidden />
      <span className={cn("min-w-0 truncate", missing && "line-through decoration-1")}>
        {shown}
      </span>
      {missing ? <span className="sr-only">（アクセスできません）</span> : null}
      {onRemove ? (
        <button
          type="button"
          onClick={onRemove}
          aria-label={`${shown} を外す`}
          className={cn(
            "relative -mr-0.5 ml-0.5 flex size-4 shrink-0 items-center justify-center rounded-sm opacity-70 outline-none",
            "hover:opacity-100 focus-visible:opacity-100 focus-visible:ring-2 focus-visible:ring-ring",
            // 見た目は小さいまま、指では 44px 四方に広げる。
            "pointer-coarse:after:absolute pointer-coarse:after:-inset-3 pointer-coarse:after:content-['']",
          )}
        >
          <X className="size-3" aria-hidden />
        </button>
      ) : null}
    </span>
  );
}
