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
  /// 使う人に足りない権限があって表を描けない理由（GAP-37）。描けるなら null。
  blockedReason?: (alias: string) => string | null;
  /// 追加だけの共有で、自分が追加したレコードだけを表示している（GAP-37）。
  ownRecordsOnly?: (alias: string) => boolean;
  /// 表を描けない理由が権限のときに、確かめ直す（共有してもらった後に、開き直さずに表を出す・BUG-217）。
  recheck?: () => Promise<void>;
};

/// 表の別名ごとの、使う人の権限の確かめの結果（[`miniAppDataGridHost`] に渡す）。
export type DataGridAccess = {
  blockedReason: (alias: string) => string | null;
  ownRecordsOnly: (alias: string) => boolean;
};

export const DataGridHostContext = React.createContext<DataGridHost | null>(null);

/// ミニアプリ（id・版）向けの実行先。`access` は使う人の権限の確かめの結果（読み込む前・読めなかったときは省く）。
export function miniAppDataGridHost(
  appId: string,
  version: number,
  revision = 0,
  access?: DataGridAccess,
): DataGridHost {
  return { run: (alias, page) => runMiniAppDataGrid(appId, version, alias, page), revision, ...access };
}
