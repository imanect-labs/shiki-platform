/// スライド Yjs ドキュメントへの書き込みヘルパ（Task 11.2）。
///
/// Yjs 構造はサーバ（crates/collab/src/slide/yjs_doc.rs）と対。ここが**人間編集の
/// 唯一の書き込み口**（エディタ砂箱は HTML を返すだけで Yjs に触れない）。
/// トランザクション origin `LOCAL_ORIGIN` でエコー抑制する（自分の書き込みで
/// エディタを再ロードしない）。

import * as Y from "yjs";

/// 人間のローカル編集トランザクションの origin タグ。
export const LOCAL_ORIGIN = "grapes-local";

function slidesArray(doc: Y.Doc): Y.Array<unknown> {
  return doc.getArray("slides");
}

function findSlide(doc: Y.Doc, id: string): Y.Map<unknown> | null {
  for (const entry of slidesArray(doc)) {
    if (entry instanceof Y.Map && entry.get("id") === id) return entry;
  }
  return null;
}

/// Y.Text を目標文字列へ**単一スプライスの差分**で更新する（全置換で CRDT の
/// マージ粒度を壊さない・共通接頭辞/接尾辞方式）。
function spliceText(text: Y.Text, next: string) {
  const prev = text.toString();
  if (prev === next) return;
  let start = 0;
  const minLen = Math.min(prev.length, next.length);
  while (start < minLen && prev[start] === next[start]) start += 1;
  let endPrev = prev.length;
  let endNext = next.length;
  while (endPrev > start && endNext > start && prev[endPrev - 1] === next[endNext - 1]) {
    endPrev -= 1;
    endNext -= 1;
  }
  if (endPrev > start) text.delete(start, endPrev - start);
  if (endNext > start) text.insert(start, next.slice(start, endNext));
}

/// スライド本文 HTML を更新する（エディタ砂箱からの `slide:changed` の着地点）。
export function updateSlideHtml(doc: Y.Doc, id: string, html: string) {
  doc.transact(() => {
    const slide = findSlide(doc, id);
    if (!slide) return;
    const text = slide.get("html");
    if (text instanceof Y.Text) {
      spliceText(text, html);
    } else {
      const fresh = new Y.Text();
      fresh.insert(0, html);
      slide.set("html", fresh);
    }
  }, LOCAL_ORIGIN);
}

/// スピーカーノートを更新する。
export function updateSlideNotes(doc: Y.Doc, id: string, notes: string) {
  doc.transact(() => {
    const slide = findSlide(doc, id);
    if (!slide) return;
    const text = slide.get("notes");
    if (text instanceof Y.Text) {
      spliceText(text, notes);
    } else {
      const fresh = new Y.Text();
      fresh.insert(0, notes);
      slide.set("notes", fresh);
    }
  }, LOCAL_ORIGIN);
}

/// 新しいスライドを挿入して id を返す（afterId 省略時は末尾）。
export function addSlide(doc: Y.Doc, afterId?: string | null): string {
  const id = crypto.randomUUID();
  doc.transact(() => {
    const arr = slidesArray(doc);
    let index = arr.length;
    if (afterId) {
      const entries = arr.toArray();
      const at = entries.findIndex((e) => e instanceof Y.Map && e.get("id") === afterId);
      if (at >= 0) index = at + 1;
    }
    const slide = new Y.Map<unknown>();
    slide.set("id", id);
    const html = new Y.Text();
    html.insert(0, "<h2>新しいスライド</h2><p>内容を入力</p>");
    slide.set("html", html);
    slide.set("notes", new Y.Text());
    arr.insert(index, [slide]);
  }, LOCAL_ORIGIN);
  return id;
}

/// スライドを削除する。
export function removeSlide(doc: Y.Doc, id: string) {
  doc.transact(() => {
    const arr = slidesArray(doc);
    const entries = arr.toArray();
    const at = entries.findIndex((e) => e instanceof Y.Map && e.get("id") === id);
    if (at >= 0) arr.delete(at, 1);
  }, LOCAL_ORIGIN);
}

/// スライドを前後へ移動する（delta = -1 | +1）。
///
/// Y.Array に move は無いため delete→insert で実現する。並行編集と重なると
/// 稀に複製が起き得るが、収束はする（PIT-41 の契約範囲・並べ替えは低頻度操作）。
export function moveSlide(doc: Y.Doc, id: string, delta: -1 | 1) {
  doc.transact(() => {
    const arr = slidesArray(doc);
    const entries = arr.toArray();
    const at = entries.findIndex((e) => e instanceof Y.Map && e.get("id") === id);
    const to = at + delta;
    if (at < 0 || to < 0 || to >= entries.length) return;
    const entry = entries[at];
    if (!(entry instanceof Y.Map)) return;
    // Yjs の共有型は再挿入できないためクローンして入れ替える。
    const clone = new Y.Map<unknown>();
    for (const [key, value] of entry.entries()) {
      if (value instanceof Y.Text) {
        const t = new Y.Text();
        t.insert(0, value.toString());
        clone.set(key, t);
      } else {
        clone.set(key, value);
      }
    }
    arr.delete(at, 1);
    arr.insert(to, [clone]);
  }, LOCAL_ORIGIN);
}

/// 現在のスライド本文 HTML を読む（エディタ再ロード用）。
export function readSlideHtml(doc: Y.Doc, id: string): string | null {
  const slide = findSlide(doc, id);
  if (!slide) return null;
  const html = slide.get("html");
  return html instanceof Y.Text ? html.toString() : typeof html === "string" ? html : "";
}
