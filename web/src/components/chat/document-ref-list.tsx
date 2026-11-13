"use client";

/// 1 回の回答に付いた document_ref カードの並び＋**AI の編集の取り消し**（#1049）。
///
/// AI が既存ファイルを編集すると新しい版ができ、カードに `{id, version}` が載る。「元に戻す」は
/// その編集の直前の版を**新しい版として**復元する（`POST /files/{id}/versions/{v}/revert`・履歴は残る）。
/// 戻した後は「やり直す」＝取り消しで作った版をさらに取り消す。
///
/// - **同じファイルは 1 枚**にする。同じ回答で同じファイルを何度も作成・編集しても、カードは最後（＝最新の版）の
///   1 枚だけを出す（v1・v2… と版ごとに並べると、別々のファイルを作ったように見える）。まとめるのは**版が
///   確定した作成・編集どうしだけ**。移動・名前変更、提案版（current に入っていない）、版未確定のライブ編集は
///   それぞれ別に出す（確定した編集とまとめると、編集の「元に戻す」か、未確定・採用待ちの変更があることの
///   どちらかが画面から消える）。
/// - **ファイル単位で 1 つの操作**にする。操作はその 1 枚に出し、この回答でそのファイルに作った全版をまとめて
///   戻す（カードごとに別々の状態を持つと、片方だけ戻した／やり直した表示と実データが食い違うため）。
/// - 編集の後に別の内容版（他の人・後の編集）があると、黙って巻き戻さず確認ダイアログを出す
///   （判定はサーバが内容版で行う＝名前変更・移動では止めない）。Office エディタで開かれている間は戻さない。
/// - 操作は**編集権限のある人にだけ**出す（`/collab/docs/{id}/access` の editor＝取り消し API と同じ
///   editor@file 判定。分かるまでは出さない）。閲覧のみ・権限を外された人が押しても API は拒否するが、
///   押せないボタンは見せない。
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
import { getCollabAccess } from "@/lib/notes-api";
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
/// 実行中のキー。行の再マウント（生成中→確定）をまたいで二重送信を防ぐため、状態と同じストアに置く。
const inflight = new Set<string>();
const listeners = new Set<() => void>();

/// ストアの購読者（各カード・一括ボタン）に変化を知らせる。
function notify() {
  for (const l of listeners) l();
}

/// そのファイルの取り消し/やり直しが実行中か（再マウントをまたいで共有）。
function useInflight(key: string): boolean {
  const subscribe = React.useCallback((cb: () => void) => {
    listeners.add(cb);
    return () => {
      listeners.delete(cb);
    };
  }, []);
  return React.useSyncExternalStore(
    subscribe,
    () => inflight.has(key),
    () => false,
  );
}

/// 取り消し状態を更新して購読者に知らせる。
function setRevertState(key: string, state: RevertState) {
  revertStates.set(key, state);
  notify();
}

/// そのファイルの取り消し状態（未操作なら編集済み）。
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

// --- 編集権限（ファイルごとに引き、短時間だけ使い回す） -----------------------------------------
//
// 結果は CAN_EDIT_TTL_MS の間だけ有効。期限を過ぎた結果は**表示に使わない**（引き直しが終わるまで
// 操作を出さない＝fail-closed）。結果ごとに期限ちょうどのタイマーを張り、期限が来たら表示を更新して
// （操作を隠して）、表示中のファイルなら引き直す。タブの再表示でも引き直すので、開いたままの画面でも
// 共有の変更（editor → viewer）は期限内に反映される。ログインし直す（利用者が変わる）経路は Keycloak を
// 経由した全体の再読込なので、このモジュールの状態ごと消える（利用者をまたいで結果が残らない）。

/// 権限の結果を使い回す時間。
const CAN_EDIT_TTL_MS = 30_000;

/// fileId → 編集できるか（と取得時刻）。取得前は未登録。
const canEditCache = new Map<string, { value: boolean; at: number }>();
const canEditPending = new Set<string>();
/// fileId → 表示中のカードの数（期限が来たときに引き直すかの判断に使う）。
const canEditWatchers = new Map<string, number>();
/// fileId → 結果の期限タイマー。
const canEditExpiry = new Map<string, number>();

