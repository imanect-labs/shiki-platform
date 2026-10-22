/// チャットの図（Yui）を、開いている Excel・Word へ入れるグラフのデータに写す（#731）。
///
/// サーバ（`POST /office/charts`）は項目 × 系列の表と見た目を受け取り、Excel は新しいシートに
/// 表とグラフ、Word は末尾にグラフとして入れる。ここでは Yui の図の形を、その表へ写すだけ。

import type { Figure } from "@imanect/yui";
import { LIGHT } from "@imanect/yui/svg";

import { apiFetch } from "@/lib/api";
import type { components } from "@/generated/api";

export type ChartInsert = components["schemas"]["ChartInsert"];
/// 入れた結果（Excel は表とグラフを置いたシート名）。
export type ChartInsertResult = components["schemas"]["InsertOfficeChartResponse"];

/// 入れる先の文書の種類（拡張子から）。グラフを入れられない形式は null。
export type OfficeTargetKind = "sheet" | "doc";

export function officeTargetKind(fileName: string): OfficeTargetKind | null {
  const ext = fileName.split(".").pop()?.toLowerCase();
  if (ext === "xlsx" || ext === "ods") return "sheet";
  if (ext === "docx" || ext === "odt") return "doc";
  return null;
}

/// 画面と同じ見た目にするため、Yui の明るい配色（系列の 8 色）をそのまま渡す。
const PALETTE = [LIGHT.s1, LIGHT.s2, LIGHT.s3, LIGHT.s4, LIGHT.s5, LIGHT.s6, LIGHT.s7, LIGHT.s8];

/// 棒・折れ線・円の図を、入れるグラフのデータにする（それ以外の図は null）。
export function figureToChartInsert(figure: Figure): ChartInsert | null {
  if (figure.kind !== "bar" && figure.kind !== "line" && figure.kind !== "pie") return null;
  if (figure.rows.length === 0 || figure.columns.length < 2) return null;
  const type = figure.kind === "bar" ? (figure.horizontal ? "bar" : "column") : figure.kind;
  // 円は 1 系列（columns[1]）。棒・折れ線は columns[1..] が系列。
  const names = figure.kind === "pie" ? figure.columns.slice(1, 2) : figure.columns.slice(1);
  return {
    type,
    title: figure.title || null,
    label: figure.columns[0] || null,
    categories: figure.rows.map((r) => r.label),
    series: names.map((name, i) => ({ name, values: figure.rows.map((r) => r.values[i] ?? null) })),
    unit: figure.unit ?? null,
    stacked: figure.kind === "bar" && !!figure.stacked,
    colors: PALETTE,
  };
}

/// 想定内の失敗（理由はそのまま画面に出せる文）。
export class ChartInsertError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ChartInsertError";
  }
}

/// グラフを入れる。全部入るか、何も変えずに失敗するか（サーバ側で丸ごと取り消す）。
export async function insertChart(fileId: string, chart: ChartInsert): Promise<ChartInsertResult> {
  const res = await apiFetch("/office/charts", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ file_id: fileId, chart }),
  });
  if (res.ok) return (await res.json()) as ChartInsertResult;
  if (res.status === 400) {
    const body = (await res.json().catch(() => null)) as { message?: string } | null;
    throw new ChartInsertError(body?.message ?? "このグラフは入れられませんでした");
  }
  if (res.status === 403) throw new ChartInsertError("この文書を編集する権限がありません");
  if (res.status === 409) {
    throw new ChartInsertError("この文書では AI の編集が進んでいます。終わってからもう一度試してください");
  }
  if (res.status === 503) throw new ChartInsertError("Office に接続できませんでした。少し待ってからもう一度試してください");
  throw new Error(`グラフの挿入に失敗しました (${res.status})`);
}
