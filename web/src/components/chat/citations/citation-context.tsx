"use client";

/// 引用 UI の共有状態（issue #505）。
///
/// - **メッセージ単位**（`MessageCitationsProvider`）: そのメッセージの引用・本文・文書ごとの
///   まとまり・主張を配る。本文中の番号チップ（Markdown の a レンダラ）はここから引く。
/// - **会話単位**（`SourcePanelProvider`）: 出典パネルの開閉。どのメッセージのどの番号を
///   開いているかを持ち、パネル本体は Conversation が右カラム（またはシート）に描く。
import * as React from "react";

import type { Citation } from "@/lib/chat-api";
import {
  citationRuns,
  claimsByNumber,
  groupCitations,
  type CitationGroups,
  type CitationRun,
  type CitedPassage,
} from "@/lib/citation";
import { isUnresolvable, useNodeMetas, type NodeMeta } from "@/lib/node-name-cache";

export type MessageCitations = {
  /// メッセージの識別（パネルが「同じメッセージを開いているか」を判定する）。
  key: string;
  citations: readonly Citation[];
  groups: CitationGroups;
  runs: CitationRun[];
  claims: Map<number, string>;
  metas: Record<string, NodeMeta>;
  /// 表示名。解決できなければ「読み込み中…」、削除済み・権限なしと確定したら「開けないファイル」。
  nameOf: (nodeId: string) => string;
};

const MessageCitationsContext = React.createContext<MessageCitations | null>(null);

/// 解決済みメタを外から差し込む（/reference のギャラリー用。認証なしで描けるように）。
const MetaOverrideContext = React.createContext<Record<string, NodeMeta> | null>(null);

export function NodeMetaOverride({
  metas,
  children,
}: {
  metas: Record<string, NodeMeta>;
  children: React.ReactNode;
}) {
  return <MetaOverrideContext.Provider value={metas}>{children}</MetaOverrideContext.Provider>;
}

/// 1 ノードの付帯情報（親フォルダ名の表示などに使う）。ギャラリーでは差し込み値を優先する。
export function useNodeMeta(id: string | null | undefined): NodeMeta | undefined {
  const override = React.useContext(MetaOverrideContext);
  const fetched = useNodeMetas(override || !id ? [] : [id]);
  if (!id) return undefined;
  return override ? override[id] : fetched[id];
}

function useCitationMetas(citations: readonly Citation[]): {
  metas: Record<string, NodeMeta>;
  missing: (id: string) => boolean;
} {
  const override = React.useContext(MetaOverrideContext);
  const ids = React.useMemo(
    () => (override ? [] : Array.from(new Set(citations.map((c) => c.node_id)))),
    [citations, override],
  );
  const fetched = useNodeMetas(ids);
  if (override) return { metas: override, missing: (id) => !(id in override) };
  return { metas: fetched, missing: isUnresolvable };
}

export function MessageCitationsProvider({
  messageKey,
  citations,
  text,
  children,
}: {
  messageKey: string;
  citations: readonly Citation[];
  text: string;
  children: React.ReactNode;
}) {
  const { metas, missing } = useCitationMetas(citations);
  const value = React.useMemo<MessageCitations>(() => {
    const runs = citationRuns(text, citations);
    return {
      key: messageKey,
      citations,
      groups: groupCitations(citations, text),
      runs,
      claims: claimsByNumber(runs),
      metas,
      nameOf: (id) => metas[id]?.name ?? (missing(id) ? "開けないファイル" : "読み込み中…"),
    };
  }, [messageKey, citations, text, metas, missing]);

  // 出典パネルが最新の値（解決済みのファイル名など）を引けるよう登録する。
  const registry = React.useContext(SourcePanelContext)?.registry;
  React.useEffect(() => {
    if (!registry) return;
    registry.set(value);
    return () => registry.remove(value);
  }, [registry, value]);

  return <MessageCitationsContext.Provider value={value}>{children}</MessageCitationsContext.Provider>;
}

export function useMessageCitations(): MessageCitations | null {
  return React.useContext(MessageCitationsContext);
}

