"use client";

/// 自分あての未読メンションの件数（ナビのバッジ・メッセージ画面の「メンション」）。
///
/// 画面の中の購読者で 1 本のポーリング（30 秒ごと）を共有し、メンションを既読にしたとき
/// （`notifyMentionsChanged`）・タブに戻ったときはすぐ引き直す。メッセージが使えない構成（503）・
/// 未ログインでは 0 のまま黙る。

import * as React from "react";

import { listMentions } from "@/lib/messaging-api";

const POLL_MS = 30_000;

let count = 0;
const listeners = new Set<() => void>();
let timer: number | null = null;
let inflight = false;
/// 引いている最中に変わったと知らされた（終わったらもう一度引く）。
let again = false;

function emit() {
  for (const l of listeners) l();
}

function load() {
  if (document.visibilityState === "hidden") return;
  if (inflight) {
    again = true;
    return;
  }
  inflight = true;
  listMentions(true)
    .then((r) => {
      if (r.length !== count) {
        count = r.length;
        emit();
      }
    })
    .catch(() => undefined)
    .finally(() => {
      inflight = false;
      if (again) {
        again = false;
        load();
      }
    });
}

function onVisible() {
  if (document.visibilityState === "visible") load();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  if (listeners.size === 1) {
    load();
    timer = window.setInterval(load, POLL_MS);
    document.addEventListener("visibilitychange", onVisible);
  }
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0) {
      if (timer !== null) window.clearInterval(timer);
      timer = null;
      document.removeEventListener("visibilitychange", onVisible);
    }
  };
}

/// 既読にした・届いたなど、件数が変わったかもしれないことを知らせる（すぐ引き直す）。
export function notifyMentionsChanged(): void {
  if (typeof window !== "undefined" && listeners.size > 0) load();
}

export function useUnreadMentions(): number {
  return React.useSyncExternalStore(
    subscribe,
    () => count,
    () => 0,
  );
}
