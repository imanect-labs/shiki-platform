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
  /// 読み直しの世代（画面のボタンの起動が終わるたびに増やす。変わると表を先頭から読み直す・BUG-19）。
  revision?: number;
};

export const DataGridHostContext = React.createContext<DataGridHost | null>(null);

/// ミニアプリ（id・版）向けの実行先。
export function miniAppDataGridHost(appId: string, version: number, revision = 0): DataGridHost {
  return { run: (alias, page) => runMiniAppDataGrid(appId, version, alias, page), revision };
}
