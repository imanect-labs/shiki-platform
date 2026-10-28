/// ユーザー設定（設定 → チャット／表示・#952）。アカウント単位でサーバに保存する（`/me/preferences`）。
///
/// ページ内で 1 つの値を共有する（入力欄・思考過程・日時の表示などが同じ値を見る）。取得前・取得失敗・
/// chat 無効（503）では既定値で動き、画面を止めない。変更は楽観的に反映してから 1 件ずつ順に保存し、
/// 失敗したら最後にサーバで確かめた値へ戻す。別タブで変えた分は、タブに戻ったとき（一定時間たって
/// いれば）取り直す。
/// ログアウト（他タブを含む）で捨て、次の人の値を取り直す（前の人の設定で動かさない）。
///
/// 型は OpenAPI(utoipa) からの生成物（正本は `chat::preferences`）。

import * as React from "react";

import type { components } from "@/generated/api";
import { ApiError } from "@/lib/admin-ai-api";
import { apiFetch } from "@/lib/api";
import { storageScopeReady, subscribeStorageScope } from "@/lib/user-storage";

type Body = components["schemas"]["UserPreferencesBody"];
type PrefsResponse = components["schemas"]["UserPreferencesResponse"];
export type SubmitKey = components["schemas"]["SubmitKey"];
export type TimeFormat = components["schemas"]["TimeFormat"];
export type DefaultModelChoice = components["schemas"]["DefaultModelChoice"];

/// 欠けたフィールドを既定値で埋めた形（生成型では既定値のある項目は任意）。
export type UserPreferences = {
  submit_key: SubmitKey;
  default_model: DefaultModelChoice | null;
  web_search_by_default: boolean;
  default_autonomous_mode: NonNullable<Body["default_autonomous_mode"]>;
  expand_reasoning: boolean;
  time_format: TimeFormat;
};

export const DEFAULT_PREFERENCES: UserPreferences = {
  submit_key: "enter",
  default_model: null,
  web_search_by_default: false,
  default_autonomous_mode: "require_approval",
  expand_reasoning: false,
  time_format: "absolute",
};

function prefsOf(b: Body): UserPreferences {
  return {
    submit_key: b.submit_key ?? DEFAULT_PREFERENCES.submit_key,
    default_model: b.default_model ?? null,
    web_search_by_default: b.web_search_by_default ?? false,
    default_autonomous_mode: b.default_autonomous_mode ?? DEFAULT_PREFERENCES.default_autonomous_mode,
    expand_reasoning: b.expand_reasoning ?? false,
    time_format: b.time_format ?? DEFAULT_PREFERENCES.time_format,
  };
}

export type PreferencesState = {
  prefs: UserPreferences;
  /// サーバの値を読めたか（false の間は既定値）。
  loaded: boolean;
  /// 組織ポリシで全自動（bypass）を選べるか。
  bypassAllowed: boolean;
  /// 読めなかった理由（chat 無効・通信失敗など）。読めていれば null。
  error: string | null;
};

const INITIAL: PreferencesState = {
  prefs: DEFAULT_PREFERENCES,
  loaded: false,
  bypassAllowed: true,
  error: null,
};

/// 取り直すまでの猶予（タブに戻ったとき）。
const STALE_MS = 30_000;

let state: PreferencesState = INITIAL;
let fetchedAt = 0;
let inflight: { epoch: number; writes: number; promise: Promise<void> } | null = null;
/// 進めると、それより前に出した取得・保存の応答を捨てる（ログアウト）。
let epoch = 0;
/// 始めた保存の数。取得の応答は、取得を始めた後に保存を始めていれば捨てる（保存より古い値で
/// 画面と確認済みの値を戻さない）。
let writes = 0;
/// 最後にサーバで確かめた値。
let confirmed: UserPreferences = DEFAULT_PREFERENCES;
/// まだサーバで確かめていない変更（出した順）。画面の値は「確認済みの値 ＋ これらを順に重ねたもの」。
/// 失敗したら、その変更だけを外す（後から重ねた変更は残して、それぞれの保存で送る）。
let unsaved: { id: number; patch: Partial<UserPreferences> }[] = [];
let nextId = 0;
/// 保存を 1 件ずつ順に送るための列（同時に送ると後着の古い全体で上書きされうる）。
let queue: Promise<void> = Promise.resolve();
const listeners = new Set<() => void>();

function setState(next: PreferencesState) {
  // 取り直しで同じ値が返ったときは知らせない（一覧の行・メッセージごとに購読しているので、
  // 新しいオブジェクトに差し替えるだけで全部が描き直しになる）。
  if (
    next.loaded === state.loaded &&
    next.bypassAllowed === state.bypassAllowed &&
    next.error === state.error &&
    JSON.stringify(next.prefs) === JSON.stringify(state.prefs)
  ) {
    return;
  }
  state = next;
  listeners.forEach((l) => l());
}

/// 画面の値（確認済みの値に、まだ確かめていない変更を出した順に重ねる）。
function localPrefs(): UserPreferences {
  return unsaved.reduce<UserPreferences>((p, u) => ({ ...p, ...u.patch }), confirmed);
}

/// サーバの値を確認済みの値として採り、まだ確かめていない変更を重ね直す。
function applyResponse(body: PrefsResponse) {
  fetchedAt = Date.now();
  confirmed = prefsOf(body.preferences);
  setState({ prefs: localPrefs(), loaded: true, bypassAllowed: body.bypass_allowed, error: null });
}

