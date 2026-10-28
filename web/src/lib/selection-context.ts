"use client";

/// エディタの「選択→AI 指示」の受け渡しストア（Task 11.10・design §4.8.3）。
///
/// 各エディタ（ノート/CSV/スライド）が選択範囲を setPendingSelection で置き、
/// チャットの Composer がチップ表示して送信時に添付・消費する。ページ内の
/// エディタ⇄チャットパネル間の受け渡し専用（このストアは永続化しない・1 件のみ）。
/// パネルを持たない CSV はホームのチャットへ引き継ぎ、ホームで作った会話の最初の発話に
/// 載せるまでの間だけ [`stashPending`](@/lib/pending-message) が sessionStorage に置く。

import * as React from "react";

/// 選択種別（サーバの chat::SelectionKind と対・閉集合）。
export type SelectionKind =
  | "note_selection"
  | "csv_range"
  | "slide_selection"
  | "office_selection"
  /// スキルエディタで編集中のスキル全体（#919・excerpt は save_skill の入力と同じ JSON）。
  | "skill_draft";

/// 選択コンテキスト（サーバの chat::SelectionContext と対）。
export interface SelectionContext {
  kind: SelectionKind;
  node_id?: string | null;
  draft_name?: string | null;
  excerpt: string;
  locator?: unknown;
}

let pending: SelectionContext | null = null;
const listeners = new Set<() => void>();

function emit() {
  for (const l of listeners) l();
}

/// 抜粋の上限（サーバの `SelectionKind::excerpt_max_chars` と同じ値）。スキルは全体を渡す
/// ため大きい（切れた内容を AI が丸ごと書き直すと、切れた後ろが消える）。
function excerptMax(kind: SelectionKind): number {
  return kind === "skill_draft" ? 48_000 : 8_000;
}

/// 選択を積む（既存があれば置き換え・excerpt はクライアント側でも軽く切り詰める）。
export function setPendingSelection(ctx: SelectionContext) {
  pending = { ...ctx, excerpt: ctx.excerpt.slice(0, excerptMax(ctx.kind)) };
  emit();
}

export function clearPendingSelection() {
  pending = null;
  emit();
}

/// 送信時に取り出して消費する（チップの二重送信を防ぐ）。
export function takePendingSelection(): SelectionContext | null {
  const out = pending;
  pending = null;
  emit();
  return out;
}

export function usePendingSelection(): SelectionContext | null {
  return React.useSyncExternalStore(
    (onChange) => {
      listeners.add(onChange);
      return () => listeners.delete(onChange);
    },
    () => pending,
    () => null,
  );
}

/// 種別の表示ラベル。
export function selectionKindLabel(kind: SelectionKind): string {
  switch (kind) {
    case "note_selection":
      return "ノートの選択範囲";
    case "csv_range":
      return "CSV の選択範囲";
    case "slide_selection":
      return "スライドの選択要素";
    case "office_selection":
      return "文書の選択範囲";
    case "skill_draft":
      return "編集中のスキル";
    default:
      return "選択範囲";
  }
}
