"use client";

import * as React from "react";

/// クライアント側のチャット永続化（localStorage）。
///
/// チャット backend（Phase 3 / #70）は未実装のため、ここでは issue #68 が要求する
/// 「ダミーデータで本番相当の動作」を満たす最小ストアを提供する。session/message の
/// データ形と公開 API（list/get/create/append…）は backend 差し替え時もそのまま使える
/// よう本番相当に設計し、実装だけ localStorage に閉じている。#70 で fetch/SSE 実装へ
/// 差し替える際は、この関数群のシグネチャを保ったまま中身を置換すればよい。

export type ChatRole = "user" | "assistant";

export type ChatMessage = {
  id: string;
  role: ChatRole;
  content: string;
  /// epoch ミリ秒（new Date は使わず Date.now で統一）。
  createdAt: number;
};

export type ChatSession = {
  id: string;
  title: string;
  createdAt: number;
  updatedAt: number;
  messages: ChatMessage[];
};

const STORAGE_KEY = "shiki:chats:v1";

/// SSR スナップショット用の安定参照（useSyncExternalStore は不変参照を要求する）。
const EMPTY: ChatSession[] = [];

/// 直近で読んだ配列をメモして getSnapshot の参照を安定させる。
/// 書き込み時のみ新しい配列に差し替えるので、無駄な再レンダを避けられる。
let cache: ChatSession[] | null = null;
const listeners = new Set<() => void>();

function isBrowser(): boolean {
  return typeof window !== "undefined";
}

function readFromStorage(): ChatSession[] {
  if (!isBrowser()) return EMPTY;
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [];
    // 最低限の形チェック（壊れた値で UI を落とさない）。
    return parsed.filter(
      (s): s is ChatSession =>
        !!s &&
        typeof s === "object" &&
        typeof (s as ChatSession).id === "string" &&
        Array.isArray((s as ChatSession).messages),
    );
  } catch {
    return [];
  }
}

function getAll(): ChatSession[] {
  if (cache) return cache;
  cache = readFromStorage();
  return cache;
}

function persist(next: ChatSession[]): void {
  cache = next;
  if (isBrowser()) {
    try {
      window.localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
    } catch {
      // 容量超過等は無視（UI は in-memory cache で継続動作する）。
    }
  }
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  // 別タブの変更を取り込む（cache を無効化して次回再読込）。
  const onStorage = (e: StorageEvent) => {
    if (e.key === STORAGE_KEY) {
      cache = null;
      for (const l of listeners) l();
    }
  };
  if (isBrowser()) window.addEventListener("storage", onStorage);
  return () => {
    listeners.delete(listener);
    if (isBrowser()) window.removeEventListener("storage", onStorage);
  };
}

