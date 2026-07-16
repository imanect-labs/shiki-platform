"use client";

/// 砂箱エディタ ⇄ 親のブリッジ（親側・Task 11.2・PIT-23 と同型の信頼境界）。
///
/// **砂箱発のメッセージは敵対的入力として扱う**: opaque origin の iframe から届く
/// データは型・サイズを検証してからのみ使う（fail-closed・未知メッセージは黙って破棄）。
/// 通信は MessageChannel（handshake で port を iframe へ移譲・第三者へ渡らない）。

import type { SlideData } from "@/lib/slides-api";

/// pptx 変換レポート（砂箱の export.ts と対・PIT-42 の可視化）。
export interface ExportReport {
  slides: number;
  texts: number;
  tables: number;
  images: number;
  shapes: number;
  rasterized: number;
}

/// 親 → 砂箱。
export type HostMessage =
  | { type: "slide:load"; id: string; html: string; editable: boolean }
  | { type: "deck:empty" }
  | { type: "export:run"; slides: SlideData[]; title: string };

/// 砂箱 → 親（検証済み）。
export type SandboxMessage =
  | { type: "ready" }
  | { type: "slide:changed"; id: string; html: string }
  | { type: "export:done"; blob: Blob; report: ExportReport }
  | { type: "export:error"; message: string };

/// HTML ペイロードの上限（暴走・メモリ圧迫の遮断。1 スライドとして十分大きい）。
const MAX_HTML_BYTES = 1_000_000;
/// pptx バイナリの上限（100MB・暴走遮断）。
const MAX_PPTX_BYTES = 100_000_000;

/// レポートの数値フィールドを検証する（非数・負値・異常値は拒否）。
function parseReport(value: unknown): ExportReport | null {
  if (typeof value !== "object" || value === null) return null;
  const r = value as Record<string, unknown>;
  const fields = ["slides", "texts", "tables", "images", "shapes", "rasterized"] as const;
  const out: Partial<Record<(typeof fields)[number], number>> = {};
  for (const f of fields) {
    const v = r[f];
    if (typeof v !== "number" || !Number.isFinite(v) || v < 0 || v > 1_000_000) return null;
    out[f] = Math.floor(v);
  }
  return out as ExportReport;
}

/// 砂箱からの生データを検証する（通らないものは null＝破棄）。
export function parseSandboxMessage(data: unknown): SandboxMessage | null {
  if (typeof data !== "object" || data === null) return null;
  const record = data as Record<string, unknown>;
  switch (record.type) {
    case "ready":
      return { type: "ready" };
    case "slide:changed": {
      const { id, html } = record;
      if (
        typeof id === "string" &&
        id.length > 0 &&
        id.length <= 128 &&
        typeof html === "string" &&
        html.length <= MAX_HTML_BYTES
      ) {
        return { type: "slide:changed", id, html };
      }
      return null;
    }
    case "export:done": {
      const { blob } = record;
      const report = parseReport(record.report);
      if (blob instanceof Blob && blob.size > 0 && blob.size <= MAX_PPTX_BYTES && report) {
        return { type: "export:done", blob, report };
      }
      return null;
    }
    case "export:error": {
      const { message } = record;
      if (typeof message === "string" && message.length <= 2000) {
        return { type: "export:error", message };
      }
      return null;
    }
    default:
      return null;
  }
}

/// iframe へ MessagePort を移譲し、検証済みメッセージを購読するブリッジ。
export class EditorBridge {
  private channel: MessageChannel;
  private closed = false;

  constructor(
    iframe: HTMLIFrameElement,
    private onMessage: (msg: SandboxMessage) => void,
  ) {
    this.channel = new MessageChannel();
    this.channel.port1.onmessage = (ev: MessageEvent) => {
      if (this.closed) return;
      const msg = parseSandboxMessage(ev.data);
      if (msg) this.onMessage(msg);
    };
    // opaque origin のため targetOrigin は "*" だが、**iframe の contentWindow へ直接**
    // port を送るため第三者には渡らない（miniapp-frame.tsx と同じ方針）。
    iframe.contentWindow?.postMessage({ type: "shiki:editor-port" }, "*", [this.channel.port2]);
  }

  send(msg: HostMessage) {
    if (!this.closed) this.channel.port1.postMessage(msg);
  }

  close() {
    this.closed = true;
    this.channel.port1.close();
  }
}
