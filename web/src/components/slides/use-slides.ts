"use client";

/// Yjs ドキュメント（Array "slides"）をリアクティブに読むフック（Task 11.1）。
///
/// Yjs 側の構造はサーバ（crates/collab/src/slide/yjs_doc.rs）と対:
/// Array "slides" の要素 = Map { id: string, html: Y.Text, notes: Y.Text, bg: JSON 文字列 }。

import * as React from "react";
import * as Y from "yjs";

import type { SlideData } from "@/lib/slides-api";

/// Y.Map のスライド 1 枚を素の JS へ読む（クライアント実装差に寛容・壊れた要素は落とす）。
function readSlide(entry: unknown): SlideData | null {
  if (!(entry instanceof Y.Map)) return null;
  const id = entry.get("id");
  if (typeof id !== "string" || id.length === 0) return null;
  const html = entry.get("html");
  const notes = entry.get("notes");
  const bgRaw = entry.get("bg");
  let bg: Record<string, unknown> | null = null;
  if (typeof bgRaw === "string" && bgRaw.length > 0) {
    try {
      const parsed: unknown = JSON.parse(bgRaw);
      if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
        bg = parsed as Record<string, unknown>;
      }
    } catch {
      bg = null;
    }
  }
  return {
    id,
    html: html instanceof Y.Text ? html.toString() : typeof html === "string" ? html : "",
    notes: notes instanceof Y.Text ? notes.toString() : typeof notes === "string" ? notes : "",
    bg,
  };
}

/// スライド列を購読する（deep 変更＝Y.Text の文字編集も再描画に反映）。
export function useSlides(doc: Y.Doc): SlideData[] {
  const array = React.useMemo(() => doc.getArray("slides"), [doc]);
  const subscribe = React.useCallback(
    (onChange: () => void) => {
      array.observeDeep(onChange);
      return () => array.unobserveDeep(onChange);
    },
    [array],
  );
  // スナップショットはキャッシュし、変更通知のたびに読み直す（useSyncExternalStore 規約）。
  const cache = React.useRef<{ version: number; slides: SlideData[] }>({
    version: -1,
    slides: [],
  });
  const versionRef = React.useRef(0);
  const subscribeWithBump = React.useCallback(
    (onChange: () => void) =>
      subscribe(() => {
        versionRef.current += 1;
        onChange();
      }),
    [subscribe],
  );
  const getSnapshot = React.useCallback(() => {
    if (cache.current.version !== versionRef.current) {
      cache.current = {
        version: versionRef.current,
        slides: array
          .toArray()
          .map(readSlide)
          .filter((s): s is SlideData => s !== null),
      };
    }
    return cache.current.slides;
  }, [array]);
  return React.useSyncExternalStore(subscribeWithBump, getSnapshot, getSnapshot);
}
