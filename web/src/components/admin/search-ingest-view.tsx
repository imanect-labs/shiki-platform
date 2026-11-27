"use client";

import * as React from "react";
import { AlertCircle, SearchX } from "lucide-react";

import { fetchSearchIngest, startSearchBackfill, type SearchIngestStatus } from "@/lib/admin-search-api";
import { formatDateTime } from "@/lib/format";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { Skeleton } from "@/components/ui/skeleton";
import { toast } from "@/components/ui/use-toast";
import { AdminPage, Panel, StatTile } from "./admin-page";

const TITLE = "社内文書の検索";

/// 進めている間に読み直す間隔。
const POLL_MS = 3_000;

const count = (n: number) => `${n.toLocaleString("ja-JP")} 件`;

/// データ › 社内文書の検索（取り込みの記録が無いファイルのまとめての取り込み・D-126）。
///
/// 管理者が始めると、サーバーが取り込みの記録が無いファイルを少しずつ取り込みに回す（待ち行列を溜めない）。
/// 進めている間と、取り込みを待っているファイルがある間は、数秒ごとに読み直して進み具合を出す。
export function SearchIngestView() {
  const [status, setStatus] = React.useState<SearchIngestStatus | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [starting, setStarting] = React.useState(false);

  const load = React.useCallback(async () => {
    try {
      setStatus(await fetchSearchIngest());
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  React.useEffect(() => {
    void load();
  }, [load]);

  const running = status?.backfill?.state === "running";
  const active = running || (status?.waiting ?? 0) > 0;
  React.useEffect(() => {
    if (!active) return;
    const timer = window.setInterval(() => void load(), POLL_MS);
    return () => window.clearInterval(timer);
  }, [active, load]);

  if (error && !status) {
    return (
      <AdminPage title={TITLE}>
        <EmptyState
          icon={AlertCircle}
          title="取り込みの様子を読み込めませんでした"
          description={error}
          action={
            <Button variant="outline" size="sm" onClick={() => void load()}>
              もう一度読み込む
            </Button>
          }
        />
      </AdminPage>
    );
  }
  if (!status) {
    return (
      <AdminPage title={TITLE}>
        <div className="flex flex-col gap-4" aria-busy="true">
          <Skeleton className="h-48" />
        </div>
      </AdminPage>
    );
  }
  if (!status.enabled) {
    return (
      <AdminPage title={TITLE}>
        <EmptyState
          icon={SearchX}
          title="この環境では社内文書の検索が使えません"
          description="サーバーの設定（rag.enabled）で社内文書の検索を有効にすると、ここから取り込めます。"
          data-testid="search-ingest-disabled"
        />
      </AdminPage>
    );
  }

  const start = async () => {
    setStarting(true);
    try {
      const res = await startSearchBackfill();
      setStatus(res.status);
      toast(
        res.started
          ? {
              title: "まとめての取り込みを始めました",
              description: "取り込みの記録が無いファイルを、少しずつ取り込みに回します。",
            }
          : { title: "すでに進めています", description: "いま進めているものの進み具合を出しています。" },
      );
    } catch (e) {
      toast({
        variant: "destructive",
        title: "始められませんでした",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setStarting(false);
    }
  };

  const backfill = status.backfill;
  const percent =
    backfill && backfill.files_total > 0
      ? Math.min(100, Math.round((backfill.files_scanned / backfill.files_total) * 100))
      : backfill?.state === "done"
        ? 100
        : 0;

  return (
    <AdminPage title={TITLE}>
      <Panel
        title="取り込まれていないファイル"
        description="検索を有効にする前に置いたファイルなど、取り込みの記録が無いファイルを、少しずつ取り込みます。取り込んだファイルは、見る権限がある人の社内文書の検索に出るようになります。"
      >
        <div className="flex flex-col gap-4 px-4 py-4" data-testid="search-backfill">
          <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
            <Button
              size="sm"
              onClick={() => void start()}
              loading={starting}
              disabled={running}
              data-testid="search-backfill-start"
            >
              まとめて取り込む
            </Button>
            {running ? (
              <p className="text-xs text-muted-foreground">
                いま進めているものが終わると、もう一度始められます。
              </p>
            ) : null}
          </div>
          {backfill ? (
            <>
              <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-[13px] text-muted-foreground">
                <Badge variant={running ? "warning" : "success"} data-testid="search-backfill-state">
                  {running ? "取り込みに回しています" : "確かめ終わりました"}
                </Badge>
                <span>始めた日時: {formatDateTime(backfill.started_at)}</span>
                {backfill.finished_at ? (
                  <span>確かめ終えた日時: {formatDateTime(backfill.finished_at)}</span>
                ) : null}
              </div>
              <div
                role="progressbar"
                aria-label="確かめたファイルの割合"
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={percent}
                className="h-2 w-full overflow-hidden rounded-full bg-muted"
              >
                <div className="h-full rounded-full bg-primary transition-[width]" style={{ width: `${percent}%` }} />
              </div>
              <div className="grid gap-3 sm:grid-cols-3">
                <StatTile
                  label="確かめたファイル"
                  value={backfill.files_scanned.toLocaleString("ja-JP")}
                  sub={`始めたときのファイル ${count(backfill.files_total)}`}
                />
                <StatTile
                  label="取り込みに回したファイル"
                  value={backfill.files_queued.toLocaleString("ja-JP")}
                  sub="取り込みの記録が無かったもの"
                />
                <StatTile
                  label="取り込みを待っているファイル"
                  value={`${status.waiting.toLocaleString("ja-JP")}${status.waiting_capped ? " 以上" : ""}`}
                  sub="新しく置いたファイルも含みます"
                />
              </div>
            </>
          ) : (
            <p className="text-[13px] text-muted-foreground">まだまとめての取り込みをしていません。</p>
          )}
          <p className="text-xs leading-relaxed text-muted-foreground">
            取り込みに失敗したファイルは対象外です。ドライブのファイルのメニューの「検索に取り込み直す」で、1 件ずつ取り込み直してください。
          </p>
        </div>
      </Panel>
    </AdminPage>
  );
}
