"use client";

/// ミニアプリの画面の表（```data_grid）の実行先（Task 15.5）。
///
/// Markdown（会話・ノートでも使う）から参照するので、グリッド本体（glide-data-grid）を読み込まない
/// 軽い部品に分ける。表の描画は必要になったときだけ遅延読み込みする（`MiniAppDataGrid`）。

import * as React from "react";

import { runMiniAppDataGrid, type DataViewRun } from "@/lib/data-api";

/// 画面の表を描くための実行先（ミニアプリの画面だけが提供する）。
export type DataGridHost = {
  run: (alias: string, page: { cursor?: string; limit?: number }) => Promise<DataViewRun>;
};

export const DataGridHostContext = React.createContext<DataGridHost | null>(null);

/// ミニアプリ（id・版）向けの実行先。
export function miniAppDataGridHost(appId: string, version: number): DataGridHost {
  return { run: (alias, page) => runMiniAppDataGrid(appId, version, alias, page) };
}
