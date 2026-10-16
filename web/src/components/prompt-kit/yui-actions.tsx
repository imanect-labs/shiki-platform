// 回答の本文の ```ui（Yui）のボタン（app・ask・plan）を会話につなぐ（#604）。
//
// ボタンの id はブロックのソースの SHA-256 の先頭 16 桁と図の中の id（a1 など）を合わせた
// `{hash}:{a1}`。サーバー（crates/yui の block_hash）と同じ規則で求める。押せるか・何を送るかの
// 判断はサーバーが保存済みの本文から読み直して行うので、ここは値を送って結果を出すだけ。
"use client";

import * as React from "react";
import type { AppActionEvent, AppActionResult } from "@imanect/yui/react";
import type { Figure } from "@imanect/yui";
import { CheckCircle2, CircleDashed, Loader2, MinusCircle, XCircle } from "lucide-react";

import {
  invokeChatYuiAction,
  UiActionAlreadyInvoked,
} from "@/lib/artifact-api";
import type { UiActionRun } from "@/lib/chat-api";
import { cn } from "@/lib/utils";

/// 図のボタンを実行できる場所（確定した回答のメッセージ）。無ければボタンは無効（表示だけ）。
export type YuiActionHost = {
  threadId: string;
  messageId: string;
  /// 押せるワークフローのボタン（回答の確定時にサーバーが束縛を解決できたもの）。
  boundActions: readonly string[];
  /// 送り済みの 1 回だけのボタン（会話に送るボタン）。
  invokedActions: readonly string[];
  /// 起動したワークフローの記録。
  uiRuns: readonly UiActionRun[];
  /// 生成直後の仮のメッセージ（id がサーバーのものではないので、読み直すまで押せない）。
  /// `reloading` はすぐ読み直される、`stopped`（生成の停止）は読み直さない。
  unsaved?: "reloading" | "stopped";
  /// 実行の後に会話を読み直す（新しい発話・起動の記録を出す）。
  onChanged: () => void;
};

export const YuiActionContext = React.createContext<YuiActionHost | null>(null);

/// 引用・箇条書きの中か。操作を持つ図はトップレベルだけ押せる（サーバーも入れ子の図は読まない）。
export const YuiNestedContext = React.createContext(false);

const ACTION_KINDS = new Set(["app", "ask", "plan"]);

/// ブロックの識別子（CRLF を LF にし、末尾の空白を落としたソースの SHA-256 の先頭 16 桁）。
export async function blockHash(source: string): Promise<string> {
  const normalized = source.replace(/\r\n/g, "\n").trimEnd();
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(normalized));
  return Array.from(new Uint8Array(digest).slice(0, 8), (b) => b.toString(16).padStart(2, "0")).join("");
}

/// 冪等キー（送信ごとに作る。同じ送信の再送でワークフローを二度起動しない）。
function newIdempotencyKey(): string {
  return typeof crypto.randomUUID === "function"
    ? crypto.randomUUID()
    : Array.from(crypto.getRandomValues(new Uint8Array(16)), (b) => b.toString(16).padStart(2, "0")).join("");
}

export type YuiActionProps = {
  onAction?: (e: AppActionEvent) => Promise<AppActionResult>;
  actionDisabledReason?: string;
  invokedActions?: readonly string[];
  /// このブロックのボタンで起動したワークフロー（新しいものが後ろ）。
  runs: readonly UiActionRun[];
};

