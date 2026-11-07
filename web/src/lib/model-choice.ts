/// チャットのモデル選択（#553）。既定は Auto（= `null`・ピルを出さない）。
///
/// 選択はスレッドごとに sessionStorage に置く。タブに選択が無いスレッド（別の端末で送った・タブを
/// 閉じた）は、自分が最後に送った発話のモデル（サーバの記録）を初期値にする。ホームの入力欄は
/// `home` スコープで持ち、チャットを始めたら新しいスレッドへ移す。送信時は呼び出し側が
/// `readModelChoice(threadId)` で読んで API に載せる（入力欄と送信経路を疎結合に保つ）。

import * as React from "react";

import {
  fetchLlmModels,
  type EffortLevel,
  type LlmModelsResponse,
  type ModelModality,
  type ModelPurpose,
} from "@/lib/admin-ai-api";
import { getThread } from "@/lib/chat-api";

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
    // Auto も "null" として残す（タブで選び直した Auto を、サーバの記録で上書きしない）。
    sessionStorage.setItem(KEY + scope, JSON.stringify(choice));
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

function hasModelChoice(scope: string): boolean {
  try {
    return sessionStorage.getItem(KEY + scope) !== null;
  } catch {
    return true; // 読めない環境では何もしない
  }
}

/// サーバの記録（自分が最後に送った発話のモデル）を、このタブにまだ選択が無いときだけ入れる
/// （PC で選んで送った会話をスマホで開いても同じモデルのまま続けられる・#1050）。
/// `undefined` はまだ送っていない（何もしない）。
export function restoreModelChoice(threadId: string, last: ModelChoice | undefined): void {
  if (last === undefined || hasModelChoice(threadId)) return;
  writeModelChoice(threadId, last);
}

/// 送った選択をこのタブに確定させる（記録の取得を待つ間に送ったとき、届いた古い記録で
/// 今送った選択を上書きしない）。
export function pinModelChoice(threadId: string, choice: ModelChoice): void {
  if (!hasModelChoice(threadId)) writeModelChoice(threadId, choice);
}

/// [`restoreModelChoice`] のためにスレッドを読む（スレッドを別に読まない画面＝文書の横のパネル用）。
export function useRestoreModelChoice(threadId: string | null): void {
  React.useEffect(() => {
    if (!threadId || hasModelChoice(threadId)) return;
    let active = true;
    getThread(threadId)
      .then((t) => {
        if (active) restoreModelChoice(threadId, t.lastModelChoice);
      })
      .catch(() => {
        /* 読めなければ既定のまま */
      });
    return () => {
      active = false;
    };
  }, [threadId]);
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

// 選べるモデルの共有キャッシュ（stale-while-revalidate・見え方（purpose と modality の組）ごと）。管理者が公開設定を
// 変えると古くなるので、手元の値をすぐ返しつつ、マウント時（一定時間たっていれば）・タブに戻ったとき・
// 同じタブで設定を保存したとき（invalidateLlmModels）に取り直す。古い一覧のまま送ると 400 になり、
// 新しく公開したモデルもリロードまで出ないため。
const STALE_MS = 30_000;
/// 一覧の見え方（用途とモデルの種類の組）。キャッシュのキーは [`viewKey`]。
type ModelsView = { purpose: ModelPurpose; modality: ModelModality };
const viewKey = (v: ModelsView) => `${v.purpose}|${v.modality}`;
/// `failed` は手元に一覧が無いまま取得に失敗したか（読み込み中と見分ける・ワークフローのモデル欄）。
type ModelsCache = { data: LlmModelsResponse | null; at: number; failed: boolean };
const modelsCaches = new Map<string, ModelsCache>();
const inflights = new Map<string, Promise<void>>();
const modelListeners = new Map<string, { view: ModelsView; listeners: Set<() => void> }>();

function refreshLlmModels(view: ModelsView): Promise<void> {
  const key = viewKey(view);
  const running = inflights.get(key);
  if (running) return running;
  const next = fetchLlmModels(view.purpose, view.modality)
    .then((data) => {
      modelsCaches.set(key, { data, at: Date.now(), failed: false });
    })
    // 取れなければ手元の値のまま（次の契機で取り直す）。初回なら null＝選択 UI を出さない。
    .catch(() => {
      if (!modelsCaches.get(key)?.data) modelsCaches.set(key, { data: null, at: 0, failed: true });
    })
    .finally(() => {
      inflights.delete(key);
      modelListeners.get(key)?.listeners.forEach((l) => l());
    });
  inflights.set(key, next);
  return next;
}

/// 選べるモデルの一覧を古いものとして取り直す（管理画面で AI の設定を保存した後に呼ぶ・全部の見え方）。
export function invalidateLlmModels(): void {
  for (const cache of modelsCaches.values()) cache.at = 0;
  for (const { view, listeners } of modelListeners.values()) {
    if (listeners.size > 0) void refreshLlmModels(view);
  }
}

/// 共有キャッシュを購読し、`read` で読んだ値を返す（`same` なら再描画しない）。
function useModelsCache<T>(
  purpose: ModelPurpose,
  modality: ModelModality,
  read: (key: string) => T,
  same: (a: T, b: T) => boolean,
): T {
  const key = viewKey({ purpose, modality });
  const [value, setValue] = React.useState<T>(() => read(key));
  React.useEffect(() => {
    const view: ModelsView = { purpose, modality };
    const sync = () =>
      setValue((prev) => {
        const next = read(key);
        return same(prev, next) ? prev : next;
      });
    let entry = modelListeners.get(key);
    if (!entry) modelListeners.set(key, (entry = { view, listeners: new Set() }));
    const listeners = entry.listeners;
    listeners.add(sync);
    const refreshIfStale = () => {
      const cache = modelsCaches.get(key);
      if (!cache || Date.now() - cache.at > STALE_MS) void refreshLlmModels(view);
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
  }, [key, purpose, modality, read, same]);
  return value;
}

const readModels = (key: string) => modelsCaches.get(key)?.data ?? null;

/// チャットで選べるモデル（ページ内で共有する）。取れなければ `null`（選択 UI を出さない）。
export function useLlmModels(): LlmModelsResponse | null {
  return useModelsCache("chat", "chat", readModels, Object.is);
}

/// 選べるモデルの一覧の読み込み状態（読み込み中と取得失敗を見分けたい欄向け）。
export type LlmModelsState =
  | { status: "loading"; data: null }
  | { status: "ready"; data: LlmModelsResponse }
  | { status: "error"; data: null };

function readModelsState(key: string): LlmModelsState {
  const cache = modelsCaches.get(key);
  if (cache?.data) return { status: "ready", data: cache.data };
  // 失敗の後に取り直している間は読み込み中として見せる。
  if (cache?.failed && !inflights.has(key)) return { status: "error", data: null };
  return { status: "loading", data: null };
}

const sameModelsState = (a: LlmModelsState, b: LlmModelsState) =>
  a.status === b.status && a.data === b.data;

/// `useLlmModels` と同じ共有キャッシュを、読み込み中・取得失敗・取得済みの別つきで返す
/// （ワークフローの「AI に聞く」のモデル欄は `workflow` の見え方: 読み込み中は Skeleton、失敗なら
/// 自由入力に戻す。「AI で判定」のモデル欄は `modality: "decision"`）。
export function useLlmModelsState(
  purpose: ModelPurpose = "chat",
  modality: ModelModality = "chat",
): LlmModelsState {
  return useModelsCache(purpose, modality, readModelsState, sameModelsState);
}
