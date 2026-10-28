/// チャットのモデル選択（#553）。既定は Auto（= `null`・ピルを出さない）。
///
/// 選択はスレッドごとに sessionStorage に置く（タブを閉じれば Auto に戻る）。ホームの入力欄は
/// `home` スコープで持ち、チャットを始めたら新しいスレッドへ移す。送信時は呼び出し側が
/// `readModelChoice(threadId)` で読んで API に載せる（入力欄と送信経路を疎結合に保つ）。

import * as React from "react";

import { fetchLlmModels, type EffortLevel, type LlmModelsResponse } from "@/lib/admin-ai-api";

/// `model` が `null` の選択は「組織の既定のモデルのまま、思考の深さだけ変える」（既定が
/// 特定のモデルのときだけ作る。Auto は思考の深さも振り先で決まり、指定は効かない）。
export type ModelChoice = { model: string | null; effort: EffortLevel } | null;

export const HOME_SCOPE = "home";
const KEY = "shiki:model-choice:";
const listeners = new Map<string, Set<() => void>>();

function notify(scope: string) {
  listeners.get(scope)?.forEach((l) => l());
}

export function readModelChoice(scope: string): ModelChoice {
  if (typeof sessionStorage === "undefined") return null;
  try {
    const raw = sessionStorage.getItem(KEY + scope);
    return raw ? (JSON.parse(raw) as ModelChoice) : null;
  } catch {
    return null;
  }
}

export function writeModelChoice(scope: string, choice: ModelChoice): void {
  if (typeof sessionStorage === "undefined") return;
  try {
    if (choice) sessionStorage.setItem(KEY + scope, JSON.stringify(choice));
    else sessionStorage.removeItem(KEY + scope);
  } catch {
    /* 容量超過等は無視（Auto のまま） */
  }
  notify(scope);
}

/// ホームで選んだモデルを新しいスレッドへ移す（ホームは Auto に戻る）。
export function moveModelChoice(from: string, to: string): void {
  const choice = readModelChoice(from);
  if (!choice) return;
  writeModelChoice(to, choice);
  writeModelChoice(from, null);
}

/// スコープのモデル選択を購読する。
export function useModelChoice(scope: string): [ModelChoice, (c: ModelChoice) => void] {
  const subscribe = React.useCallback(
    (cb: () => void) => {
      let set = listeners.get(scope);
      if (!set) listeners.set(scope, (set = new Set()));
      set.add(cb);
      return () => set.delete(cb);
    },
    [scope],
  );
  // sessionStorage の文字列をスナップショットにする（同値なら再描画しない）。
  const raw = React.useSyncExternalStore(
    subscribe,
    () => {
      try {
        return sessionStorage.getItem(KEY + scope);
      } catch {
        return null;
      }
    },
    () => null,
  );
  const choice = React.useMemo<ModelChoice>(() => {
    try {
      return raw ? (JSON.parse(raw) as ModelChoice) : null;
    } catch {
      return null;
    }
  }, [raw]);
  const set = React.useCallback((c: ModelChoice) => writeModelChoice(scope, c), [scope]);
  return [choice, set];
}

// 選べるモデルの共有キャッシュ（stale-while-revalidate）。管理者が公開設定を変えると古くなるので、
// 手元の値をすぐ返しつつ、マウント時（一定時間たっていれば）・タブに戻ったとき・同じタブで設定を
// 保存したとき（invalidateLlmModels）に取り直す。古い一覧のまま送ると 400 になり、新しく公開した
// モデルもリロードまで出ないため。
const STALE_MS = 30_000;
let modelsCache: { data: LlmModelsResponse | null; at: number } | null = null;
let inflight: Promise<void> | null = null;
const modelListeners = new Set<() => void>();

function refreshLlmModels(): Promise<void> {
  inflight ??= fetchLlmModels()
    .then((data) => {
      modelsCache = { data, at: Date.now() };
    })
    // 取れなければ手元の値のまま（次の契機で取り直す）。初回なら null＝選択 UI を出さない。
    .catch(() => {
      modelsCache ??= { data: null, at: 0 };
    })
    .finally(() => {
      inflight = null;
      modelListeners.forEach((l) => l());
    });
  return inflight;
}

/// 選べるモデルの一覧を古いものとして取り直す（管理画面で AI の設定を保存した後に呼ぶ）。
export function invalidateLlmModels(): void {
  if (modelsCache) modelsCache.at = 0;
  if (modelListeners.size > 0) void refreshLlmModels();
}

/// 選べるモデル（ページ内で共有する）。取れなければ `null`（選択 UI を出さない）。
export function useLlmModels(): LlmModelsResponse | null {
  const [data, setData] = React.useState<LlmModelsResponse | null>(modelsCache?.data ?? null);
  React.useEffect(() => {
    const sync = () => setData(modelsCache?.data ?? null);
    modelListeners.add(sync);
    sync();
    const refreshIfStale = () => {
      if (!modelsCache || Date.now() - modelsCache.at > STALE_MS) void refreshLlmModels();
    };
    refreshIfStale();
    const onVisible = () => {
      if (document.visibilityState === "visible") refreshIfStale();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      modelListeners.delete(sync);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, []);
  return data;
}