/// 図に渡すボタンの props。操作を持たない種類は `null`。
export function useYuiActions(figure: Figure | null, source: string, complete: boolean): YuiActionProps | null {
  const host = React.useContext(YuiActionContext);
  const nested = React.useContext(YuiNestedContext);
  const actionable = figure != null && ACTION_KINDS.has(figure.kind);
  const usable = actionable && host != null && !nested && complete;
  const [hash, setHash] = React.useState<string | null>(null);
  React.useEffect(() => {
    if (!usable) return;
    let live = true;
    blockHash(source).then(
      (h) => live && setHash(h),
      () => live && setHash(null),
    );
    return () => {
      live = false;
    };
  }, [usable, source]);

  const onAction = React.useCallback(
    async (e: AppActionEvent): Promise<AppActionResult> => {
      if (!host || host.unsaved || !hash) return { ok: false, message: "いまは実行できません" };
      const key = `${hash}:${e.actionId}`;
      const target = e.action.target;
      if (target.kind === "workflow" && !host.boundActions.includes(key)) {
        return { ok: false, message: `ワークフロー「${target.name}」が見つからないか、使う権限がありません` };
      }
      try {
        await invokeChatYuiAction(
          host.threadId,
          host.messageId,
          key,
          e.values as Record<string, unknown>,
          target.kind === "workflow" ? newIdempotencyKey() : undefined,
        );
        host.onChanged();
        return {
          ok: true,
          message: target.kind === "workflow" ? `ワークフロー「${target.name}」を起動しました` : "送信しました",
        };
      } catch (err) {
        if (err instanceof UiActionAlreadyInvoked) {
          // 送り済み（別のタブ・二度押し）。会話を読み直して送信済みの表示に揃える。
          host.onChanged();
          return { ok: false, message: target.kind === "workflow" ? "起動の途中です" : "送信済みです" };
        }
        return { ok: false, message: err instanceof Error ? err.message : "実行できませんでした" };
      }
    },
    [host, hash],
  );

  if (!actionable) return null;
  if (!host) return { actionDisabledReason: "この場所ではボタンを使えません", runs: [] };
  if (nested) return { actionDisabledReason: "引用や箇条書きの中の図のボタンは使えません", runs: [] };
  if (!complete) return { actionDisabledReason: "回答の生成が終わると使えます", runs: [] };
  if (host.unsaved === "stopped") {
    return { actionDisabledReason: "生成を止めた回答のボタンは、ページを読み直すと使えます", runs: [] };
  }
  if (host.unsaved || !hash) return { actionDisabledReason: "準備しています…", runs: [] };
  const prefix = `${hash}:`;
  return {
    onAction,
    invokedActions: host.invokedActions.filter((a) => a.startsWith(prefix)).map((a) => a.slice(prefix.length)),
    runs: host.uiRuns.filter((r) => r.action_id.startsWith(prefix)),
  };
}

const STATUS: Record<UiActionRun["status"], { label: string; icon: React.ElementType; tone: string; spin?: boolean }> = {
  starting: { label: "起動中", icon: Loader2, tone: "text-muted-foreground", spin: true },
  queued: { label: "待ち", icon: CircleDashed, tone: "text-muted-foreground" },
  running: { label: "実行中", icon: Loader2, tone: "text-muted-foreground", spin: true },
  succeeded: { label: "成功", icon: CheckCircle2, tone: "text-primary" },
  failed: { label: "失敗", icon: XCircle, tone: "text-destructive" },
  cancelled: { label: "取り消し", icon: MinusCircle, tone: "text-muted-foreground" },
  skipped: { label: "見送り（同時実行の上限）", icon: MinusCircle, tone: "text-muted-foreground" },
};

function time(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime())
    ? ""
    : d.toLocaleString("ja-JP", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

/// 図のボタンで起動したワークフローの記録（図の下・新しいものが下）。
export function YuiRunList({ runs }: { runs: readonly UiActionRun[] }) {
  if (runs.length === 0) return null;
  return (
    <ul className="mt-2 space-y-1 text-[12px]" aria-label="起動したワークフロー" data-testid="yui-runs">
      {runs.map((r) => {
        const s = STATUS[r.status];
        const Icon = s.icon;
        const values = (r.values ?? []).map((v) => `${v.label}: ${v.value}`).join("・");
        return (
          <li
            key={`${r.action_id}-${r.invoked_at}-${r.run_id ?? ""}`}
            className="flex min-w-0 items-start gap-2 rounded-lg border border-border bg-secondary/30 px-2.5 py-1.5"
            data-status={r.status}
          >
            <Icon className={cn("mt-0.5 size-3.5 shrink-0", s.tone, s.spin && "animate-spin")} aria-hidden />
            <div className="min-w-0 flex-1">
              <div className="flex flex-wrap items-baseline gap-x-2">
                <span className="font-medium text-foreground">
                  {r.label} → {r.workflow_name}
                </span>
                <span className={s.tone}>{s.label}</span>
                <span className="text-muted-foreground">{time(r.invoked_at)}</span>
              </div>
              {values ? <p className="truncate text-muted-foreground" title={values}>{values}</p> : null}
            </div>
          </li>
        );
      })}
    </ul>
  );
}

/// 本文に ```ui（Yui）のブロックがあるか。生成の後にメッセージを読み直す（ボタンの束縛と id を得る）かの判断に使う。
export function hasYuiBlock(text: string): boolean {
  return /^[ \t]*(?:>[ \t]*)*(?:`{3,}|~{3,})[ \t]*y?ui\b/m.test(text);
}

/// まだ終わっていない起動があるか（あれば会話を少しずつ読み直して状態を更新する）。
export function hasOpenRuns(runs: readonly UiActionRun[] | undefined): boolean {
  return (runs ?? []).some((r) => r.status === "starting" || r.status === "queued" || r.status === "running");
}
