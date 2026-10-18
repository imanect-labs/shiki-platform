"use client";

/// 下書き CSV 画面（Task 11.11）。/csv/draft?thread=&name= で未保存の下書きを詰める。
///
/// - 下書きは**クライアント内のみ**（サーバ未作成・[`csvDraftStore`] が真実源）。チャットで
///   「表を作って」→ save_csv が csv_draft を返し、本画面へ遷移してグリッドで内容を用意する。
/// - 左はローカルデータの [`CsvDraftGrid`]（サーバページング非依存）。編集はセル行列 →
///   CSV 文字列で下書きストアへ書き戻す（リロード復元）。
/// - 同じ会話の複数下書きは**上部タブ**で切替（別名 = 別下書き・N 本並存）。
/// - 右上「保存」（PC は「ドライブに保存」）で POST /tabular/save → 実体化して /csv/{id} へ（ノート下書きと同型）。

import { FileWarning } from "lucide-react";
import { useRouter, useSearchParams } from "next/navigation";
import * as React from "react";

import { Conversation } from "@/components/chat/conversation";
import { CsvDraftGrid } from "@/components/csv/csv-draft-grid";
import {
  DraftBadge,
  DraftNotFound,
  DraftTabs,
  SaveDraftDialog,
  draftChrome,
  type SaveTarget,
} from "@/components/notes/save-draft-dialog";
import { AssistantLayout } from "@/components/shell/assistant-sheet";
import { EditorMobileHeading, useEditorBack } from "@/components/shell/editor-loading";
import { usePageChrome } from "@/components/shell/page-header-context";
import { toast } from "@/components/ui/use-toast";
import { getThreadMessages } from "@/lib/chat-api";
import { csvDraftStore, parseCsvDraft } from "@/lib/csv/draft";
import { parseCsv, toCsv } from "@/lib/csv/parse";
import { saveNewCsv } from "@/lib/tabular-api";

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
  // AI 出力が不揃い（行ごとに列数が違う）でも、最大列数へ空セルでパディングして矩形にする。
  const openDraft = React.useCallback(
    (name: string) => {
      setActiveName(name);
      const csv = csvDraftStore.get(threadId, name)?.content ?? "";
      const parsed = parseCsv(csv);
      const width = Math.max(1, ...parsed.map((r) => r.length));
      const rect = parsed.map((r) =>
        r.length === width ? r : [...r, ...Array.from({ length: width - r.length }, () => "")],
      );
      setCells(rect.length > 0 ? rect : [[""]]);
    },
    [threadId],
  );

  // 会話履歴の csv_draft ブロックから復元する（リロード/別端末）。ローカルに一部だけ
  // 残っている場合も履歴を常に取得し、**未登録の (threadId, name) のみ**追加する
  // （ローカルのユーザー編集は上書きしない・notes/slides 下書きと同型）。
  React.useEffect(() => {
    if (!threadId) {
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
              if (d && !csvDraftStore.get(threadId, d.name)) {
                csvDraftStore.upsert(threadId, d.name, d.csv, "ai");
              }
            }
          }
        }
      })
      // 履歴を読めなくても（会話が消えた等）ローカルの下書きで開く。
      .catch(() => {})
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

  // クローム（戻る → 保存 → アシスタント → ⋯・ノート下書きと同型）。
  const back = useEditorBack();
  usePageChrome(
    () =>
      draftChrome({
        back,
        threadId,
        name: activeName || "下書き CSV",
        canSave: Boolean(activeName),
        onSave: () => setSaveOpen(true),
        chatOpen,
        onToggleChat: () => setChatOpen((v) => !v),
      }),
    [back, threadId, activeName, chatOpen],
  );

  if (!threadId) {
    return <DraftNotFound what="表" />;
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* スマホ: 文書名＋状態（PC はバーに出す）。 */}
      <EditorMobileHeading name={activeName || "下書き CSV"} status={<DraftBadge />} />
      <AssistantLayout
        open={chatOpen}
        onClose={() => setChatOpen(false)}
        label="CSV のアシスタント"
        panel={
          // 下書き画面では遷移せずアクティブ下書きを切替（onCsvDraftOpened）。
          <Conversation threadId={threadId} variant="panel" onCsvDraftOpened={onCsvDraftOpened} />
        }
      >
        {/* 下書きタブ（同じ会話の複数下書きを切替・別名 = 別下書き）。 */}
        <DraftTabs
          drafts={drafts}
          activeName={activeName}
          onOpen={openDraft}
          className="px-4 pt-3 max-md:pb-1 max-md:pt-0"
        />

        <div className="min-h-0 flex-1 p-4 max-md:px-2 max-md:pb-[max(0.5rem,env(safe-area-inset-bottom))] max-md:pt-1">
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
      </AssistantLayout>

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
