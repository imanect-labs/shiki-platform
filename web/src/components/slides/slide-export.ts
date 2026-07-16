"use client";

/// pptx エクスポートの実行（親側・Task 11.4）。
///
/// 変換は**砂箱内**（apps オリジン・opaque でない別オリジン・builtin バンドル）で行う —
/// 計測は「HTML を描画する」行為なので、アプリオリジンでは行わない（PIT-40 の配置原則）。
/// 本モジュールは隠し iframe を一時マウントし、Yjs から読んだスライドを送って
/// bytes（Blob）とレポートを受け取る。エディタの表示状態に依存しない（viewer でも使える）。

import type * as Y from "yjs";

import {
  EditorBridge,
  type ExportReport,
} from "@/components/slides/editor-bridge";
import { b1Origin } from "@/lib/miniapp-b1-api";
import type { SlideData } from "@/lib/slides-api";

/// エクスポート全体のタイムアウト（大きいデッキ・ラスタライズ多数でも収まる余裕）。
const EXPORT_TIMEOUT_MS = 60_000;

/// Y.Doc からスライド列を読む（use-slides と同じ構造・非フック版）。
export function readSlidesForExport(doc: Y.Doc): SlideData[] {
  const array = doc.getArray("slides");
  const out: SlideData[] = [];
  for (const entry of array.toArray()) {
    const map = entry as { get?: (k: string) => unknown };
    if (typeof map.get !== "function") continue;
    const id = map.get("id");
    if (typeof id !== "string") continue;
    const html = map.get("html");
    const notes = map.get("notes");
    const bgRaw = map.get("bg");
    let bg: Record<string, unknown> | null = null;
    if (typeof bgRaw === "string") {
      try {
        const parsed: unknown = JSON.parse(bgRaw);
        if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
          bg = parsed as Record<string, unknown>;
        }
      } catch {
        bg = null;
      }
    }
    out.push({
      id,
      html: html?.toString() ?? "",
      notes: notes?.toString() ?? "",
      bg,
    });
  }
  return out;
}

/// 隠し砂箱 iframe で pptx を生成する。
export function exportDeckToPptx(
  slides: SlideData[],
  title: string,
): Promise<{ blob: Blob; report: ExportReport }> {
  return new Promise((resolve, reject) => {
    const iframe = document.createElement("iframe");
    iframe.setAttribute("sandbox", "allow-scripts allow-same-origin");
    iframe.style.cssText = "position:fixed;left:-99999px;width:1400px;height:900px;border:0;";
    iframe.title = "pptx エクスポート";
    let bridge: EditorBridge | null = null;
    let done = false;

    const cleanup = () => {
      bridge?.close();
      iframe.remove();
    };
    const timer = window.setTimeout(() => {
      if (done) return;
      done = true;
      cleanup();
      reject(new Error("エクスポートがタイムアウトしました"));
    }, EXPORT_TIMEOUT_MS);

    iframe.addEventListener("load", () => {
      bridge = new EditorBridge(iframe, (msg) => {
        switch (msg.type) {
          case "ready":
            bridge?.send({ type: "export:run", slides, title });
            break;
          case "export:done":
            if (done) return;
            done = true;
            window.clearTimeout(timer);
            cleanup();
            resolve({ blob: msg.blob, report: msg.report });
            break;
          case "export:error":
            if (done) return;
            done = true;
            window.clearTimeout(timer);
            cleanup();
            reject(new Error(msg.message));
            break;
          default:
        }
      });
    });
    document.body.appendChild(iframe);
    iframe.src = `${b1Origin()}/builtin/slide-editor`;
  });
}

/// Blob をブラウザのダウンロードとして保存する。
export function downloadBlob(blob: Blob, filename: string) {
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  // revoke は少し遅らせる（Safari 系のダウンロード開始前解放を避ける）。
  window.setTimeout(() => URL.revokeObjectURL(url), 10_000);
}
