"use client";

/// 文書内検索の状態（語句・一致・今の一致）。語句は少し待ってから探し始め、ページ順に
/// 読みながら一致を増やす（大きな PDF でも最初の一致をすぐ出す）。
import * as React from "react";
import type { PDFDocumentProxy } from "pdfjs-dist";

import type { PageHighlights } from "@/components/pdf/pdf-page";
import { searchDocument, type PdfMatch } from "@/lib/pdf-text";

/// 打鍵が止まってから探し始めるまでの時間。
const DEBOUNCE_MS = 250;

export function usePdfSearch(doc: PDFDocumentProxy | null, currentPage: number) {
  const [open, setOpen] = React.useState(false);
  const [query, setQuery] = React.useState("");
  const [matches, setMatches] = React.useState<PdfMatch[]>([]);
  const [scanning, setScanning] = React.useState(false);
  const [index, setIndex] = React.useState(-1);
  const currentRef = React.useRef(currentPage);
  currentRef.current = currentPage;

  React.useEffect(() => {
    const q = query.trim();
    setMatches([]);
    setIndex(-1);
    if (!doc || !open || !q) {
      setScanning(false);
      return;
    }
    const ctrl = new AbortController();
    setScanning(true);
    // 今のページ以降で最初の一致を「今の一致」にする（読み終えても無ければ先頭へ）。
    const pick = (found: PdfMatch[], done: boolean) => {
      setIndex((prev) => {
        if (prev >= 0 || found.length === 0) return prev;
        const at = found.findIndex((m) => m.page >= currentRef.current);
        return at >= 0 ? at : done ? 0 : -1;
      });
    };
    const timer = window.setTimeout(() => {
      searchDocument(doc, q, {
        signal: ctrl.signal,
        onProgress: (found) => {
          if (ctrl.signal.aborted) return;
          setMatches(found);
          pick(found, false);
        },
      })
        .then((found) => {
          if (ctrl.signal.aborted) return;
          setMatches(found);
          pick(found, true);
          setScanning(false);
        })
        .catch(() => !ctrl.signal.aborted && setScanning(false));
    }, DEBOUNCE_MS);
    return () => {
      window.clearTimeout(timer);
      ctrl.abort();
    };
  }, [doc, query, open]);

  const step = React.useCallback(
    (delta: number) => {
      if (matches.length === 0) return;
      setIndex((i) => (i < 0 ? 0 : (i + delta + matches.length) % matches.length));
    },
    [matches.length],
  );

  // ページごとの塗り（一致の無いページは undefined のまま・再描画を減らす）。
  const byPage = React.useMemo(() => {
    const map = new Map<number, { ranges: Array<readonly [number, number]>; active: readonly [number, number] | null }>();
    if (!open) return map as Map<number, PageHighlights>;
    const active = matches[index];
    for (const m of matches) {
      let cur = map.get(m.page);
      if (!cur) {
        cur = { ranges: [], active: null };
        map.set(m.page, cur);
      }
      cur.ranges.push([m.start, m.end]);
      if (m === active) cur.active = [m.start, m.end];
    }
    return map as Map<number, PageHighlights>;
  }, [matches, index, open]);

  return {
    open,
    setOpen,
    query,
    setQuery,
    matches,
    scanning,
    index,
    active: matches[index] ?? null,
    next: () => step(1),
    prev: () => step(-1),
    byPage,
  };
}
