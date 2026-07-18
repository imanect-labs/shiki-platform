"use client";

/// エディタの「選択→AI 指示」の受け渡しストア（Task 11.10・design §4.8.3）。
///
/// 各エディタ（ノート/CSV/スライド）が選択範囲を setPendingSelection で置き、
/// チャットの Composer がチップ表示して送信時に添付・消費する。ページ内の
/// エディタ⇄チャットパネル間の受け渡し専用（永続化しない・1 件のみ）。

import * as React from "react";

/// 選択種別（サーバの chat::SelectionKind と対・閉集合）。
export type SelectionKind = "note_selection" | "csv_range" | "slide_selection";

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

/// 選択を積む（既存があれば置き換え・excerpt はクライアント側でも軽く切り詰める）。
export function setPendingSelection(ctx: SelectionContext) {
  pending = { ...ctx, excerpt: ctx.excerpt.slice(0, 8_000) };
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
    default:
      return "選択範囲";
  }
}
