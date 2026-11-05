"use client";

/// 1 回の回答に付いた document_ref カードの並び＋**AI の編集の取り消し**（#1049）。
///
/// AI が既存ファイルを編集すると新しい版ができ、カードに `{id, version}` が載る。各カードの
/// 「元に戻す」はその編集の直前の版を**新しい版として**復元する（`POST /files/{id}/versions/{v}/revert`・
/// 履歴は残る）。戻した後は「やり直す」＝取り消しの取り消し（取り消しで作った版をさらに取り消す）。
///
/// - 編集の後に別の版がある（他の人・後の編集）ときは黙って巻き戻さず、確認ダイアログを出す。
/// - 2 件以上あるときは「まとめて元に戻す」。**ファイルごと**に、この回答で最初に編集した版から
///   最後に編集した版までをまとめて戻す（`expected_current_version`＝最後の版）。ファイルを
///   またいだ原子性は無い（1 ファイルずつ戻し、戻せなかったものは理由を出す）。
/// - 状態はこの画面だけのもの。再読込後にもう一度押しても、既に戻っていれば何もしない（サーバ側）。

import { Redo2, Undo2 } from "lucide-react";
import * as React from "react";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { toast } from "@/components/ui/use-toast";
import { RevertConflict, revertEdit, StorageApiError } from "@/lib/storage";

import { DocumentRefCard, isRevertable, parseDocumentRef, type DocumentRef } from "./document-ref-card";

/// カード 1 枚の取り消し状態。`version` は「次に取り消す対象」の版。
type RevertState =
  | { phase: "edited"; version: number }
  | { phase: "reverted"; undoneVersion: number; newVersion: number; restoredFrom: number | null };

type Item = { raw: unknown; doc: DocumentRef | null };

/// 取り消しの失敗を利用者向けの文言へ。
function failureMessage(e: unknown): string {
  if (e instanceof StorageApiError) {
    if (e.status === 403) return "このファイルを編集する権限がありません。";
    if (e.status === 404) return "戻す前の版が見つかりません（ファイルが削除された可能性があります）。";
  }
  return e instanceof Error ? e.message : String(e);
}