/* ------------------------------------------------------------------ */
/* 出典パネル                                                          */
/* ------------------------------------------------------------------ */

/// パネルが開いている引用。中身（ファイル名など）は開いた時点で固めず、メッセージ側の
/// 最新値をレジストリから引く。メッセージが消えたら（生成中 → 保存済みへの差し替え等）
/// 開いた時点の写しで表示を続ける。
export type SourcePanelState = { key: string; n: number; snapshot: MessageCitations } | null;

/// メッセージごとの最新の引用文脈（MessageCitationsProvider が登録する）。
type Registry = {
  get: (key: string) => MessageCitations | undefined;
  set: (value: MessageCitations) => void;
  remove: (value: MessageCitations) => void;
  subscribe: (cb: () => void) => () => void;
  version: () => number;
};

function createRegistry(): Registry {
  const map = new Map<string, MessageCitations>();
  const listeners = new Set<() => void>();
  let v = 0;
  const emit = () => {
    v++;
    for (const l of listeners) l();
  };
  return {
    get: (key) => map.get(key),
    set: (value) => {
      if (map.get(value.key) === value) return;
      map.set(value.key, value);
      emit();
    },
    remove: (value) => {
      if (map.get(value.key) !== value) return;
      map.delete(value.key);
      emit();
    },
    subscribe: (cb) => {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    version: () => v,
  };
}

type SourcePanelApi = {
  state: SourcePanelState;
  open: (message: MessageCitations, n: number) => void;
  close: () => void;
  registry: Registry;
};

const SourcePanelContext = React.createContext<SourcePanelApi | null>(null);

export function SourcePanelProvider({ children }: { children: React.ReactNode }) {
  const [state, setState] = React.useState<SourcePanelState>(null);
  const [registry] = React.useState(createRegistry);
  // 閉じたときにフォーカスを戻す先（パネルを開いた番号チップ・一覧の行）。シートは Radix が
  // 戻すが、右カラムはアンマウントされるだけなので自前で戻す。
  const returnFocus = React.useRef<HTMLElement | null>(null);
  const api = React.useMemo<SourcePanelApi>(
    () => ({
      state,
      registry,
      open: (message, n) => {
        if (!state && document.activeElement instanceof HTMLElement) {
          returnFocus.current = document.activeElement;
        }
        setState({ key: message.key, n, snapshot: message });
      },
      close: () => {
        setState(null);
        const el = returnFocus.current;
        returnFocus.current = null;
        if (el?.isConnected) window.requestAnimationFrame(() => el.focus({ preventScroll: true }));
      },
    }),
    [state, registry],
  );
  return <SourcePanelContext.Provider value={api}>{children}</SourcePanelContext.Provider>;
}

/// 出典パネルの操作。Provider の外（ギャラリー等）では null。
export function useSourcePanel(): SourcePanelApi | null {
  return React.useContext(SourcePanelContext);
}

/// パネルが表示すべきメッセージの最新の引用文脈。
export function usePanelMessage(api: SourcePanelApi | null): MessageCitations | null {
  const registry = api?.registry;
  React.useSyncExternalStore(
    registry?.subscribe ?? noopSubscribe,
    registry?.version ?? zero,
    zero,
  );
  if (!api?.state) return null;
  return api.registry.get(api.state.key) ?? api.state.snapshot;
}

const noopSubscribe = () => () => {};
const zero = () => 0;

/// パネルで送る順（本文で使われた引用の番号順。マーカーが無ければ全件）。
export function panelOrder(message: MessageCitations): number[] {
  const { groups } = message;
  const out: number[] = [];
  for (const d of groups.docs) for (const p of d.passages) out.push(p.n);
  return out.sort((a, b) => a - b);
}

/// 番号 n を含む引用箇所。
export function passageOf(message: MessageCitations, n: number): CitedPassage | undefined {
  const { groups } = message;
  for (const d of groups.docs) for (const p of d.passages) if (p.ns.includes(n)) return p;
  return groups.unused.find((p) => p.ns.includes(n));
}
