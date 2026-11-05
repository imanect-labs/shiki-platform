"use client";

/// 1 回の回答に付いた document_ref カードの並び＋**AI の編集の取り消し**（#1049）。
///
/// AI が既存ファイルを編集すると新しい版ができ、カードに `{id, version}` が載る。「元に戻す」は
/// その編集の直前の版を**新しい版として**復元する（`POST /files/{id}/versions/{v}/revert`・履歴は残る）。
/// 戻した後は「やり直す」＝取り消しで作った版をさらに取り消す。
///
/// - **ファイル単位で 1 つの操作**にする。同じ回答で同じファイルを複数回編集したら、操作はそのファイルの
///   最後のカードにだけ出し、この回答でそのファイルに作った全版をまとめて戻す（カードごとに別々の状態を
///   持つと、片方だけ戻した／やり直した表示と実データが食い違うため）。
/// - 編集の後に別の内容版（他の人・後の編集）があると、黙って巻き戻さず確認ダイアログを出す
///   （判定はサーバが内容版で行う＝名前変更・移動では止めない）。Office エディタで開かれている間は戻さない。
/// - 2 ファイル以上なら「まとめて元に戻す」。ファイルごとに戻し、ファイルをまたいだ原子性は無い
///   （戻せなかったものは理由を出す）。
/// - 状態は**ファイル×この回答で作った版**をキーにした画面内ストアに置く。回答の生成中の行と確定後の行は
///   別のコンポーネントなので、コンポーネントの state だと確定時に取り消し済みの表示が消える。
///   再読込後に押した場合も、既に戻っていればサーバは何もしない。

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
import { RevertConflict, RevertLocked, revertEdit, StorageApiError } from "@/lib/storage";

import { DocumentRefCard, isRevertable, parseDocumentRef, type DocumentRef } from "./document-ref-card";

/// 1 ファイル分の取り消し状態。
type RevertState =
  | { phase: "edited" }
  /// 元に戻した。`undoVersion` は取り消しで作った版（やり直す＝これを取り消す）。
  | { phase: "reverted"; undoVersion: number; restoredFrom: number | null }
  /// やり直した。`version` はやり直しで作った版（もう一度元に戻す＝これを取り消す）。
  | { phase: "redone"; version: number };

// --- 画面内ストア（行の再マウントをまたいで状態を保つ） -----------------------------------------

const revertStates = new Map<string, RevertState>();
const listeners = new Set<() => void>();

function setRevertState(key: string, state: RevertState) {
  revertStates.set(key, state);
  for (const l of listeners) l();
}

function useRevertState(key: string): RevertState {
  const subscribe = React.useCallback((cb: () => void) => {
    listeners.add(cb);
    return () => {
      listeners.delete(cb);
    };
  }, []);
  const get = React.useCallback(() => revertStates.get(key) ?? EDITED, [key]);
  return React.useSyncExternalStore(subscribe, get, () => EDITED);
}

const EDITED: RevertState = { phase: "edited" };

// --- グルーピング -------------------------------------------------------------------------------

/// 回答内の 1 ファイル分（この回答で作った版と、操作を出すカードの位置）。
type FileGroup = { fileId: string; name: string; versions: number[]; lastIndex: number; key: string };

function groupByFile(docs: (DocumentRef | null)[]): FileGroup[] {
  const groups = new Map<string, FileGroup>();
  docs.forEach((doc, i) => {
    if (!doc || !isRevertable(doc) || doc.version === null) return;
    const g = groups.get(doc.id) ?? { fileId: doc.id, name: doc.name, versions: [], lastIndex: i, key: "" };
    g.versions.push(doc.version);
    g.lastIndex = i;
    g.name = doc.name;
    groups.set(doc.id, g);
  });
  return [...groups.values()].map((g) => {
    const versions = [...new Set(g.versions)].sort((a, b) => a - b);
    return { ...g, versions, key: `${g.fileId}@${versions.join(",")}` };
  });
}