export function DocumentRefList({ raws }: { raws: unknown[] }) {
  const items: Item[] = React.useMemo(
    () => raws.map((raw) => ({ raw, doc: parseDocumentRef(raw) })),
    [raws],
  );
  const [states, setStates] = React.useState<Record<number, RevertState>>({});
  const [busy, setBusy] = React.useState<Set<number>>(new Set());
  const [batchBusy, setBatchBusy] = React.useState(false);
  /// 後続の版がある取り消しの確認（どのカードの、どの操作か）。
  const [conflict, setConflict] = React.useState<{
    index: number;
    mode: "undo" | "redo";
    current: number;
  } | null>(null);

  const stateOf = React.useCallback(
    (i: number, doc: DocumentRef): RevertState =>
      states[i] ?? { phase: "edited", version: doc.version ?? 0 },
    [states],
  );
  const revertable = items
    .map((it, i) => ({ ...it, i }))
    .filter((it): it is { raw: unknown; doc: DocumentRef; i: number } => !!it.doc && isRevertable(it.doc));

  const setBusyFor = (i: number, on: boolean) =>
    setBusy((cur) => {
      const next = new Set(cur);
      if (on) next.add(i);
      else next.delete(i);
      return next;
    });

  /// 1 枚分の取り消し（undo）/ 取り消しの取り消し（redo）。成功すれば true。
  const run = async (i: number, mode: "undo" | "redo", force = false): Promise<boolean> => {
    const doc = items[i]?.doc;
    if (!doc) return false;
    const st = stateOf(i, doc);
    const target = st.phase === "edited" ? st.version : st.newVersion;
    setBusyFor(i, true);
    try {
      const res = await revertEdit(doc.id, target, { force });
      setStates((cur) => ({
        ...cur,
        [i]:
          mode === "undo"
            ? {
                phase: "reverted",
                undoneVersion: target,
                newVersion: res.node.version,
                restoredFrom: res.reverted ? (res.restored_from_version ?? null) : null,
              }
            : { phase: "edited", version: res.node.version },
      }));
      return true;
    } catch (e) {
      if (e instanceof RevertConflict) {
        setConflict({ index: i, mode, current: e.currentVersion });
      } else {
        toast({ variant: "destructive", title: "元に戻せませんでした", description: failureMessage(e) });
      }
      return false;
    } finally {
      setBusyFor(i, false);
    }
  };

  /// この回答の編集をまとめて取り消す（ファイルごとに最初〜最後の編集をひとまとめ）。
  const undoAll = async () => {
    setBatchBusy(true);
    // ファイル id → この回答でのカード（版の昇順）。まだ戻していないものだけ。
    const byFile = new Map<string, { i: number; version: number }[]>();
    for (const it of revertable) {
      const st = stateOf(it.i, it.doc);
      if (st.phase !== "edited") continue;
      const list = byFile.get(it.doc.id) ?? [];
      list.push({ i: it.i, version: st.version });
      byFile.set(it.doc.id, list);
    }
    let ok = 0;
    const skipped: string[] = [];
    for (const [fileId, list] of byFile) {
      list.sort((a, b) => a.version - b.version);
      const first = list[0];
      const last = list[list.length - 1];
      const name = items[first.i]?.doc?.name ?? "";
      try {
        const res = await revertEdit(fileId, first.version, { expectedCurrentVersion: last.version });
        setStates((cur) => {
          const next = { ...cur };
          for (const { i, version } of list) {
            next[i] = {
              phase: "reverted",
              undoneVersion: version,
              newVersion: res.node.version,
              restoredFrom: res.reverted ? (res.restored_from_version ?? null) : null,
            };
          }
          return next;
        });
        ok += 1;
      } catch (e) {
        skipped.push(
          e instanceof RevertConflict
            ? `${name}（後から更新されているため。カードから個別に戻せます）`
            : `${name}（${failureMessage(e)}）`,
        );
      }
    }
    setBatchBusy(false);
    if (skipped.length === 0) {
      toast({ title: "元に戻しました", description: `${ok} 件のファイルを編集前に戻しました。` });
    } else {
      toast({
        variant: "destructive",
        title: ok > 0 ? `${ok} 件を戻し、${skipped.length} 件は戻していません` : "元に戻せませんでした",
        description: skipped.join("、"),
      });
    }
  };

  const pendingUndo = revertable.filter((it) => stateOf(it.i, it.doc).phase === "edited").length;

  return (
    <>
      {items.map(({ raw, doc }, i) => {
        if (!doc || !isRevertable(doc)) return <DocumentRefCard key={i} raw={raw} />;
        const st = stateOf(i, doc);
        const isBusy = busy.has(i) || batchBusy;
        const status =
          st.phase === "reverted"
            ? st.restoredFrom === null
              ? `元に戻っています（v${st.newVersion}）`
              : `元に戻しました（v${st.restoredFrom} の内容を v${st.newVersion} として保存）`
            : st.version !== doc.version
              ? `やり直しました（v${st.version}）`
              : undefined;
        return (
          <DocumentRefCard
            key={i}
            raw={raw}
            status={status}
            actions={
              st.phase === "edited" ? (
                <RevertButton
                  label="元に戻す"
                  ariaLabel={`${doc.name}の AI の編集を元に戻す`}
                  icon={<Undo2 className="size-3.5" aria-hidden />}
                  busy={isBusy}
                  onClick={() => void run(i, "undo")}
                  testId="document-ref-undo"
                />
              ) : st.restoredFrom !== null ? (
                <RevertButton
                  label="やり直す"
                  ariaLabel={`${doc.name}の AI の編集をやり直す`}
                  icon={<Redo2 className="size-3.5" aria-hidden />}
                  busy={isBusy}
                  onClick={() => void run(i, "redo")}
                  testId="document-ref-redo"
                />
              ) : null
            }
          />
        );
      })}
      {pendingUndo >= 2 ? (
        <div className="-mt-1 mb-2 flex justify-end">
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="h-7 px-2 text-xs text-muted-foreground"
            loading={batchBusy}
            disabled={batchBusy || busy.size > 0}
            onClick={() => void undoAll()}
            data-testid="document-ref-undo-all"
          >
            {!batchBusy ? <Undo2 className="size-3.5" aria-hidden /> : null}
            この回答の編集をまとめて元に戻す
          </Button>
        </div>
      ) : null}

      <Dialog open={conflict !== null} onOpenChange={(o) => !o && setConflict(null)}>
        <DialogContent data-testid="document-ref-revert-conflict">
          <DialogHeader>
            <DialogTitle>後から更新されています</DialogTitle>
            <DialogDescription>
              {conflict
                ? `「${items[conflict.index]?.doc?.name ?? ""}」はこの編集の後に更新されています（現在 v${conflict.current}）。${
                    conflict.mode === "undo" ? "元に戻す" : "やり直す"
                  }と、その後の変更も失われます（版履歴からは戻せます）。`
                : null}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setConflict(null)}>
              やめる
            </Button>
            <Button
              variant="destructive"
              onClick={() => {
                const c = conflict;
                setConflict(null);
                if (c) void run(c.index, c.mode, true);
              }}
              data-testid="document-ref-revert-force"
            >
              それでも{conflict?.mode === "redo" ? "やり直す" : "元に戻す"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}

function RevertButton({
  label,
  ariaLabel,
  icon,
  busy,
  onClick,
  testId,
}: {
  label: string;
  ariaLabel: string;
  icon: React.ReactNode;
  busy: boolean;
  onClick: () => void;
  testId: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={busy}
      aria-label={ariaLabel}
      // 狭幅ではアイコンだけになるので、ホバー/長押しで何のボタンか分かるようにする。
      title={label}
      aria-busy={busy || undefined}
      data-testid={testId}
      className="inline-flex shrink-0 items-center gap-1.5 rounded-full border px-3 py-1.5 text-xs font-medium transition-colors duration-fast hover:border-primary/40 hover:bg-secondary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-50"
    >
      {icon}
      <span className="max-sm:sr-only">{label}</span>
    </button>
  );
}