async function readError(res: globalThis.Response): Promise<ApiError> {
  let detail = "";
  try {
    detail = ((await res.json()) as { detail?: string }).detail ?? "";
  } catch {
    // 本文が JSON でない
  }
  const fallback =
    res.status === 503
      ? "この環境ではチャットが有効になっていないため、この設定は使えません"
      : `エラーが発生しました（${res.status}）`;
  return new ApiError(res.status, detail || fallback);
}

function refresh(): Promise<void> {
  // 同じ世代・保存を挟んでいない取得が走っていれば相乗りする（それ以外は応答を捨てるので待たない）。
  if (inflight && inflight.epoch === epoch && inflight.writes === writes) return inflight.promise;
  const startedEpoch = epoch;
  const startedWrites = writes;
  const entry = { epoch: startedEpoch, writes: startedWrites, promise: Promise.resolve() };
  // 応答を採ってよいか（ログアウト・保存を挟んでおらず、保存待ちも無い）。
  const current = () => startedEpoch === epoch && startedWrites === writes && unsaved.length === 0;
  entry.promise = (async () => {
    try {
      const res = await apiFetch("/me/preferences");
      if (!res.ok) throw await readError(res);
      const body = (await res.json()) as PrefsResponse;
      if (current()) applyResponse(body);
    } catch (e) {
      if (!current()) return;
      // 読めなくても既定値（または手元の値）のまま動く（次の契機で取り直す）。
      fetchedAt = Date.now();
      setState({ ...state, error: e instanceof Error ? e.message : String(e) });
    } finally {
      if (inflight === entry) inflight = null;
    }
  })();
  inflight = entry;
  return entry.promise;
}

// ログアウト（他タブを含む）で捨てる（出ている取得・保存の応答も捨てる）。次の人のログインで
// スコープが確定したら取り直す。別のユーザーへの切替は必ずログアウト（スコープの解除）を挟む。
subscribeStorageScope(() => {
  if (!storageScopeReady()) {
    epoch += 1;
    fetchedAt = 0;
    confirmed = DEFAULT_PREFERENCES;
    unsaved = [];
    setState(INITIAL);
  } else if (!state.loaded && listeners.size > 0) {
    void refresh();
  }
});

/// 設定を変える（楽観的に反映してから保存する）。失敗したらその変更だけを戻し、例外を投げる。
///
/// 保存は 1 件ずつ順に送り、送る時点の画面の値（後から重ねた変更も含む）を送る。前の保存が後の変更も
/// 含めて成功していれば、後の保存は送らない。前の保存が失敗しても、後の変更は残って自分の保存で送る。
export function updatePreferences(patch: Partial<UserPreferences>): Promise<void> {
  // 読めていない間に保存すると、既定値でサーバの設定を上書きしてしまう。
  if (!state.loaded) return Promise.reject(new Error("設定を読み込めていないため保存できません"));
  const id = ++nextId;
  unsaved.push({ id, patch });
  writes += 1;
  setState({ ...state, prefs: localPrefs() });
  const started = epoch;
  const run = queue
    .then(async () => {
      if (started !== epoch) throw new Error("ログアウトしたため保存しませんでした");
      // 前の保存がこの変更も含めて済ませていれば送らない。
      if (!unsaved.some((u) => u.id === id)) return;
      const sentUpTo = nextId;
      const res = await apiFetch("/me/preferences", {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(localPrefs()),
      });
      if (!res.ok) throw await readError(res);
      const body = (await res.json()) as PrefsResponse;
      if (started !== epoch) return;
      // 送った値に含まれていた変更は確かめ済み。後から重ねた変更は残す。
      unsaved = unsaved.filter((u) => u.id > sentUpTo);
      applyResponse(body);
    })
    .catch((e: unknown) => {
      // 失敗した変更だけを外す（画面が保存済みに見えたまま、次の保存で意図せず送られないように）。
      if (started === epoch) {
        unsaved = unsaved.filter((u) => u.id !== id);
        setState({ ...state, prefs: localPrefs() });
      }
      throw e;
    });
  queue = run.catch(() => undefined);
  return run;
}

/// サーバの値を取り直す（読み込みの失敗からの再試行）。
export function reloadPreferences(): Promise<void> {
  return refresh();
}

function refreshIfStale() {
  if (Date.now() - fetchedAt > STALE_MS) void refresh();
}

function onVisible() {
  if (document.visibilityState === "visible") refreshIfStale();
}

/// 最初の購読者が来たら取得し、タブに戻ったときの取り直しを 1 つだけ登録する
/// （思考過程の表示などメッセージごとに購読するので、購読者ごとには登録しない）。
function subscribe(cb: () => void): () => void {
  if (listeners.size === 0) document.addEventListener("visibilitychange", onVisible);
  listeners.add(cb);
  refreshIfStale();
  return () => {
    listeners.delete(cb);
    if (listeners.size === 0) document.removeEventListener("visibilitychange", onVisible);
  };
}

/// ユーザー設定（ページ内で共有する）。初回の購読・タブに戻ったとき（一定時間たっていれば）取り直す。
export function usePreferences(): PreferencesState {
  return React.useSyncExternalStore(
    subscribe,
    () => state,
    () => INITIAL,
  );
}