/// 結果の期限ちょうどに表示を更新し、表示中なら引き直す。
function scheduleCanEditExpiry(fileId: string) {
  const prev = canEditExpiry.get(fileId);
  if (prev !== undefined) window.clearTimeout(prev);
  const timer = window.setTimeout(() => {
    canEditExpiry.delete(fileId);
    // 期限切れで操作を隠す（引き直しの結果が来るまで出さない）。
    notify();
    if ((canEditWatchers.get(fileId) ?? 0) > 0) loadCanEdit(fileId);
  }, CAN_EDIT_TTL_MS);
  canEditExpiry.set(fileId, timer);
}

/// 期限内の結果で編集できると分かっているか（期限切れ・未取得・失敗は false）。
function isEditable(fileId: string): boolean {
  const c = canEditCache.get(fileId);
  return !!c && c.value && Date.now() - c.at < CAN_EDIT_TTL_MS;
}

/// 編集権限を引く（未取得・期限切れのときだけ）。失敗は「出さない」として記録し、期限で切れたら引き直す。
function loadCanEdit(fileId: string) {
  const cached = canEditCache.get(fileId);
  if (canEditPending.has(fileId)) return;
  if (cached && Date.now() - cached.at < CAN_EDIT_TTL_MS) return;
  canEditPending.add(fileId);
  void getCollabAccess(fileId)
    .then((a) => canEditCache.set(fileId, { value: a?.mode === "editor", at: Date.now() }))
    .catch(() => canEditCache.set(fileId, { value: false, at: Date.now() }))
    .finally(() => {
      canEditPending.delete(fileId);
      scheduleCanEditExpiry(fileId);
      notify();
    });
}

/// 表示中のファイルの権限を、マウント時・結果の期限・タブの再表示で引き直す。
function useKeepCanEditFresh(fileIds: string[]) {
  const key = fileIds.join(",");
  React.useEffect(() => {
    const ids = key ? key.split(",") : [];
    for (const id of ids) canEditWatchers.set(id, (canEditWatchers.get(id) ?? 0) + 1);
    const refresh = () => {
      for (const id of ids) loadCanEdit(id);
      // 期限切れで表示が変わる（操作を隠す）ことを購読者に知らせる。
      notify();
    };
    refresh();
    const onVisible = () => {
      if (document.visibilityState === "visible") refresh();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      document.removeEventListener("visibilitychange", onVisible);
      for (const id of ids) {
        const n = (canEditWatchers.get(id) ?? 1) - 1;
        if (n > 0) canEditWatchers.set(id, n);
        else canEditWatchers.delete(id);
      }
    };
  }, [key]);
}

/// 編集できるか（期限内の結果で editor と分かっているときだけ true）。
function useCanEdit(fileId: string): boolean {
  useKeepCanEditFresh([fileId]);
  const subscribe = React.useCallback((cb: () => void) => {
    listeners.add(cb);
    return () => {
      listeners.delete(cb);
    };
  }, []);
  return React.useSyncExternalStore(
    subscribe,
    () => isEditable(fileId),
    () => false,
  );
}

// --- グルーピング -------------------------------------------------------------------------------

/// 回答内の 1 ファイル分（この回答で作った版と、操作を出すカードの位置）。
type FileGroup = { fileId: string; name: string; versions: number[]; lastIndex: number; key: string };

/// 取り消せるカードをファイルごとにまとめる（この回答で作った版の昇順と、最後のカードの位置）。
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

