"use client";

/// 下書き CSV 画面（Task 11.11）。/csv/draft?thread=&name= で未保存の下書きを詰める。
///
/// - 下書きは**クライアント内のみ**（サーバ未作成・[`csvDraftStore`] が真実源）。チャットで
///   「表を作って」→ save_csv が csv_draft を返し、本画面へ遷移してグリッドで内容を用意する。
/// - 左はローカルデータの [`CsvDraftGrid`]（サーバページング非依存）。編集はセル行列 →
///   CSV 文字列で下書きストアへ書き戻す（リロード復元）。
/// - 同じ会話の複数下書きは**上部タブ**で切替（別名 = 別下書き・N 本並存）。
/// - 右上「ドライブに保存」で POST /tabular/save → 実体化して /csv/{id} へ（ノート下書きと同型）。

import { ArrowLeft, FileWarning, MessageSquare, PencilLine, Save, X } from "lucide-react";
import Link from "next/link";
import { useRouter, useSearchParams } from "next/navigation";
import * as React from "react";

import { Conversation } from "@/components/chat/conversation";
import { CsvDraftGrid } from "@/components/csv/csv-draft-grid";
import { SaveDraftDialog, type SaveTarget } from "@/components/notes/save-draft-dialog";
import { usePageHeader } from "@/components/shell/page-header-context";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { FadeSlide } from "@/components/ui/motion-primitives";
import { toast } from "@/components/ui/use-toast";
import { getThreadMessages } from "@/lib/chat-api";
import { csvDraftStore, parseCsvDraft } from "@/lib/csv/draft";
import { parseCsv, toCsv } from "@/lib/csv/parse";
import { saveNewCsv } from "@/lib/tabular-api";
import { cn } from "@/lib/utils";

/// useSearchParams を使うため Suspense 境界でラップする（静的生成の CSR bailout 要件・Next.js）。
export default function DraftCsvPage() {
  return (
    <React.Suspense fallback={<div className="h-full" />}>
      <DraftCsvPageInner />
    </React.Suspense>
  );
}

