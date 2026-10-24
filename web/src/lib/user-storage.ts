/// ユーザー（＋テナント）単位の localStorage 名前空間（#817）。
///
/// 下書き・最近使ったスキル・Office 文書のアクティブ会話は、本文や会話 id を含む**ユーザーのデータ**。
/// 素のキーで保存すると、共用 PC で次にログインした別ユーザー（別テナントを含む）が前の人の
/// 下書きや会話の続きを開けてしまう。そこで
/// - キーに `tenant|user` を付けて人ごとに分け（[`scopedStorageKey`]）、
/// - ログアウト時にこれらのキーを丸ごと消す（[`clearUserStorage`]）。
///
/// スコープは `/me` の応答（[`fetchMe`](@/lib/api)）で確定する。確定前（初回描画〜/me 応答）は
/// キーが無い＝読まない・書かない（メモリ上だけで動く）。確定時に購読者へ通知するので、
/// 確定前に読んだ側は読み直せる（[`subscribeStorageScope`]）。
///
/// サイドバー幅・一覧/グリッド表示などの**画面の好み**（ユーザーのデータを含まない）は対象外。

import * as React from "react";

/// 現在のスコープ（`tenant|user`）。未確定・未ログインは null。
let scope: string | null = null;
/// ログアウトのたびに進める世代。ログアウト前に出した /me の応答でスコープを戻さないために使う。
let generation = 0;
const listeners = new Set<() => void>();

/// ユーザーのデータを置く localStorage / sessionStorage キーかどうか（スコープ付き・旧来の素のキーの両方）。
///
/// ログアウトで消す対象の単一定義。新しくユーザーのデータを Web Storage に置くときはここへ足す。
export function isUserDataKey(key: string): boolean {
  return (
    /^shiki\.[a-z]+-drafts\.v1(?:$|@)/.test(key) ||
    key === "shiki.composer.recent-skills" ||
    key.startsWith("shiki.composer.recent-skills@") ||
    key.startsWith("docchat:active:") ||
    // 入力欄で選んだモデル（sessionStorage・[`model-choice`](@/lib/model-choice)）。次の人の
    // 最初の発話（ホーム・文書のパネル）に前の人の選択を載せない。
    key.startsWith("shiki:model-choice:") ||
    // ホームの入力欄の書きかけ（[`Composer`](@/components/chat/composer) の draftKey）。
    key.startsWith("shiki.composer.draft:") ||
    // 引用箇所の手がかり（文書本文の断片・[`stashCiteLocator`](@/lib/citation-locate)）。
    key.startsWith("shiki:cite:") ||
    // ホームから会話画面へ渡す最初のメッセージ（本文・添付。sessionStorage・
    // [`stashPending`](@/lib/pending-message)）。通常は直後に取り出すが、遷移が途切れると残る。
    key.startsWith("shiki:pending-message:") ||
    // 旧クライアント内チャット（backend 実装前のダミー保存）。今は書かないが残骸を消す。
    key === "shiki:chats:v1"
  );
}

/// 今の世代（[`setStorageScope`] の `startedAt` に渡す。/me を出す前に読む）。
export function storageScopeGeneration(): number {
  return generation;
}

/// `/me` の応答からスコープを確定する（未ログインは null）。変化したときだけ通知する。
///
/// `startedAt` は /me を出したときの世代。その後にログアウト（他タブを含む）があれば、
/// 遅れて届いた応答は捨てる。旧ユーザーの成功応答でスコープを戻さない（消したキーを作り直させない）。
/// 旧い 401 で次の人のスコープを解かない（ログアウト時のスコープ解除は purge 側で済んでいる）。
export function setStorageScope(
  me: { tenant_id: string; id: string } | null,
  startedAt?: number,
): void {
  if (startedAt !== undefined && startedAt !== generation) return;
  // tenant_id は `|` を含まない（サーバの識別子ポリシー）ので `tenant|user` は一意に分解できる。
  const next = me ? `${me.tenant_id}|${me.id}` : null;
  if (next === scope) return;
  scope = next;
  for (const l of listeners) l();
}

/// スコープの確定・変化を購読する。
export function subscribeStorageScope(cb: () => void): () => void {
  listeners.add(cb);
  return () => {
    listeners.delete(cb);
  };
}

