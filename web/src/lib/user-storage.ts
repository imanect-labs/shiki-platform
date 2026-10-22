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
const listeners = new Set<() => void>();

/// ユーザーのデータを置く localStorage キーかどうか（スコープ付き・旧来の素のキーの両方）。
///
/// ログアウトで消す対象の単一定義。新しくユーザーのデータを localStorage に置くときはここへ足す。
export function isUserDataKey(key: string): boolean {
  return (
    /^shiki\.[a-z]+-drafts\.v1(?:$|@)/.test(key) ||
    key === "shiki.composer.recent-skills" ||
    key.startsWith("shiki.composer.recent-skills@") ||
    key.startsWith("docchat:active:") ||
    // 旧クライアント内チャット（backend 実装前のダミー保存）。今は書かないが残骸を消す。
    key === "shiki:chats:v1"
  );
}

/// `/me` の応答からスコープを確定する（未ログインは null）。変化したときだけ通知する。
export function setStorageScope(me: { tenant_id: string; id: string } | null): void {
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
  return React.useSyncExternalStore(
    subscribeStorageScope,
    () => scope !== null,
    () => false,
  );
}

/// `base` をユーザー単位にしたキー。スコープ未確定なら null（読み書きしない）。
export function scopedStorageKey(base: string): string | null {
  return scope ? `${base}@${scope}` : null;
}

/// スコープ付きキーで読む。旧来の素のキー（#817 以前）に値があれば、今のユーザーへ移して消す。
///
/// 移行は 1 回きり（読んだ時点で素のキーを消す）。ログアウトでも消えるので、共用 PC で前の人の
/// 値を拾うのはアップデート直後の 1 回に限られ、従来（素のキーを全員が共有）より悪くならない。
export function readScoped(base: string): string | null {
  const key = scopedStorageKey(base);
  if (!key || typeof window === "undefined") return null;
  const own = window.localStorage.getItem(key);
  if (own !== null) return own;
  const legacy = window.localStorage.getItem(base);
  if (legacy === null) return null;
  window.localStorage.setItem(key, legacy);
  window.localStorage.removeItem(base);
  return legacy;
}

/// スコープ付きキーへ書く。スコープ未確定なら書かない（false）。
export function writeScoped(base: string, value: string): boolean {
  const key = scopedStorageKey(base);
  if (!key || typeof window === "undefined") return false;
  window.localStorage.setItem(key, value);
  return true;
}

/// ユーザーのデータを置いた localStorage キーを全ユーザー分消し、スコープを解く（ログアウト時）。
///
/// 共用 PC で次の人に残さないため、今のユーザーの分だけでなく旧来の素のキー・他ユーザーの分も消す。
export function clearUserStorage(): void {
  setStorageScope(null);
  if (typeof window === "undefined") return;
  try {
    const keys: string[] = [];
    for (let i = 0; i < window.localStorage.length; i++) {
      const k = window.localStorage.key(i);
      if (k !== null && isUserDataKey(k)) keys.push(k);
    }
    for (const k of keys) window.localStorage.removeItem(k);
  } catch {
    // localStorage 不可（プライベートモード等）なら残っているものも無い。
  }
}