/// 失敗を利用者向けの文言へ。
function failureMessage(e: unknown): string {
  if (e instanceof RevertLocked) return e.message;
  if (e instanceof StorageApiError) {
    if (e.status === 403) return "このファイルを編集する権限がありません。";
    if (e.status === 404) return "戻す前の版が見つかりません（ファイルが削除された可能性があります）。";
  }
  return e instanceof Error ? e.message : String(e);
}

/// 次の操作で取り消す版（undo は編集の版、redo は取り消しで作った版）。
function targetVersions(group: FileGroup, st: RevertState, mode: "undo" | "redo"): number[] {
  if (mode === "redo") return st.phase === "reverted" ? [st.undoVersion] : [];
  return st.phase === "redone" ? [st.version] : group.versions;
}

/// 1 ファイル分の取り消し/やり直しを実行して状態を進める。競合はそのまま投げる（呼び出し側が扱う）。
async function runRevert(group: FileGroup, mode: "undo" | "redo", force: boolean) {
  const st = revertStates.get(group.key) ?? EDITED;
  const versions = targetVersions(group, st, mode);
  if (versions.length === 0) return;
  const res = await revertEdit(group.fileId, versions, { force });
  setRevertState(
    group.key,
    mode === "undo"
      ? {
          phase: "reverted",
          undoVersion: res.node.version,
          restoredFrom: res.reverted ? (res.restored_from_version ?? null) : null,
        }
      : { phase: "redone", version: res.node.version },
  );
}

