"use client";

/// 未保存下書きの generic クライアントストア（kind×threadId×name キー・localStorage v1）。
///
/// ノート（issue #282）で確立した「下書き確定型」フローの共通基盤で、notes / slides / csv が
/// kind 別ストアとして共用する。下書きは**新規作成フロー限定**・クライアント内のみ（サーバ同期/
/// awareness なし・1 人で編集）。「ドライブに保存」で初めて StorageService へ実体化する。
///
/// キーは **(threadId, name)**。同じ会話で別名なら別の下書き（N 本並存）、同名なら同じ下書きを
/// 更新（refine・流し込み）。離脱/リロードでも消えないよう localStorage に退避する。
///
/// `rev` / `source` は editor↔store の再帰更新を断ち切るために持つ: AI の流し込み（source=ai）は
/// エディタへ再シードするが、エディタ自身の書き戻し（source=user）は再シードしない。

import * as React from "react";

export type DraftKind = "note" | "slide" | "csv";

export type DraftSource = "ai" | "user";

export type DraftEntry = {
  threadId: string;
  /// 下書き名（拡張子なし・表示/保存名）。
  name: string;
  /// 下書き本文（kind ごとの表現: note=Markdown / slide=正規化スライド JSON / csv=CSV 文字列）。
  content: string;
  /// 単調増加の版。エディタは自分が書いた版を覚えておき、その版の通知では再シードしない。
  rev: number;
  /// 最終更新の出所（ai=流し込み→再シード / user=手編集→再シードしない）。
  source: DraftSource;
  updatedAt: number;
};

export type DraftStore = {
  /// 変更購読（React 外からも使える）。
  subscribe(cb: () => void): () => void;
  /// 下書きを upsert する（同キーは rev+1 で更新）。戻り値は確定エントリ。
  upsert(threadId: string, name: string, content: string, source: DraftSource): DraftEntry;
  get(threadId: string, name: string): DraftEntry | null;
  /// 会話の下書き一覧（更新日昇順＝タブは作られた順）。
  list(threadId: string): DraftEntry[];
  remove(threadId: string, name: string): void;
  /// React 購読フック（会話の下書き一覧・参照安定なスナップショット）。
  useDrafts(threadId: string): DraftEntry[];
};

type PersistedEntry = DraftEntry & {
  /// v1 のノート形式（`markdown` フィールド）からの移行用（読み取り時に content へ寄せる）。
  markdown?: string;
};

const EMPTY: DraftEntry[] = [];

/// kind 別の下書きストアを作る。localStorage キーは `shiki.<kind>-drafts.v1`
/// （note はリファクタ前の `shiki.note-drafts.v1` と同一＝既存の下書きを引き継ぐ）。
export function createDraftStore(kind: DraftKind): DraftStore {
  const storageKey = `shiki.${kind}-drafts.v1`;
  const keyOf = (threadId: string, name: string) => `${threadId} ${name}`;

  type Store = Record<string, DraftEntry>;
  let cache: Store | null = null;
  const listeners = new Set<() => void>();

  function read(): Store {
    if (cache) return cache;
    if (typeof window === "undefined") return {};
    try {
      const raw = window.localStorage.getItem(storageKey);
      const parsed = raw ? (JSON.parse(raw) as Record<string, PersistedEntry>) : {};
      // 旧ノート形式（markdown フィールド）を content へ移行する（読み取り時に一度だけ）。
      for (const entry of Object.values(parsed)) {
        if (typeof entry.content !== "string" && typeof entry.markdown === "string") {
          entry.content = entry.markdown;
          delete entry.markdown;
        }
      }
      cache = parsed as Store;
    } catch {
      cache = {};
    }
    return cache;
  }

  function write(next: Store): void {
    cache = next;
    if (typeof window !== "undefined") {
      try {
        window.localStorage.setItem(storageKey, JSON.stringify(next));
      } catch {
        // localStorage 不可（プライベートモード等）でもメモリキャッシュで機能させる。
      }
    }
    for (const l of listeners) l();
  }

  function list(threadId: string): DraftEntry[] {
    return Object.values(read())
      .filter((d) => d.threadId === threadId)
      .sort((a, b) => a.updatedAt - b.updatedAt);
  }

  // スレッドごとの直近スナップショットをメモ化し、無変更時は同参照を返す（無限再レンダー回避）。
  const snapCache = new Map<string, { key: string; list: DraftEntry[] }>();
  function snapshotFor(threadId: string): DraftEntry[] {
    const entries = list(threadId);
    const key = entries.map((d) => `${d.name}:${d.rev}`).join("|");
    const cached = snapCache.get(threadId);
    if (cached && cached.key === key) return cached.list;
    snapCache.set(threadId, { key, list: entries });
    return entries;
  }

  const subscribe = (cb: () => void): (() => void) => {
    listeners.add(cb);
    return () => listeners.delete(cb);
  };

  return {
    subscribe,
    upsert(threadId, name, content, source) {
      const store = { ...read() };
      const k = keyOf(threadId, name);
      const prev = store[k];
      const entry: DraftEntry = {
        threadId,
        name,
        content,
        rev: (prev?.rev ?? 0) + 1,
        source,
        updatedAt: Date.now(),
      };
      store[k] = entry;
      write(store);
      return entry;
    },
    get(threadId, name) {
      return read()[keyOf(threadId, name)] ?? null;
    },
    list,
    remove(threadId, name) {
      const store = { ...read() };
      delete store[keyOf(threadId, name)];
      write(store);
    },
    useDrafts(threadId) {
      const sub = React.useCallback((cb: () => void) => subscribe(cb), []);
      const getSnapshot = React.useCallback(() => snapshotFor(threadId), [threadId]);
      return React.useSyncExternalStore(sub, getSnapshot, () => EMPTY);
    },
  };
}

/// 下書きイベント/ブロックの payload（`{name, <field>}`）を厳格に検証する（fail-closed）。
/// note=`markdown` / slide=`content` / csv=`csv` のフィールド名差をここで吸収する。
export function parseDraftPayload(
  raw: unknown,
  field: string,
): { name: string; content: string } | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Record<string, unknown>;
  const content = o[field];
  if (typeof o.name !== "string" || o.name.length === 0) return null;
  if (typeof content !== "string") return null;
  return { name: o.name, content };
}