/// 同じファイルの参照をまとめたときに出すカードの位置（ファイルごとに最後＝最新の版）。
/// 版が確定した作成・編集だけを 1 つに数え、移動・名前変更・提案版・版未確定はそれぞれ別に数える
/// （同じ種類どうしは最後の 1 枚）。形の崩れた参照は位置で残す。
export function latestRefIndices(docs: (DocumentRef | null)[]): Set<number> {
  const last = new Map<string, number>();
  docs.forEach((doc, i) => {
    if (!doc) {
      last.set(`#${i}`, i);
      return;
    }
    const kind = doc.moved
      ? "moved"
      : doc.proposal
        ? "proposal"
        : doc.version === null
          ? "unsettled"
          : "content";
    last.set(`${doc.id}:${kind}`, i);
  });
  return new Set(last.values());
}

/// この回答で新規作成したファイル（後から編集してまとめたカードでも「作成した」ことを残す）。
function createdFileIds(docs: (DocumentRef | null)[]): Set<string> {
  return new Set(docs.filter((d): d is DocumentRef => !!d && d.created && !d.moved).map((d) => d.id));
}

/// 作成した後に編集したファイルのカードの状態（最後のカードは「編集しました」になるため）。
function createdThenEdited(doc: DocumentRef, created: Set<string>): string | undefined {
  if (doc.created || doc.moved || doc.proposal || !created.has(doc.id)) return undefined;
  return doc.version === null ? "作成し、編集しました" : `作成し、編集しました（v${doc.version}）`;
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
/// 同じキーの実行中・対象の版が無いときは何も送らず `false` を返す（画面内ストアで判定＝行が
/// 再マウントされても二重に送らない）。実際に送って成功したら `true`。
async function runRevert(group: FileGroup, mode: "undo" | "redo", force: boolean): Promise<boolean> {
  if (inflight.has(group.key)) return false;
  const st = revertStates.get(group.key) ?? EDITED;
  const versions = targetVersions(group, st, mode);
  if (versions.length === 0) return false;
  inflight.add(group.key);
  notify();
  try {
    const res = await revertEdit(group.fileId, versions, { force });
    if (mode === "redo") {
      setRevertState(group.key, { phase: "redone", version: res.node.version });
    } else if (res.reverted) {
      setRevertState(group.key, {
        phase: "reverted",
        undoVersion: res.node.version,
        restoredFrom: res.restored_from_version ?? null,
      });
    } else {
      // 既に戻っていた（何もしていない）。この画面で戻した記録があればそれを保つ
      // （「やり直す」の対象＝取り消しで作った版を失わない）。
      const prev = revertStates.get(group.key);
      if (prev?.phase !== "reverted") {
        setRevertState(group.key, { phase: "reverted", undoVersion: res.node.version, restoredFrom: null });
      }
    }
    return true;
  } finally {
    inflight.delete(group.key);
    notify();
  }
}

/// 回答 1 つ分のカードを並べ、ファイル単位の取り消し・一括取り消し・競合の確認ダイアログを持つ。
export function DocumentRefList({ raws }: { raws: unknown[] }) {
  const docs = React.useMemo(() => raws.map(parseDocumentRef), [raws]);
  const groups = React.useMemo(() => groupByFile(docs), [docs]);
  const visible = React.useMemo(() => latestRefIndices(docs), [docs]);
  const created = React.useMemo(() => createdFileIds(docs), [docs]);
  const [batchBusy, setBatchBusy] = React.useState(false);
  /// 後続の版がある取り消しの確認（どのファイルの、どの操作か）。
  const [conflict, setConflict] = React.useState<{
    group: FileGroup;
    mode: "undo" | "redo";
    current: number;
  } | null>(null);

  const run = async (group: FileGroup, mode: "undo" | "redo", force = false) => {
    try {
      await runRevert(group, mode, force);
    } catch (e) {
      if (e instanceof RevertConflict) {
        setConflict({ group, mode, current: e.currentVersion });
      } else {
        toast({ variant: "destructive", title: "元に戻せませんでした", description: failureMessage(e) });
      }
    }
  };

  /// この回答の編集をまとめて取り消す（ファイルごと・まだ戻していないものだけ）。
  const undoAll = async () => {
    setBatchBusy(true);
    let ok = 0;
    const skipped: string[] = [];
    for (const g of groups) {
      if (!isEditable(g.fileId)) continue;
      if ((revertStates.get(g.key) ?? EDITED).phase === "reverted") continue;
      try {
        // 個別の操作が実行中のファイルは送らない（成功として数えない・結果はそのカードに出る）。
        if (await runRevert(g, "undo", false)) ok += 1;
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
      if (ok > 0) {
        toast({ title: "元に戻しました", description: `${ok} 件のファイルを編集前に戻しました。` });
      }
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
        if (!visible.has(i)) return null;
        const raw = raws[i];
        const group = doc && !doc.moved ? groups.find((g) => g.fileId === doc.id) : undefined;
        const createdStatus = doc ? createdThenEdited(doc, created) : undefined;
        if (!doc || !group || group.lastIndex !== i) {
          return <DocumentRefCard key={i} raw={raw} status={createdStatus} />;
        }
        return (
          <FileRefCard
            key={i}
            raw={raw}
            doc={doc}
            group={group}
            createdStatus={createdStatus}
            batchBusy={batchBusy}
            onRun={(mode) => void run(group, mode)}
          />
        );
      })}
      <UndoAllButton groups={groups} busy={batchBusy} onClick={() => void undoAll()} />

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

/// 取り消し対象のファイルのカード（そのファイルの最後＝最新の版の 1 枚）。
function FileRefCard({
  raw,
  doc,
  group,
  createdStatus,
  batchBusy,
  onRun,
}: {
  raw: unknown;
  doc: DocumentRef;
  group: FileGroup;
  /// この回答で作成してから編集したときの状態表示。
  createdStatus: string | undefined;
  batchBusy: boolean;
  onRun: (mode: "undo" | "redo") => void;
}) {
  const st = useRevertState(group.key);
  const busy = useInflight(group.key) || batchBusy;
  const canEdit = useCanEdit(group.fileId);
  const status =
    st.phase === "reverted"
      ? st.restoredFrom === null
        ? "元に戻っています"
        : `元に戻しました（v${st.restoredFrom} の内容を v${st.undoVersion} として保存）`
      : st.phase === "redone"
        ? `やり直しました（v${st.version}）`
        : undefined;
  const multi = group.versions.length > 1;
  // 作成してから編集したものも、複数回なら回数を出す（「作成し、編集しました」だけだと 1 回に見える）。
  const edited =
    status ??
    (multi
      ? `${createdStatus ? "作成し、" : ""}この回答で ${group.versions.length} 回編集しました（v${doc.version}）`
      : createdStatus);
  if (!canEdit) {
    // 編集権限が無い（または未確認）なら操作は出さない。
    return <DocumentRefCard raw={raw} status={edited} />;
  }
  return (
    <DocumentRefCard
      raw={raw}
      status={edited}
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
  const noteId = React.useId();
  useKeepCanEditFresh(groups.map((g) => g.fileId));
  const pending = React.useSyncExternalStore(
    subscribe,
    () =>
      groups.filter(
        (g) =>
          isEditable(g.fileId) &&
          (revertStates.get(g.key) ?? EDITED).phase !== "reverted",
      ).length,
    () => 0,
  );
  if (pending < 2) return null;
  return (
    <div className="-mt-1 mb-2 flex flex-col items-end gap-0.5">
      <Button
        type="button"
        variant="ghost"
        size="sm"
        className="h-7 px-2 text-xs text-muted-foreground"
        loading={busy}
        disabled={busy}
        onClick={onClick}
        aria-describedby={noteId}
        data-testid="document-ref-undo-all"
      >
        {!busy ? <Undo2 className="size-3.5" aria-hidden /> : null}
        この回答の編集をまとめて元に戻す
      </Button>
      {/* ファイルをまたいだ一括の原子性は無い。押す前に分かるようにする。 */}
      <p id={noteId} className="px-2 text-[11px] text-muted-foreground/80">
        ファイルごとに戻します。後から更新されたファイルなどは戻さず、理由を表示します。
      </p>
    </div>
  );
}

/// カード右側の小さな操作ボタン（狭幅ではアイコンだけ）。
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
