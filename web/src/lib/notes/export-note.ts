"use client";

/// ノートのエクスポート（#334）。md / docx / pdf の 3 形式。
///
/// - **md**: エディタ state を正規化 Markdown へ直列化してダウンロード（クライアント完結）。
/// - **docx**: 本文を Markdown 化し、埋め込みは静的化する（iframe/ドライブ参照は
///   プレースホルダ＋リンク）。
///   その Markdown を POST /documents/export へ渡し .docx bytes を受けてダウンロードする。
/// - **pdf**: 専用プリントビュー（/notes/{id}/print）を開き window.print()（呼び出し側）。
///
/// 埋め込みの静的化は本モジュールの肝: インタラクティブな iframe は
/// そのままでは docx に載らないため、エクスポート時に静的スナップショット/プレースホルダへ
/// 置き換える（「生 HTML を描画しない」不変条件は維持・埋め込みノードの方針を崩さない）。
/// 撤去した旧形式の図（genui・#604）は文言だけのプレースホルダにする。

import { Fragment } from "@tiptap/pm/model";
import type { Editor } from "@tiptap/react";

import { apiFetch } from "@/lib/api";
import { parseEmbedPayload } from "@/components/notes/embed/types";
import { serializeFragment } from "./markdown-serialize";

/// エディタ内容を正規化 Markdown へ（md エクスポート・埋め込みはフェンスのまま）。
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
/// - iframe/drive → 「この埋め込みはエクスポートに含まれません」＋参照リンクのプレースホルダ。
/// - genui（旧形式の図）→ 文言だけのプレースホルダ。
/// それ以外のノートは通常どおり Markdown へ直列化する。
export function buildDocxMarkdown(editor: Editor): string {
  const parts: string[] = [];
  const doc = editor.state.doc;
  const nodes: import("@tiptap/pm/model").Node[] = [];
  doc.forEach((node) => nodes.push(node));

  for (const node of nodes) {
    if (node.type.name !== "shikiEmbed") {
      parts.push(serializeFragment(Fragment.from(node)).trimEnd());
      continue;
    }
    const payload = parseEmbedPayload((node.attrs.payload as string) ?? "");
    if (!payload) {
      parts.push("> 表示できない埋め込みはエクスポートに含まれません。");
      continue;
    }
    parts.push(embedPlaceholder(payload));
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
  return "> 旧形式の図はエクスポートに含められませんでした。";
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
  const markdown = buildDocxMarkdown(editor);
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
