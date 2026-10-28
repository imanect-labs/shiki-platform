"use client";

/// SQL で問い合わせる画面（#978）。見えるテーブルを閉じた SELECT で検索・集計する。

import { ChevronLeft } from "lucide-react";
import Link from "next/link";

import { SqlConsole } from "@/components/data/sql-console";
import { PageContainer } from "@/components/shell/page-container";
import { Button } from "@/components/ui/button";

export default function DataSqlPage() {
  return (
    <PageContainer
      title="SQL で問い合わせ"
      description="SELECT 文で、見られるテーブルを結合・集計して読めます。行と列はあなたの権限で絞られます。"
      className="max-w-6xl"
      actions={
        <Button variant="ghost" size="sm" asChild>
          <Link href="/data">
            <ChevronLeft className="size-4" aria-hidden />
            テーブル一覧
          </Link>
        </Button>
      }
    >
      <SqlConsole />
    </PageContainer>
  );
}
