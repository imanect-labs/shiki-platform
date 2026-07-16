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

// ── 下書きスライド（ローカル Y.Doc・Task 11.3）────────────────────────────
//
// 下書き画面はサーバ collab に繋がず、正規化スライド JSON（save_slide の content）を
// ローカル Y.Doc へ流し込んで SlideWorkspace を動かす。保存時は Y.Doc から読み戻して
// POST /slides の content にする（サニタイズ・正規化はサーバ側が最終防壁・PIT-40）。

/// 正規化スライド JSON の 1 枚（ワイヤ形式・サーバ collab::slide::Slide と対）。
export type SlideJsonItem = {
  id?: string;
  html?: string;
  notes?: string;
  bg?: Record<string, unknown> | null;
};

/// 正規化スライド JSON 文字列をパースする（fail-closed・version 1 以外は null）。
export function parseSlideDocJson(
  json: string,
): { meta: Record<string, unknown>; slides: SlideJsonItem[] } | null {
  try {
    const parsed: unknown = JSON.parse(json);
    if (!parsed || typeof parsed !== "object") return null;
    const o = parsed as { version?: unknown; meta?: unknown; slides?: unknown };
    if (o.version !== 1) return null;
    const meta =
      o.meta && typeof o.meta === "object" && !Array.isArray(o.meta)
        ? (o.meta as Record<string, unknown>)
        : {};
    const slides = Array.isArray(o.slides) ? (o.slides as SlideJsonItem[]) : [];
    return { meta, slides };
  } catch {
    return null;
  }
}

/// スライド列をローカル Y.Doc へ**全置換**で流し込む（下書きの seed / AI 流し込みの再シード）。
export function seedSlides(doc: Y.Doc, slides: SlideJsonItem[]) {
  doc.transact(() => {
    const arr = slidesArray(doc);
    if (arr.length > 0) arr.delete(0, arr.length);
    const entries = slides.map((s) => {
      const slide = new Y.Map<unknown>();
      slide.set("id", typeof s.id === "string" && s.id ? s.id : crypto.randomUUID());
      const html = new Y.Text();
      html.insert(0, typeof s.html === "string" ? s.html : "");
      slide.set("html", html);
      const notes = new Y.Text();
      notes.insert(0, typeof s.notes === "string" ? s.notes : "");
      slide.set("notes", notes);
      if (s.bg && typeof s.bg === "object") slide.set("bg", JSON.stringify(s.bg));
      return slide;
    });
    if (entries.length > 0) arr.insert(0, entries);
  }, LOCAL_ORIGIN);
}

/// ローカル Y.Doc のスライド列を正規化 JSON のワイヤ形式へ読み戻す（下書き保存用）。
export function readSlidesJson(doc: Y.Doc): SlideJsonItem[] {
  const out: SlideJsonItem[] = [];
  for (const entry of slidesArray(doc)) {
    if (!(entry instanceof Y.Map)) continue;
    const id = entry.get("id");
    if (typeof id !== "string" || id.length === 0) continue;
    const html = entry.get("html");
    const notes = entry.get("notes");
    const bgRaw = entry.get("bg");
    let bg: Record<string, unknown> | undefined;
    if (typeof bgRaw === "string" && bgRaw.length > 0) {
      try {
        const parsed: unknown = JSON.parse(bgRaw);
        if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
          bg = parsed as Record<string, unknown>;
        }
      } catch {
        // 壊れた bg は落とす（描画側も未知キーは無視する）。
      }
    }
    out.push({
      id,
      html: html instanceof Y.Text ? html.toString() : typeof html === "string" ? html : "",
      notes: notes instanceof Y.Text ? notes.toString() : typeof notes === "string" ? notes : "",
      ...(bg ? { bg } : {}),
    });
  }
  return out;
}