/// スコープ（ログイン中のユーザー）が確定しているか。保存値の有無で振る舞いが変わる画面
/// （無ければ新しい会話を作る等）は、これが true になってから読む。
export function useStorageScopeReady(): boolean {
  return React.useSyncExternalStore(subscribeStorageScope, storageScopeReady, () => false);
}

/// スコープが確定しているか（React 外から）。スコープを持たない素のキーへ書く側
/// （引用の手がかり等）は、これが false なら書かない。他タブのログアウトで消した後に、
/// 開いたままの画面の操作で前の人のデータを書き戻させないため。
export function storageScopeReady(): boolean {
  return scope !== null;
}

/// `base` をユーザー単位にしたキー。スコープ未確定なら null（読み書きしない）。
export function scopedStorageKey(base: string): string | null {
  return scope ? `${base}@${scope}` : null;
}

/// スコープ付きキーで読む。旧来の素のキー（#817 以前）は今のユーザーへ移さずに消す。
///
/// 素のキーには持ち主の情報が無く、共用 PC では前の人（別テナントを含む）の値かもしれない。
/// 下書きは会話履歴から復元でき、最近使ったスキル・アクティブ会話は失っても困らないので移行しない。
export function readScoped(base: string): string | null {
  const key = scopedStorageKey(base);
  if (!key || typeof window === "undefined") return null;
  try {
    window.localStorage.removeItem(base);
    return window.localStorage.getItem(key);
  } catch {
    // Web Storage 不可（プライベートモード等）なら保存値は無いものとして扱う。
    return null;
  }
}

/// スコープ付きキーへ書く。スコープ未確定なら書かない（false）。
export function writeScoped(base: string, value: string): boolean {
  const key = scopedStorageKey(base);
  if (!key || typeof window === "undefined") return false;
  window.localStorage.setItem(key, value);
  return true;
}

/// スコープ付きキーを消す。スコープ未確定なら何もしない。
export function removeScoped(base: string): void {
  const key = scopedStorageKey(base);
  if (!key || typeof window === "undefined") return;
  window.localStorage.removeItem(key);
}

/// ログアウトを同じオリジンの他タブへ知らせる localStorage キー（値は時刻・内容は使わない）。
const LOGOUT_SIGNAL_KEY = "shiki:logout-signal";

/// ユーザーのデータを置いた localStorage / sessionStorage キーを全ユーザー分消し、スコープを解く（ログアウト時）。
///
/// 共用 PC で次の人に残さないため、今のユーザーの分だけでなく旧来の素のキー・他ユーザーの分も消す。
/// 他タブにも知らせる: sessionStorage はタブごとで、メモリ上のスコープも残るため、知らせないと
/// 他タブに最初のメッセージ（本文・添付）が残り、そのタブの書込みが消したキーを作り直す。
export function clearUserStorage(): void {
  purgeUserStorage();
  if (typeof window === "undefined") return;
  try {
    // `storage` イベントは書いたタブ以外に届く。
    window.localStorage.setItem(LOGOUT_SIGNAL_KEY, String(Date.now()));
  } catch {
    // Web Storage 不可なら他タブにも残っているものは無い。
  }
}

function purgeUserStorage(): void {
  generation += 1;
  // スコープが既に未確定（/me 待ち）でも購読者へ知らせる。未確定のままメモリ上に作られた下書きを
  // 捨てさせないと、このタブで次の人の /me が成功したときにその人の保存分へ合流してしまう。
  scope = null;
  for (const l of listeners) l();
  if (typeof window === "undefined") return;
  // sessionStorage もログアウト〜次のログインで同じタブに残るので消す。
  for (const storage of ["localStorage", "sessionStorage"] as const) {
    try {
      const store = window[storage];
      const keys: string[] = [];
      for (let i = 0; i < store.length; i++) {
        const k = store.key(i);
        if (k !== null && isUserDataKey(k)) keys.push(k);
      }
      for (const k of keys) store.removeItem(k);
    } catch {
      // Web Storage 不可（プライベートモード等）なら残っているものも無い。
    }
  }
}

// 他タブのログアウトを受けて、このタブのスコープと sessionStorage も消す（知らせ直しはしない）。
if (typeof window !== "undefined") {
  window.addEventListener("storage", (e) => {
    if (e.key === LOGOUT_SIGNAL_KEY && e.newValue !== null) purgeUserStorage();
  });
}
