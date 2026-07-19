"use client";

/// ノートのエクスポート（#334）。md / docx / pdf の 3 形式。
///
/// - **md**: エディタ state を正規化 Markdown へ直列化してダウンロード（クライアント完結）。
/// - **docx**: 本文を Markdown 化し、シキ（genui）コンポーネントは静的化して埋め込む
///   （チャートは PNG スナップショット・iframe/ドライブ参照はプレースホルダ＋リンク）。
///   その Markdown を POST /documents/export へ渡し .docx bytes を受けてダウンロードする。
/// - **pdf**: 専用プリントビュー（/notes/{id}/print）を開き window.print()（呼び出し側）。
///
/// シキコンポーネントの静的化は本モジュールの肝: インタラクティブな genui/iframe は
/// そのままでは docx に載らないため、エクスポート時に静的スナップショット/プレースホルダへ
/// 置き換える（「生 HTML を描画しない」不変条件は維持・埋め込みノードの方針を崩さない）。

import { Fragment } from "@tiptap/pm/model";
import type { Editor } from "@tiptap/react";
import { toPng } from "html-to-image";

import { apiFetch } from "@/lib/api";
import { parseEmbedPayload } from "@/components/notes/embed/types";
import { serializeFragment } from "./markdown-serialize";

/// エディタ内容を正規化 Markdown へ（md エクスポート・genui はフェンスのまま）。
export function noteMarkdown(editor: Editor): string {
  return serializeFragment(editor.state.doc.content);
}

/// Blob を名前つきでダウンロードさせる（スライドの downloadBlob と同型の汎用ユーティリティ）。
export function downloadBlob(blob: Blob, filename: string): void {
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  // 次の tick で revoke（Safari が click 前に revoke すると失敗するため遅延する）。
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/// md をそのままダウンロードする。
export function exportNoteMarkdown(editor: Editor, name: string): void {
  const md = noteMarkdown(editor);
  downloadBlob(new Blob([md], { type: "text/markdown;charset=utf-8" }), `${name}.md`);
}

/// docx 用の Markdown を組み立てる（シキコンポーネントを静的化・#334 の肝）。
///
/// top-level ノードを走査し、`shikiEmbed` は種別で分岐する:
/// - genui → 描画済み DOM を PNG 化し `![タイトル](dataUrl)` の画像行にする（worker が add_picture）。
///   スナップショット失敗時はプレースホルダ行へ縮退（fail-soft）。
/// - iframe/drive → 「この埋め込みはエクスポートに含まれません」＋参照リンクのプレースホルダ。
/// それ以外のノートは通常どおり Markdown へ直列化する。
export async function buildDocxMarkdown(editor: Editor): Promise<string> {
  const parts: string[] = [];
  const doc = editor.state.doc;
  const positions: { node: import("@tiptap/pm/model").Node; pos: number }[] = [];
  doc.forEach((node, offset) => positions.push({ node, pos: offset }));

  for (const { node, pos } of positions) {
    if (node.type.name !== "shikiEmbed") {
      parts.push(serializeFragment(Fragment.from(node)).trimEnd());
      continue;
    }
    const payload = parseEmbedPayload((node.attrs.payload as string) ?? "");
    if (!payload) {
      parts.push("> 表示できない埋め込みはエクスポートに含まれません。");
      continue;
    }
    if (payload.kind === "genui") {
      const title = genuiTitle(payload.spec);
      const dataUrl = await snapshotEmbed(editor, pos);
      parts.push(
        dataUrl
          ? `![${escapeAlt(title)}](${dataUrl})`
          : `> 図「${title}」はエクスポートに含められませんでした。`,
      );
    } else {
      parts.push(embedPlaceholder(payload));
    }
  }
  // トップレベル走査で拾えなかった入れ子（リスト/引用内）の埋め込みは serializeFragment が
  // ```shiki-embed フェンスとして出す。worker はフェンスを解さないため、残ったフェンスを
  // 静的なプレースホルダへ置換して**生の埋め込み JSON を docx に載せない**（Codex 指摘）。
  return staticizeEmbedFences(parts.filter((p) => p.length > 0).join("\n\n"));
}

/// iframe/ドライブ埋め込みの静的プレースホルダ（リンク付き・生 JSON を出さない）。
function embedPlaceholder(payload: NonNullable<ReturnType<typeof parseEmbedPayload>>): string {
  if (payload.kind === "iframe") {
    const label = payload.title ?? payload.src;
    return `> 埋め込み「${label}」はエクスポートに含まれません（${payload.src}）。`;
  }
  if (payload.kind === "drive") {
    const label = payload.name ?? "ドライブのファイル";
    return `> 埋め込みファイル「${label}」はエクスポートに含まれません。`;
  }
  // genui（入れ子でスナップショット位置が取れないケース）はタイトルのみのプレースホルダ。
  return `> 図「${genuiTitle(payload.spec)}」はエクスポートに含められませんでした。`;
}

/// 残存する ```shiki-embed フェンスをプレースホルダへ置換する（入れ子埋め込みの保険）。
function staticizeEmbedFences(md: string): string {
  // 3 個以上のバックティック＋`shiki-embed`、本文、同数のバックティックの閉じ。
  const fence = /^(`{3,})shiki-embed[ \t]*\n([\s\S]*?)\n\1[ \t]*$/gm;
  return md.replace(fence, (_all, _ticks: string, body: string) => {
    const payload = parseEmbedPayload(body.trim());
    return payload
      ? embedPlaceholder(payload)
      : "> 表示できない埋め込みはエクスポートに含まれません。";
  });
}

/// docx をサーバ変換（POST /documents/export）してダウンロードする。
export async function exportNoteDocx(editor: Editor, name: string): Promise<void> {
  const markdown = await buildDocxMarkdown(editor);
  const res = await apiFetch("/documents/export", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name, markdown }),
  });
  if (res.status === 503) {
    throw new Error("文書変換サービスに接続できません。時間をおいて再試行してください (503)");
  }
  if (!res.ok) {
    throw new Error(`docx への変換に失敗しました (${res.status})`);
  }
  downloadBlob(await res.blob(), `${name}.docx`);
}

/// 埋め込みノードの DOM を PNG data URL へスナップショットする（失敗は null＝縮退）。
async function snapshotEmbed(editor: Editor, pos: number): Promise<string | null> {
  try {
    const dom = editor.view.nodeDOM(pos);
    const el = dom instanceof HTMLElement ? dom : null;
    const target = el?.querySelector<HTMLElement>('[data-testid="embed-genui"]') ?? el;
    if (!target) return null;
    return await toPng(target, {
      pixelRatio: 2,
      // 背景を白にする（透過 PNG が docx で黒く沈むのを防ぐ）。
      backgroundColor: "#ffffff",
      // ノート本文のフォント/色を継承（テーマに依存しない静的画像）。
      cacheBust: true,
    });
  } catch {
    return null;
  }
}

/// genui スペックからタイトルらしき文字列を拾う（無ければ既定ラベル）。
function genuiTitle(spec: unknown): string {
  if (spec && typeof spec === "object") {
    const s = spec as { title?: unknown; component?: unknown };
    if (typeof s.title === "string" && s.title.length > 0) return s.title;
    if (typeof s.component === "string") return `${s.component} 図`;
  }
  return "図";
}

/// Markdown 画像 alt の `]` と改行を除去する（画像行を単独行に保つ）。
function escapeAlt(text: string): string {
  return text.replace(/[\]\r\n]/g, " ").trim();
}
