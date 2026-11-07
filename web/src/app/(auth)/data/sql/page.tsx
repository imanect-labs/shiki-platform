"use client";

/// SQL で問い合わせる画面（#978）。見えるテーブルを閉じた SELECT で検索・集計する。
/// `?table=<id>` はテーブルの画面から開いたとき（そのテーブルの例を入れ、戻る先をそのテーブルにする）。

import * as React from "react";

import { SqlConsole } from "@/components/data/sql-console";

export default function DataSqlPage() {
  return (
    <React.Suspense fallback={null}>
      <SqlConsole />
    </React.Suspense>
  );
}
