/// チャットのモデル選択（#553）。既定は Auto（= `null`・ピルを出さない）。
///
/// 選択はスレッドごとに sessionStorage に置く（タブを閉じれば Auto に戻る）。ホームの入力欄は
/// `home` スコープで持ち、チャットを始めたら新しいスレッドへ移す。送信時は呼び出し側が
/// `readModelChoice(threadId)` で読んで API に載せる（入力欄と送信経路を疎結合に保つ）。

import * as React from "react";

import {
  fetchLlmModels,
  type EffortLevel,
  type LlmModelsResponse,
  type ModelPurpose,
} from "@/lib/admin-ai-api";

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

// 選べるモデルの共有キャッシュ（stale-while-revalidate・見え方（purpose）ごと）。管理者が公開設定を
// 変えると古くなるので、手元の値をすぐ返しつつ、マウント時（一定時間たっていれば）・タブに戻ったとき・
// 同じタブで設定を保存したとき（invalidateLlmModels）に取り直す。古い一覧のまま送ると 400 になり、
// 新しく公開したモデルもリロードまで出ないため。
const STALE_MS = 30_000;
/// `failed` は手元に一覧が無いまま取得に失敗したか（読み込み中と見分ける・ワークフローのモデル欄）。
type ModelsCache = { data: LlmModelsResponse | null; at: number; failed: boolean };
const modelsCaches = new Map<ModelPurpose, ModelsCache>();
const inflights = new Map<ModelPurpose, Promise<void>>();
const modelListeners = new Map<ModelPurpose, Set<() => void>>();

function refreshLlmModels(purpose: ModelPurpose): Promise<void> {
  const running = inflights.get(purpose);
  if (running) return running;
  const next = fetchLlmModels(purpose)
    .then((data) => {
      modelsCaches.set(purpose, { data, at: Date.now(), failed: false });
    })
    // 取れなければ手元の値のまま（次の契機で取り直す）。初回なら null＝選択 UI を出さない。
    .catch(() => {
      if (!modelsCaches.get(purpose)?.data) modelsCaches.set(purpose, { data: null, at: 0, failed: true });
    })
    .finally(() => {
      inflights.delete(purpose);
      modelListeners.get(purpose)?.forEach((l) => l());
    });
  inflights.set(purpose, next);
  return next;
}

/// 選べるモデルの一覧を古いものとして取り直す（管理画面で AI の設定を保存した後に呼ぶ・全部の見え方）。
export function invalidateLlmModels(): void {
  for (const cache of modelsCaches.values()) cache.at = 0;
  for (const [purpose, listeners] of modelListeners) {
    if (listeners.size > 0) void refreshLlmModels(purpose);
  }
}

/// 共有キャッシュを購読し、`read` で読んだ値を返す（`same` なら再描画しない）。
function useModelsCache<T>(
  purpose: ModelPurpose,
  read: (purpose: ModelPurpose) => T,
  same: (a: T, b: T) => boolean,
): T {
  const [value, setValue] = React.useState<T>(() => read(purpose));
  React.useEffect(() => {
    const sync = () =>
      setValue((prev) => {
        const next = read(purpose);
        return same(prev, next) ? prev : next;
      });
    let listeners = modelListeners.get(purpose);
    if (!listeners) modelListeners.set(purpose, (listeners = new Set()));
    listeners.add(sync);
    const refreshIfStale = () => {
      const cache = modelsCaches.get(purpose);
      if (!cache || Date.now() - cache.at > STALE_MS) void refreshLlmModels(purpose);
    };
    // 取り直しを始めてから読む（失敗の後の取り直しの間は読み込み中に見せる）。
    refreshIfStale();
    sync();
    const onVisible = () => {
      if (document.visibilityState === "visible") refreshIfStale();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      listeners.delete(sync);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [purpose, read, same]);
  return value;
}

const readModels = (purpose: ModelPurpose) => modelsCaches.get(purpose)?.data ?? null;

/// チャットで選べるモデル（ページ内で共有する）。取れなければ `null`（選択 UI を出さない）。
export function useLlmModels(): LlmModelsResponse | null {
  return useModelsCache("chat", readModels, Object.is);
}

/// 選べるモデルの一覧の読み込み状態（読み込み中と取得失敗を見分けたい欄向け）。
export type LlmModelsState =
  | { status: "loading"; data: null }
  | { status: "ready"; data: LlmModelsResponse }
  | { status: "error"; data: null };

function readModelsState(purpose: ModelPurpose): LlmModelsState {
  const cache = modelsCaches.get(purpose);
  if (cache?.data) return { status: "ready", data: cache.data };
  // 失敗の後に取り直している間は読み込み中として見せる。
  if (cache?.failed && !inflights.has(purpose)) return { status: "error", data: null };
  return { status: "loading", data: null };
}

const sameModelsState = (a: LlmModelsState, b: LlmModelsState) =>
  a.status === b.status && a.data === b.data;

/// `useLlmModels` と同じ共有キャッシュを、読み込み中・取得失敗・取得済みの別つきで返す
/// （ワークフローの「AI に聞く」のモデル欄は `workflow` の見え方: 読み込み中は Skeleton、失敗なら
/// 自由入力に戻す）。
export function useLlmModelsState(purpose: ModelPurpose = "chat"): LlmModelsState {
  return useModelsCache(purpose, readModelsState, sameModelsState);
}
