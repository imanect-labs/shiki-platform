/// ドライブ横断検索の統合レイヤ。
///
/// 検索結果 = **名前一致**（`GET /nodes?q=`・フォルダ/ファイル）∪ **内容一致**
/// （`POST /search`・permission-aware RAG のチャンクをファイル単位へ集約）。
/// どちらもサーバ側で権限フィルタ済み（読めないものは返らない）。
import * as React from "react";

import type { NodeResponse } from "./storage";
import { listChildren } from "./storage";
import { searchDocuments, SearchApiError } from "./search";

/// 内容一致のファイル（チャンクを file 単位に集約した最良ヒット）。
export type ContentHit = {
  fileId: string;
  fileName: string;
  /// ドライブでの遷移先（親フォルダ）。ルート直下は null。
  folderId: string | null;
  /// 最良チャンクのスコア（0..1・rerank 順位由来）。
  score: number;
  /// 最良チャンクの本文（一覧のスニペット表示用）。
  snippet: string;
};

/// 統合結果の 1 行（フォルダ/ファイル混在・スコア降順で表示する）。
export type DriveSearchItem = {
  id: string;
  kind: "folder" | "file";
  name: string;
  /// 選択時の遷移先フォルダ（フォルダ自身 or ファイルの親）。
  targetFolderId: string | null;
  /// 内容一致のときのみ（名前一致のみの行は undefined）。
  snippet?: string;
  score: number;
};

/// 内容一致検索（RAG）。RAG 無効（503）の環境では以後問い合わせず空を返す。
export function useContentSearch(query: string, enabled: boolean) {
  const [hits, setHits] = React.useState<ContentHit[]>([]);
  const [loading, setLoading] = React.useState(false);
  const [disabled, setDisabled] = React.useState(false);
  const seq = React.useRef(0);

  React.useEffect(() => {
    const q = query.trim();
    if (!enabled || !q || disabled) {
      setHits([]);
      setLoading(false);
      return;
    }
    const mySeq = ++seq.current;
    setLoading(true);
    void (async () => {
      try {
        const res = await searchDocuments({ query: q, top_k: 20, mode: "hybrid" });
        if (mySeq !== seq.current) return;
        // チャンク → ファイル集約（最良スコアのチャンクを代表にする）。
        const byFile = new Map<string, ContentHit>();
        for (const r of res.results) {
          const prev = byFile.get(r.file_id);
          if (!prev || r.score > prev.score) {
            byFile.set(r.file_id, {
              fileId: r.file_id,
              fileName: r.file_name,
              folderId: r.folder_id ?? null,
              score: r.score,
              snippet: r.content,
            });
          }
        }
        setHits([...byFile.values()].sort((a, b) => b.score - a.score));
      } catch (e) {
        if (mySeq !== seq.current) return;
        setHits([]);
        if (e instanceof SearchApiError && e.status === 503) setDisabled(true);
      } finally {
        if (mySeq === seq.current) setLoading(false);
      }
    })();
  }, [query, enabled, disabled]);

  return { hits, loading, disabled };
}

/// 名前一致（フォルダ/ファイル横断・先頭 1 ページのみ。パレット等の要約表示用）。
export function useNameSearch(query: string, enabled: boolean, limit = 6) {
  const [nodes, setNodes] = React.useState<NodeResponse[]>([]);
  const [loading, setLoading] = React.useState(false);
  const seq = React.useRef(0);

  React.useEffect(() => {
    const q = query.trim();
    if (!enabled || !q) {
      setNodes([]);
      setLoading(false);
      return;
    }
    const mySeq = ++seq.current;
    setLoading(true);
    void (async () => {
      try {
        const page = await listChildren({ q, limit, sort: "name", desc: false });
        if (mySeq !== seq.current) return;
        setNodes(page.items);
      } catch {
        if (mySeq !== seq.current) return;
        setNodes([]);
      } finally {
        if (mySeq === seq.current) setLoading(false);
      }
    })();
  }, [query, enabled, limit]);

  return { nodes, loading };
}

/// 名前一致＋内容一致をスコア降順の単一リストへ統合する。
///
/// 名前一致はバックエンドがスコアを持たないため 1.0（最上位帯）として扱い、
/// 同一ファイルが両方に出た場合は名前一致の位置を保ちつつスニペットを付ける。
export function mergeSearchResults(
  nodes: NodeResponse[],
  hits: ContentHit[],
  max?: number,
): DriveSearchItem[] {
  const items: DriveSearchItem[] = nodes.map((n) => ({
    id: n.id,
    kind: n.kind === "folder" ? "folder" : "file",
    name: n.name,
    targetFolderId: n.kind === "folder" ? n.id : (n.parent_id ?? null),
    score: 1.0,
  }));
  const seen = new Set(items.map((i) => i.id));
  for (const h of hits) {
    const existing = items.find((i) => i.id === h.fileId);
    if (existing) {
      existing.snippet = h.snippet;
      continue;
    }
    if (seen.has(h.fileId)) continue;
    seen.add(h.fileId);
    items.push({
      id: h.fileId,
      kind: "file",
      name: h.fileName,
      targetFolderId: h.folderId,
      snippet: h.snippet,
      score: h.score,
    });
  }
  items.sort((a, b) => b.score - a.score);
  return max ? items.slice(0, max) : items;
}