function DraftCsvPageInner() {
  const searchParams = useSearchParams();
  const router = useRouter();
  const threadId = searchParams.get("thread") ?? "";
  const nameParam = searchParams.get("name") ?? "";

  const drafts = csvDraftStore.useDrafts(threadId);
  const [activeName, setActiveName] = React.useState(nameParam);
  const [chatOpen, setChatOpen] = React.useState(true);
  const [saveOpen, setSaveOpen] = React.useState(false);
  const [saving, setSaving] = React.useState(false);
  const [recovered, setRecovered] = React.useState(false);
  // グリッドのローカルデータ（[0]=ヘッダ行）。編集のたびに CSV へ直して下書きストアへ書き戻す。
  const [cells, setCells] = React.useState<string[][] | null>(null);

  // 対象の下書きを開く（下書きストアの CSV をパースしてグリッドへ流し込む）。手編集では呼ばない。
  const openDraft = React.useCallback(
    (name: string) => {
      setActiveName(name);
      const csv = csvDraftStore.get(threadId, name)?.content ?? "";
      const parsed = parseCsv(csv);
      setCells(parsed.length > 0 ? parsed : [[""]]);
    },
    [threadId],
  );

  // 下書きストアが空なら、会話履歴の csv_draft ブロックから復元する（リロード/別端末）。
  React.useEffect(() => {
    if (!threadId) {
      setRecovered(true);
      return;
    }
    if (csvDraftStore.list(threadId).length > 0) {
      setRecovered(true);
      return;
    }
    let cancelled = false;
    getThreadMessages(threadId)
      .then(({ messages }) => {
        if (cancelled) return;
        for (const m of messages) {
          for (const b of m.content) {
            if (b.type === "csv_draft") {
              const d = parseCsvDraft(b.draft);
              if (d) csvDraftStore.upsert(threadId, d.name, d.csv, "ai");
            }
          }
        }
      })
      .finally(() => {
        if (!cancelled) setRecovered(true);
      });
    return () => {
      cancelled = true;
    };
  }, [threadId]);

  // 対象下書きをアクティブへ（?name 優先・無ければ最新）。復元完了後、**nameParam ごとに一度**開く。
  const openedForRef = React.useRef<string | null>(null);
  React.useEffect(() => {
    if (!recovered) return;
    const list = csvDraftStore.list(threadId);
    if (list.length === 0) return;
    const key = nameParam || "__latest__";
    if (openedForRef.current === key) return;
    openedForRef.current = key;
    const target =
      (nameParam && list.find((d) => d.name === nameParam)?.name) ?? list[list.length - 1].name;
    openDraft(target);
  }, [recovered, threadId, nameParam, openDraft]);

  // 手編集は下書きストアへ書き戻す（source=user＝再シードしない）。
  const writeBack = React.useCallback(
    (next: string[][]) => {
      setCells(next);
      if (activeName) csvDraftStore.upsert(threadId, activeName, toCsv(next), "user");
    },
    [threadId, activeName],
  );

  const onEdit = React.useCallback(
    (row: number, col: number, value: string) => {
      if (!cells) return;
      const next = cells.map((r) => [...r]);
      // グリッドの row はデータ行 index（ヘッダを除く）なので +1。
      const target = next[row + 1];
      if (!target) return;
      target[col] = value;
      writeBack(next);
    },
    [cells, writeBack],
  );

  const onAppendRow = React.useCallback(() => {
    if (!cells) return;
    const width = cells[0]?.length ?? 1;
    writeBack([...cells.map((r) => [...r]), Array.from({ length: width }, () => "")]);
  }, [cells, writeBack]);

  // アシスタントが同じ会話で流し込んだとき（csv_draft）は、その下書きを開き直す（再シード）。
  const onCsvDraftOpened = React.useCallback((name: string) => openDraft(name), [openDraft]);

  const doSave = React.useCallback(
    (target: SaveTarget) => {
      if (!activeName || !cells || saving) return;
      setSaving(true);
      saveNewCsv({ parentId: target.parentId, name: target.name, csv: toCsv(cells) })
        .then((saved) => {
          csvDraftStore.remove(threadId, activeName);
          toast({ description: `「${saved.name.replace(/\.csv$/i, "")}」を保存しました。` });
          router.replace(`/csv/${saved.node_id}`);
        })
        .catch(() => {
          toast({ description: "保存に失敗しました。" });
          setSaving(false);
        });
    },
    [activeName, cells, saving, threadId, router],
  );

  // 統一ヘッダへ注入（戻る/下書きバッジ/保存/アシスタント切替・ノート下書きと同型）。
  usePageHeader(
    () => (
      <div className="flex min-w-0 flex-1 items-center gap-2.5">
        <Link
          href="/drive"
          className="flex size-8 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground active:scale-95"
          aria-label="ドライブへ戻る"
        >
          <ArrowLeft className="size-4" />
        </Link>
        <span className="min-w-0 flex-1 truncate text-sm font-medium text-foreground">
          {activeName || "下書き CSV"}
        </span>
        <span
          className="inline-flex items-center gap-1 rounded-full border border-dashed border-amber-500/50 bg-amber-500/10 px-2.5 py-0.5 text-xs font-medium text-amber-600 dark:text-amber-400"
          data-testid="draft-badge"
        >
          <PencilLine className="size-3.5" aria-hidden />
          下書き（未保存）
        </span>
        <Button
          type="button"
          size="sm"
          onClick={() => setSaveOpen(true)}
          disabled={!activeName}
          data-testid="draft-save-button"
        >
          <Save className="mr-1.5 size-4" aria-hidden />
          ドライブに保存
        </Button>
        <Button
          type="button"
          variant={chatOpen ? "secondary" : "ghost"}
          size="sm"
          onClick={() => setChatOpen((v) => !v)}
          aria-pressed={chatOpen}
          data-testid="note-chat-toggle"
        >
          <MessageSquare className="mr-1.5 size-4" aria-hidden />
          アシスタント
        </Button>
      </div>
    ),
    [activeName, chatOpen],
  );

  if (!threadId) {
    return (
      <EmptyState
        title="下書きが見つかりません"
        description="チャットから「表を作って」と頼むと、ここに下書きが用意されます。"
      />
    );
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="relative flex min-h-0 flex-1">
        <div
          className={cn(
            "flex min-w-0 flex-1 flex-col transition-[padding] duration-[var(--duration-normal)] ease-[var(--ease-standard)]",
            chatOpen && "lg:pr-[28rem]",
          )}
        >
          {/* 下書きタブ（同じ会話の複数下書きを切替・別名 = 別下書き）。 */}
          {drafts.length > 1 ? (
            <div className="flex flex-wrap items-center gap-1.5 px-4 pt-3" data-testid="draft-tabs">
              {drafts.map((d) => (
                <button
                  key={d.name}
                  type="button"
                  onClick={() => openDraft(d.name)}
                  className={cn(
                    "inline-flex items-center gap-1.5 rounded-full border px-3 py-1 text-[13px] transition-colors",
                    d.name === activeName
                      ? "bg-accent font-medium text-foreground"
                      : "border-border/60 bg-card/40 text-muted-foreground hover:bg-secondary hover:text-foreground",
                  )}
                >
                  <PencilLine className="size-3.5 shrink-0" aria-hidden />
                  <span className="max-w-[12rem] truncate">{d.name}</span>
                </button>
              ))}
            </div>
          ) : null}

          <div className="min-h-0 flex-1 p-4">
            {cells && activeName ? (
              <CsvDraftGrid
                key={activeName}
                header={cells[0] ?? []}
                rows={cells.slice(1)}
                onEdit={onEdit}
                onAppendRow={onAppendRow}
              />
            ) : (
              <div className="flex min-h-[40vh] flex-col items-center justify-center gap-2 text-center text-sm text-muted-foreground">
                <FileWarning className="size-6 text-muted-foreground/60" aria-hidden />
                下書きを読み込んでいます…
              </div>
            )}
          </div>
        </div>

        {chatOpen && (
          <FadeSlide
            from="right"
            role="complementary"
            aria-label="CSV のアシスタント"
            className="absolute inset-y-3 right-3 z-20 flex w-[min(420px,calc(100%-1.5rem))] flex-col overflow-hidden rounded-2xl border bg-card shadow-lg"
          >
            <div className="flex h-11 shrink-0 items-center gap-2 px-3 shiki-dash-bottom">
              <MessageSquare className="size-4 text-muted-foreground" aria-hidden />
              <span className="flex-1 text-sm font-medium">アシスタント</span>
              <button
                type="button"
                onClick={() => setChatOpen(false)}
                aria-label="チャットを閉じる"
                className="flex size-7 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground active:scale-90"
              >
                <X className="size-4" aria-hidden />
              </button>
            </div>
            <div className="min-h-0 flex-1">
              {/* 下書き画面では遷移せずアクティブ下書きを切替（onCsvDraftOpened）。 */}
              <Conversation
                threadId={threadId}
                variant="panel"
                onCsvDraftOpened={onCsvDraftOpened}
              />
            </div>
          </FadeSlide>
        )}
      </div>

      <SaveDraftDialog
        open={saveOpen}
        onOpenChange={setSaveOpen}
        defaultName={activeName}
        saving={saving}
        onConfirm={doSave}
        entityLabel="CSV"
        description="下書きを CSV として保存します。保存後はグリッド編集・SQL 分析・共有ができます。"
      />
    </div>
  );
}