export function DocumentRefList({ raws }: { raws: unknown[] }) {
  const docs = React.useMemo(() => raws.map(parseDocumentRef), [raws]);
  const groups = React.useMemo(() => groupByFile(docs), [docs]);
  const [busy, setBusy] = React.useState<Set<string>>(new Set());
  const [batchBusy, setBatchBusy] = React.useState(false);
  /// 後続の版がある取り消しの確認（どのファイルの、どの操作か）。
  const [conflict, setConflict] = React.useState<{
    group: FileGroup;
    mode: "undo" | "redo";
    current: number;
  } | null>(null);

  const setBusyFor = (key: string, on: boolean) =>
    setBusy((cur) => {
      const next = new Set(cur);
      if (on) next.add(key);
      else next.delete(key);
      return next;
    });

  const run = async (group: FileGroup, mode: "undo" | "redo", force = false) => {
    if (busy.has(group.key)) return;
    setBusyFor(group.key, true);
    try {
      await runRevert(group, mode, force);
    } catch (e) {
      if (e instanceof RevertConflict) {
        setConflict({ group, mode, current: e.currentVersion });
      } else {
        toast({ variant: "destructive", title: "元に戻せませんでした", description: failureMessage(e) });
      }
    } finally {
      setBusyFor(group.key, false);
    }
  };

  /// この回答の編集をまとめて取り消す（ファイルごと・まだ戻していないものだけ）。
  const undoAll = async () => {
    setBatchBusy(true);
    let ok = 0;
    const skipped: string[] = [];
    for (const g of groups) {
      if ((revertStates.get(g.key) ?? EDITED).phase === "reverted") continue;
      try {
        await runRevert(g, "undo", false);
        ok += 1;
      } catch (e) {
        skipped.push(
          e instanceof RevertConflict
            ? `${g.name}（後から更新されているため。カードから個別に戻せます）`
            : `${g.name}（${failureMessage(e)}）`,
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

  return (
    <>
      {docs.map((doc, i) => {
        const raw = raws[i];
        const group = doc ? groups.find((g) => g.fileId === doc.id) : undefined;
        if (!doc || !group) return <DocumentRefCard key={i} raw={raw} />;
        return (
          <FileRefCard
            key={i}
            raw={raw}
            doc={doc}
            group={group}
            isLast={group.lastIndex === i}
            busy={busy.has(group.key) || batchBusy}
            onRun={(mode) => void run(group, mode)}
          />
        );
      })}
      <UndoAllButton groups={groups} busy={batchBusy || busy.size > 0} onClick={() => void undoAll()} />

      <Dialog open={conflict !== null} onOpenChange={(o) => !o && setConflict(null)}>
        <DialogContent data-testid="document-ref-revert-conflict">
          <DialogHeader>
            <DialogTitle>後から更新されています</DialogTitle>
            <DialogDescription>
              {conflict
                ? `「${conflict.group.name}」はこの編集の後に更新されています（現在 v${conflict.current}）。${
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
                if (c) void run(c.group, c.mode, true);
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

/// 取り消し対象のファイルのカード。操作はそのファイルの最後のカードにだけ出す。
function FileRefCard({
  raw,
  doc,
  group,
  isLast,
  busy,
  onRun,
}: {
  raw: unknown;
  doc: DocumentRef;
  group: FileGroup;
  isLast: boolean;
  busy: boolean;
  onRun: (mode: "undo" | "redo") => void;
}) {
  const st = useRevertState(group.key);
  const status =
    st.phase === "reverted"
      ? st.restoredFrom === null
        ? "元に戻っています"
        : `元に戻しました（v${st.restoredFrom} の内容を v${st.undoVersion} として保存）`
      : st.phase === "redone"
        ? `やり直しました（v${st.version}）`
        : undefined;
  if (!isLast) {
    // 同じファイルの前のカード: 状態だけ揃え、操作は最後のカードに集める。
    return <DocumentRefCard raw={raw} status={status} />;
  }
  const multi = group.versions.length > 1;
  return (
    <DocumentRefCard
      raw={raw}
      status={status ?? (multi ? `この回答で ${group.versions.length} 回編集しました（v${doc.version}）` : undefined)}
      actions={
        st.phase === "reverted" ? (
          st.restoredFrom !== null ? (
            <RevertButton
              label="やり直す"
              ariaLabel={`${doc.name}の AI の編集をやり直す`}
              icon={<Redo2 className="size-3.5" aria-hidden />}
              busy={busy}
              onClick={() => onRun("redo")}
              testId="document-ref-redo"
            />
          ) : null
        ) : (
          <RevertButton
            label="元に戻す"
            ariaLabel={
              multi
                ? `${doc.name}へのこの回答の AI の編集（${group.versions.length} 回）を元に戻す`
                : `${doc.name}の AI の編集を元に戻す`
            }
            icon={<Undo2 className="size-3.5" aria-hidden />}
            busy={busy}
            onClick={() => onRun("undo")}
            testId="document-ref-undo"
          />
        )
      }
    />
  );
}

/// 2 ファイル以上を編集した回答だけに出す一括取り消し（まだ戻していないファイルが 2 つ以上のとき）。
function UndoAllButton({
  groups,
  busy,
  onClick,
}: {
  groups: FileGroup[];
  busy: boolean;
  onClick: () => void;
}) {
  const subscribe = React.useCallback((cb: () => void) => {
    listeners.add(cb);
    return () => {
      listeners.delete(cb);
    };
  }, []);
  const pending = React.useSyncExternalStore(
    subscribe,
    () => groups.filter((g) => (revertStates.get(g.key) ?? EDITED).phase !== "reverted").length,
    () => groups.length,
  );
  if (pending < 2) return null;
  return (
    <div className="-mt-1 mb-2 flex justify-end">
      <Button
        type="button"
        variant="ghost"
        size="sm"
        className="h-7 px-2 text-xs text-muted-foreground"
        loading={busy}
        disabled={busy}
        onClick={onClick}
        data-testid="document-ref-undo-all"
      >
        {!busy ? <Undo2 className="size-3.5" aria-hidden /> : null}
        この回答の編集をまとめて元に戻す
      </Button>
    </div>
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