/// 衝突しにくい id（crypto.randomUUID が無い環境はタイムスタンプ＋乱数）。
export function newId(): string {
  if (isBrowser() && "randomUUID" in crypto) return crypto.randomUUID();
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

/// 先頭メッセージからセッションタイトルを作る（長すぎる場合は丸める）。
export function titleFrom(text: string): string {
  const trimmed = text.trim().replace(/\s+/g, " ");
  if (!trimmed) return "新しいチャット";
  return trimmed.length > 40 ? `${trimmed.slice(0, 40)}…` : trimmed;
}

// ── 公開 API（backend 差し替え時もこの形を保つ）─────────────────────

export function listSessions(): ChatSession[] {
  return [...getAll()].sort((a, b) => b.updatedAt - a.updatedAt);
}

export function getSession(id: string): ChatSession | undefined {
  return getAll().find((s) => s.id === id);
}

/// 新しいセッションを作る。最初のユーザーメッセージがあればそれを格納する。
export function createSession(firstUserMessage?: string): ChatSession {
  const now = Date.now();
  const messages: ChatMessage[] = firstUserMessage
    ? [{ id: newId(), role: "user", content: firstUserMessage, createdAt: now }]
    : [];
  const session: ChatSession = {
    id: newId(),
    title: firstUserMessage ? titleFrom(firstUserMessage) : "新しいチャット",
    createdAt: now,
    updatedAt: now,
    messages,
  };
  persist([session, ...getAll()]);
  return session;
}

export function appendMessage(
  sessionId: string,
  role: ChatRole,
  content: string,
): ChatMessage {
  const message: ChatMessage = { id: newId(), role, content, createdAt: Date.now() };
  persist(
    getAll().map((s) =>
      s.id === sessionId
        ? { ...s, updatedAt: message.createdAt, messages: [...s.messages, message] }
        : s,
    ),
  );
  return message;
}

/// 既存メッセージの本文を置換する（ストリーミング中の逐次更新に使う）。
export function updateMessage(sessionId: string, messageId: string, content: string): void {
  persist(
    getAll().map((s) =>
      s.id === sessionId
        ? {
            ...s,
            updatedAt: Date.now(),
            messages: s.messages.map((m) => (m.id === messageId ? { ...m, content } : m)),
          }
        : s,
    ),
  );
}

export function renameSession(sessionId: string, title: string): void {
  const next = title.trim() || "無題のチャット";
  persist(getAll().map((s) => (s.id === sessionId ? { ...s, title: next } : s)));
}

export function deleteSession(sessionId: string): void {
  persist(getAll().filter((s) => s.id !== sessionId));
}

// ── 日付グルーピング（サイドバー履歴・検索モーダルで共用）──────────────

export type DateGroupLabel = "今日" | "昨日" | "過去 7 日間" | "それ以前";

export type ChatSessionGroup = {
  label: DateGroupLabel;
  sessions: ChatSession[];
};

const DAY_MS = 86_400_000;

/// セッション群を「今日 / 昨日 / 過去 7 日間 / それ以前」に分ける。
/// 境界は now のローカル日付の 00:00 起点で判定する（深夜跨ぎでも自然な表示）。
export function groupSessionsByDate(sessions: ChatSession[], now = Date.now()): ChatSessionGroup[] {
  const startOfToday = new Date(now);
  startOfToday.setHours(0, 0, 0, 0);
  const todayStart = startOfToday.getTime();
  const yesterdayStart = todayStart - DAY_MS;
  const weekStart = todayStart - 6 * DAY_MS;

  const buckets: Record<DateGroupLabel, ChatSession[]> = {
    今日: [],
    昨日: [],
    "過去 7 日間": [],
    それ以前: [],
  };

  for (const s of sessions) {
    if (s.updatedAt >= todayStart) buckets["今日"].push(s);
    else if (s.updatedAt >= yesterdayStart) buckets["昨日"].push(s);
    else if (s.updatedAt >= weekStart) buckets["過去 7 日間"].push(s);
    else buckets["それ以前"].push(s);
  }

  const order: DateGroupLabel[] = ["今日", "昨日", "過去 7 日間", "それ以前"];
  return order
    .map((label) => ({ label, sessions: buckets[label] }))
    .filter((g) => g.sessions.length > 0);
}

// ── React 連携 ─────────────────────────────────────────────────

/// 全セッション（updatedAt 降順）を購読する。別タブ・別コンポーネントの変更も反映。
export function useChatSessions(): ChatSession[] {
  const snapshot = React.useSyncExternalStore(
    subscribe,
    getAll,
    () => EMPTY,
  );
  // getAll は未ソートの安定参照。表示用ソートは memo して参照を安定させる。
  return React.useMemo(
    () => [...snapshot].sort((a, b) => b.updatedAt - a.updatedAt),
    [snapshot],
  );
}

/// 単一セッションを購読する（存在しなければ undefined）。
export function useChatSession(id: string | undefined): ChatSession | undefined {
  const snapshot = React.useSyncExternalStore(subscribe, getAll, () => EMPTY);
  return React.useMemo(
    () => (id ? snapshot.find((s) => s.id === id) : undefined),
    [snapshot, id],
  );
}
