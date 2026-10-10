/// PDF の読み込み（pdf.js・引用箇所のページ表示・#508）。
///
/// 本体はダウンロード URL（短 TTL の presigned GET・発行時に viewer 判定）から取って
/// pdf.js に渡す。pdf.js は重いので、使う画面でだけ動的に読み込む。
/// 同じ版を出典パネルとビューアで続けて開くことが多いので、直近の数件は覚えておく。
import type { PDFDocumentProxy } from "pdfjs-dist";

import { versionDownloadUrl } from "@/lib/storage";

type PdfJs = typeof import("pdfjs-dist");

let lib: Promise<PdfJs> | null = null;

function pdfjs(): Promise<PdfJs> {
  lib ??= import("pdfjs-dist").then((m) => {
    m.GlobalWorkerOptions.workerSrc = new URL("pdfjs-dist/build/pdf.worker.min.mjs", import.meta.url).toString();
    return m;
  });
  return lib;
}

/// 覚えておく文書数（PDF 1 冊ぶんのメモリを長く握らない）。
const KEEP = 3;
const docs = new Map<string, Promise<PDFDocumentProxy>>();

export function loadPdf(fileId: string, version: number): Promise<PDFDocumentProxy> {
  const key = `${fileId}@${version}`;
  const hit = docs.get(key);
  if (hit) {
    // 最近使ったものを末尾へ（古いものから捨てる）。
    docs.delete(key);
    docs.set(key, hit);
    return hit;
  }
  const p = versionDownloadUrl(fileId, version)
    .then((t) => fetch(t.url))
    .then((r) => {
      if (!r.ok) throw new Error(`PDF の取得に失敗しました（${r.status}）`);
      return r.arrayBuffer();
    })
    .then((data) => pdfjs().then((m) => m.getDocument({ data }).promise));
  p.catch(() => docs.delete(key));
  docs.set(key, p);
  // 溢れたものはキャッシュから外すだけにする（destroy しない）。出典パネルやビューアが
  // まだ描いている最中の文書を壊さないため。worker 側の資源は destroy まで残るが、1 回の
  // 利用で開く PDF の数は限られるので許容する（タブを閉じれば解放される）。
  while (docs.size > KEEP) {
    docs.delete(docs.keys().next().value as string);
  }
  return p;
}

/// PDF 座標（ポイント・原点は左下か左上）の枠を、表示上の CSS ボックスへ写す。
export function boxToCss(
  bbox: readonly [number, number, number, number],
  topLeft: boolean,
  pageHeight: number,
  scale: number,
): { left: number; top: number; width: number; height: number } {
  const [l, t, r, b] = bbox;
  const top = topLeft ? t : pageHeight - t;
  const bottom = topLeft ? b : pageHeight - b;
  return {
    left: Math.min(l, r) * scale,
    top: Math.min(top, bottom) * scale,
    width: Math.abs(r - l) * scale,
    height: Math.abs(bottom - top) * scale,
  };
}
