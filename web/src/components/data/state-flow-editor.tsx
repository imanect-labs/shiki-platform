"use client";

/// テーブルの設定の「状態の進め方」（GAP-44・D-84・D-115）: 状態の列と状態の一覧、どの状態からどの状態へ誰が
/// 進められるかを決める。「承認を待つ」「状態を進める」ブロックと、状態が変わったときのきっかけで使う。
///
/// - 読み書きはテーブルの持ち主だけ（サーバも持ち主でなければ 403）。状態の列の選択肢は、保存した状態の一覧になる。
/// - 承認・却下・差し戻しにあたる進め方は「申請した本人は除く」をはじめからオンにする（最初の状態へ戻す進め方を
///   除く）。最初の状態を過ぎたら申請者が書き換えられない決まりも、はじめからオンにする。どちらもオフにできる。
///   申請者の列は作成者の列を勧め、無ければボタン 1 つで足せる（D-122・[`ApplicantSection`]）。
/// - 進め方が無いうちは「申請・承認のひな形」を出す（承認する人の進め方と、差し戻しから「レコードを追加した人」が
///   出し直す進め方）。「レコードを追加した人」に許した進め方は、「追加だけ」で共有した人も自分のレコードで使える（D-127）。
/// - 保存の前に、行き止まり・届かない状態・重複を欄の下で言う（欄の誤りは読み上げで割り込まず、欄に結ぶ）。
///   承認を待っているレコードが判断できなくなる変更と、書き込みの決まりで誰も書き換えられなくなるレコードが
///   出る変更は確認してから保存し、このテーブルの状態を使うワークフローへの影響は注意で言う。
/// - 保存していない変更があるときは、閉じる・列の編集を開く・最新の内容を読み込む前に、変更が消えることを言う。

import { ArrowDown, ArrowUp, Loader2, Plus, Trash2, Workflow } from "lucide-react";
import Link from "next/link";
import * as React from "react";

import { ConfirmDialog } from "@/components/drive/dialogs";
import { ApplicantSection } from "@/components/data/state-flow-applicant";
import { MovesTable } from "@/components/data/state-flow-moves";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Sheet, SheetBody, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet";
import { toast } from "@/components/ui/use-toast";
import {
  DataApiError,
  getTable,
  getTableFsm,
  saveTableFsm,
  TABLE_NOT_FOUND,
  updateSchema,
  type DataTable,
  type FieldDef,
  type PendingApprovalMove,
  type TableFsmResponse,
} from "@/lib/data-api";
import { uniqueKey } from "@/lib/data-view";
import {
  applyTemplate,
  bodyOf,
  checkFlow,
  draftFrom,
  draftId,
  guessApprover,
  reachableNames,
  statesFromField,
  type FlowDraft,
} from "@/lib/state-flow";
import { cn } from "@/lib/utils";

type Loaded = { resp: TableFsmResponse; draft: FlowDraft; mismatch: boolean };

function Section({ title, children, testId }: { title: string; children: React.ReactNode; testId?: string }) {
  return (
    <section className="space-y-2" data-testid={testId}>
      <h3 className="text-sm font-semibold">{title}</h3>
      {children}
    </section>
  );
}

/// 欄の下の注記・注意・誤り。誤りは開いた直後から出るものもあるので、読み上げで割り込ませず（role="alert" に
/// しない）、`id` で欄の aria-describedby に結ぶ。
function Hint({ children, tone, id }: { children: React.ReactNode; tone?: "warn" | "error"; id?: string }) {
  return (
    <p
      id={id}
      className={cn(
        "text-[11px] leading-relaxed",
        tone === "error" ? "text-destructive" : tone === "warn" ? "text-[var(--season-autumn)]" : "text-muted-foreground",
      )}
    >
      {children}
    </p>
  );
}

function loadError(e: unknown): string {
  if (e instanceof DataApiError) {
    if (e.status === 403) return "状態の進め方を変えられるのは、テーブルの持ち主だけです。変えたいときは、持ち主に頼んでください。";
    if (e.status === 404) return TABLE_NOT_FOUND;
  }
  return "状態の進め方を読み込めませんでした。少し待ってから、もう一度開いてください。";
}

export function StateFlowEditor({
  open,
  onOpenChange,
  table,
  onSaved,
  onOpenSchema,
  onOpenShare,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  table: DataTable;
  onSaved: (t: DataTable) => void;
  /// 「列の編集」を開く（状態や申請者に使う列が無いとき）。
  onOpenSchema?: () => void;
  /// テーブルの共有を開く（申請する人に「追加だけ」で渡す）。持ち主でも共有できないテーブルでは無い。
  onOpenShare?: () => void;
}) {
  const fields = table.schema.fields;
  const [loaded, setLoaded] = React.useState<Loaded | null>(null);
  const [draft, setDraft] = React.useState<FlowDraft | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [saving, setSaving] = React.useState(false);
  const [saveError, setSaveError] = React.useState<string | null>(null);
  const [lostConfirm, setLostConfirm] = React.useState<PendingApprovalMove[] | null>(null);
  /// 書き込みの決まりで誰も書き換えられなくなるレコードの数（保存の前の確認・サーバが数える）。
  const [lockConfirm, setLockConfirm] = React.useState<{ count: number; more: boolean; pending: boolean } | null>(null);
  /// ほかの人が先にテーブルの設定を保存した（付録 C の §3.5）。
  const [conflict, setConflict] = React.useState(false);
  /// 保存していない変更を捨てる確認（閉じる・列の編集を開く）。
  const [confirmClose, setConfirmClose] = React.useState<"close" | "schema" | null>(null);
  /// 作成者の列を足している・足せなかった理由。
  const [addingCreator, setAddingCreator] = React.useState(false);
  const [addCreatorError, setAddCreatorError] = React.useState<string | null>(null);
  const tableRef = React.useRef(table);
  tableRef.current = table;

  const load = React.useCallback(() => {
    setLoaded(null);
    setDraft(null);
    setError(null);
    setSaveError(null);
    setConflict(false);
    setAddCreatorError(null);
    getTableFsm(tableRef.current.id)
      .then((resp) => {
        const d = draftFrom(resp, tableRef.current.schema.fields);
        setLoaded({ resp, ...d });
        setDraft(d.draft);
      })
      .catch((e: unknown) => setError(loadError(e)));
  }, []);
  React.useEffect(() => {
    if (open) load();
  }, [open, load]);

  const issues = React.useMemo(() => (draft ? checkFlow(draft) : null), [draft]);
  // 列の選択肢と状態が合っていなかったときは、読み込んだままでも保存できる（保存で選択肢を状態にそろえる）。
  const dirty = Boolean(draft && loaded && (loaded.mismatch || JSON.stringify(draft) !== JSON.stringify(loaded.draft)));
  /// 保存していない変更を捨てる確認が要るか（列の選択肢をそろえるだけの保存は、変更として数えない）。
  const unsaved = Boolean(draft && loaded && JSON.stringify(draft) !== JSON.stringify(loaded.draft));
  const hasFsm = Boolean(loaded?.resp.fsm);
  const selects = fields.filter((f) => f.type === "select");
  const label = (k: string | null) => fields.find((f) => f.key === k)?.display_name || k || "";
  const set = (patch: Partial<FlowDraft>) => setDraft((d) => (d ? { ...d, ...patch } : d));

  /// 承認を待っているレコードのうち、この下書きでは判断できなくなる組。
  const lost = React.useMemo(() => {
    if (!draft || !loaded) return [];
    const body = bodyOf(draft);
    return loaded.resp.pending_approvals.filter((p) => !body.transitions.some((t) => t.from === p.from && t.to === p.to));
  }, [draft, loaded]);
  /// このテーブルの状態を使うワークフローへの影響（保存は止めない）。
  const workflowNotes = React.useMemo(() => {
    if (!draft || !loaded) return [];
    const names = new Set(draft.states.map((s) => s.name.trim()));
    const reach = reachableNames(draft);
    return loaded.resp.workflows.flatMap((w) => {
      const missing = w.states.filter((s) => !names.has(s));
      const unreachable = w.states.filter((s) => names.has(s) && !reach.has(s));
      const parts = [
        ...(missing.length ? [`${missing.map((s) => `「${s}」`).join("、")}は状態にありません。`] : []),
        ...(unreachable.length ? [`${unreachable.map((s) => `「${s}」`).join("、")}へは、最初の状態から進めません。`] : []),
      ];
      return parts.length ? [{ id: w.id, name: w.name, text: parts.join("") }] : [];
    });
  }, [draft, loaded]);

  const requestClose = (o: boolean) => {
    if (o) return onOpenChange(true);
    if (unsaved && !saving) setConfirmClose("close");
    else onOpenChange(false);
  };
  /// 列の編集を開く（この画面は閉じる。保存していない変更があれば、消えることを確かめてから）。
  const openSchema = onOpenSchema
    ? () => {
        if (unsaved && !saving) return setConfirmClose("schema");
        onOpenChange(false);
        onOpenSchema();
      }
    : undefined;

  const addMove = () => {
    if (!draft) return;
    const named = draft.states.filter((s) => s.name.trim());
    const from = draft.moves.at(-1)?.from ?? named[0]?.id ?? "";
    const used = new Set(draft.moves.filter((m) => m.from === from).map((m) => m.to));
    const to = named.find((s) => s.id !== from && !used.has(s.id))?.id ?? "";
    // 最初の状態へ戻す進め方（出し直しなど）は申請者がするので、本人を除く既定をオフにする（D-115）。
    const toInitial = to !== "" && to === draft.states[0]?.id;
    set({
      moves: [
        ...draft.moves,
        {
          id: draftId("m"),
          from,
          to,
          actor: { kind: "simple", who: [guessApprover(fields, draft.applicantField)], excludeApplicant: !toInitial },
          autoExclude: true,
        },
      ],
    });
  };

  const save = async (confirm: { pending?: boolean; locked?: boolean }) => {
    if (!draft || !loaded || !draft.statusField || issues?.hasErrors) return;
    setSaving(true);
    setSaveError(null);
    setConflict(false);
    try {
      const saved = await saveTableFsm(table.id, {
        status_field: draft.statusField,
        body: bodyOf(draft),
        applicant_write_lock: draft.writeLock && draft.applicantField ? draft.applicantField : null,
        expected_schema_version: loaded.resp.schema_version,
        confirm_pending: confirm.pending ?? false,
        confirm_locked: confirm.locked ?? false,
      });
      toast({ title: "状態の進め方を保存しました" });
      onSaved(saved);
      onOpenChange(false);
    } catch (e) {
      const inUse = e instanceof DataApiError ? e.fsmInUse : null;
      if (inUse) {
        setLostConfirm(inUse);
        return;
      }
      const locks = e instanceof DataApiError ? e.fsmLocksRecords : null;
      if (locks) {
        setLockConfirm({ ...locks, pending: confirm.pending ?? false });
        return;
      }
      if (e instanceof DataApiError && e.status === 409) {
        setConflict(true);
      } else if (e instanceof DataApiError && e.status === 403) {
        setSaveError("状態の進め方を変えられるのは、テーブルの持ち主だけです。");
      } else {
        setSaveError(e instanceof Error ? e.message : String(e));
      }
    } finally {
      setSaving(false);
    }
  };
  /// 作成者の列を足して、申請者の列にする（列はすぐ保存する。状態の進め方は保存するまで変わらない）。
  ///
  /// 足す先は、押した時点のテーブルの定義を読み直したもの（画面のテーブルは、ほかのタブや AI の変更より古いことが
  /// ある）。送る中身と版の番号を同じ時点のものにし、ほかの人の列の変更を黙って元に戻さない。
  const addCreator = async () => {
    if (!loaded || !draft) return;
    setAddingCreator(true);
    setAddCreatorError(null);
    try {
      const latest = await getTable(table.id);
      const current = latest.schema.fields;
      const key = uniqueKey("created_by", new Set(current.map((f) => f.key)));
      const names = new Set(current.map((f) => f.display_name || f.key));
      const name = names.has("作成者") ? "作成者（自動）" : "作成者";
      const field = { key, display_name: name, type: "created_by" } as FieldDef;
      const t = await updateSchema(table.id, { ...latest.schema, fields: [...current, field] }, latest.schema_version);
      onSaved(t);
      // 状態の進め方を読んだ後にテーブルの設定が変わっていなければ、状態の進め方の保存は新しいスキーマの版で送る
      // （列を足した保存と食い違わせない）。変わっていたら、そのまま送って「ほかの人が先に保存した」にする。
      setLoaded((l) =>
        l && l.resp.schema_version === latest.schema_version ? { ...l, resp: { ...l.resp, schema_version: t.schema_version } } : l,
      );
      set({ applicantField: key, creatorField: key });
      toast({ title: `「${name}」の列を追加しました`, description: "状態の進め方を保存すると、申請した本人を除くのに使います。" });
    } catch (e) {
      setAddCreatorError(
        e instanceof DataApiError && e.status === 409
          ? "ほかの人が同時にテーブルの設定を保存したため、列を追加できませんでした。もう一度追加してください。"
          : e instanceof DataApiError && e.status === 403
            ? "列を追加できるのは、テーブルの持ち主だけです。"
            : e instanceof DataApiError && e.status === 400
              ? `作成者の列を追加できませんでした。${e.message}`
              : "作成者の列を追加できませんでした。少し待ってから、もう一度追加してください。",
      );
    } finally {
      setAddingCreator(false);
    }
  };

  const trySave = () => {
    if (lost.length > 0) setLostConfirm(lost);
    else void save({});
  };

  const initialName = draft?.states[0]?.name.trim() || "最初の状態";

  return (
    <Sheet open={open} onOpenChange={requestClose}>
      <SheetContent side="right" className="w-full sm:max-w-3xl" data-testid="state-flow-editor">
        <SheetHeader className="pr-14">
          <SheetTitle>状態の進め方</SheetTitle>
          <SheetDescription>
            レコードの状態を、どの状態からどの状態へ、誰が進められるかを決めます（例: 申請中 → 承認済み）。「承認を待つ」「状態を進める」ブロックと、状態が変わったときのきっかけで使います。
          </SheetDescription>
        </SheetHeader>
        <SheetBody className="space-y-6 px-4 pb-4">
          {error ? (
            <p className="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive" role="alert" data-testid="state-flow-load-error">
              {error}
            </p>
          ) : !draft || !loaded || !issues ? (
            <p className="flex items-center gap-2 text-sm text-muted-foreground" role="status">
              <Loader2 className="size-4 animate-spin" aria-hidden />
              読み込み中…
            </p>
          ) : (
            <>
              <Section title="状態の列" testId="state-flow-status">
                {selects.length === 0 ? (
                  <div className="space-y-2 rounded-lg border border-dashed p-3">
                    <Hint>状態に使える選択肢の列がありません。「列の編集」で選択肢の列（例: 状態）を追加してください。</Hint>
                    {openSchema ? (
                      <Button size="sm" variant="outline" onClick={openSchema}>
                        列の編集を開く
                      </Button>
                    ) : null}
                  </div>
                ) : (
                  <>
                    <Select
                      value={draft.statusField ?? undefined}
                      disabled={hasFsm}
                      onValueChange={(k) => set({ statusField: k, states: statesFromField(fields, k), moves: [] })}
                    >
                      <SelectTrigger
                        aria-label="状態の列"
                        aria-describedby={!draft.statusField ? "state-flow-status-error" : undefined}
                        className="h-9 sm:w-72"
                      >
                        <SelectValue placeholder="選んでください" />
                      </SelectTrigger>
                      <SelectContent>
                        {selects.map((f) => (
                          <SelectItem key={f.key} value={f.key}>
                            {f.display_name || f.key}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                    <Hint>
                      {hasFsm
                        ? "状態の進め方を作った後は、状態の列を変えられません。"
                        : "レコードの状態を入れる選択肢の列です。保存すると、この列は状態を進める操作でだけ変わります。"}
                    </Hint>
                    {!draft.statusField ? (
                      <Hint tone="error" id="state-flow-status-error">
                        状態の列を選んでください。
                      </Hint>
                    ) : null}
                  </>
                )}
              </Section>

              {draft.statusField ? (
                <>
                  <Section title="状態" testId="state-flow-states">
                    <Hint>
                      いちばん上の状態が、レコードを追加したときの状態（最初の状態）です。ここで変えた状態は、「{label(draft.statusField)}」の選択肢になります。
                    </Hint>
                    {loaded.mismatch ? (
                      <Hint tone="warn">
                        「{label(draft.statusField)}」の選択肢と状態の進め方の状態が合っていなかったため、両方を合わせて出しています。保存すると、選択肢がこの一覧になります。
                      </Hint>
                    ) : null}
                    <ol className="space-y-1.5">
                      {draft.states.map((s, i) => {
                        const err = issues.stateErrors.get(s.id);
                        const warn = issues.stateWarnings.get(s.id);
                        return (
                          <li key={s.id} className="space-y-0.5" data-testid="state-flow-state">
                            <div className="flex items-center gap-1.5">
                              <Input
                                value={s.name}
                                maxLength={128}
                                aria-label={`状態 ${i + 1} の名前`}
                                aria-invalid={err ? true : undefined}
                                aria-describedby={err || warn ? `state-note-${s.id}` : undefined}
                                onChange={(e) =>
                                  set({ states: draft.states.map((x) => (x.id === s.id ? { ...x, name: e.target.value } : x)) })
                                }
                                className={cn("h-8 min-w-0 flex-1 text-[13px]", err && "border-destructive")}
                              />
                              {i === 0 ? (
                                <Badge variant="secondary" className="shrink-0">
                                  最初の状態
                                </Badge>
                              ) : issues.ends.has(s.id) ? (
                                <Badge variant="muted" className="shrink-0">
                                  終わり
                                </Badge>
                              ) : null}
                              <Button
                                variant="ghost"
                                size="icon"
                                className="size-7 shrink-0 text-muted-foreground pointer-coarse:size-9"
                                aria-label={`「${s.name || `状態 ${i + 1}`}」を上へ`}
                                disabled={i === 0}
                                onClick={() => {
                                  const n = [...draft.states];
                                  [n[i - 1], n[i]] = [n[i], n[i - 1]];
                                  set({ states: n });
                                }}
                              >
                                <ArrowUp className="size-3.5" aria-hidden />
                              </Button>
                              <Button
                                variant="ghost"
                                size="icon"
                                className="size-7 shrink-0 text-muted-foreground pointer-coarse:size-9"
                                aria-label={`「${s.name || `状態 ${i + 1}`}」を下へ`}
                                disabled={i === draft.states.length - 1}
                                onClick={() => {
                                  const n = [...draft.states];
                                  [n[i], n[i + 1]] = [n[i + 1], n[i]];
                                  set({ states: n });
                                }}
                              >
                                <ArrowDown className="size-3.5" aria-hidden />
                              </Button>
                              <Button
                                variant="ghost"
                                size="icon"
                                className="size-7 shrink-0 text-muted-foreground hover:text-destructive pointer-coarse:size-9"
                                aria-label={`「${s.name || `状態 ${i + 1}`}」を削除`}
                                onClick={() =>
                                  set({
                                    states: draft.states.filter((x) => x.id !== s.id),
                                    moves: draft.moves.filter((m) => m.from !== s.id && m.to !== s.id),
                                  })
                                }
                              >
                                <Trash2 className="size-3.5" aria-hidden />
                              </Button>
                            </div>
                            {err ? (
                              <Hint tone="error" id={`state-note-${s.id}`}>
                                {err}
                              </Hint>
                            ) : warn ? (
                              <Hint tone="warn" id={`state-note-${s.id}`}>
                                {warn}
                              </Hint>
                            ) : null}
                          </li>
                        );
                      })}
                    </ol>
                    <Button
                      variant="outline"
                      size="sm"
                      className="w-full border-dashed"
                      onClick={() => set({ states: [...draft.states, { id: draftId("s"), name: "" }] })}
                    >
                      <Plus className="size-4" aria-hidden />
                      状態を追加
                    </Button>
                    <Hint>使っているレコードがある状態は、削除や名前の変更をすると保存できません。</Hint>
                  </Section>

                  <Section title="申請した本人の扱い" testId="state-flow-applicant">
                    <ApplicantSection
                      draft={draft}
                      fields={fields}
                      initialName={initialName}
                      onChange={set}
                      onAddCreator={() => void addCreator()}
                      adding={addingCreator}
                      addError={addCreatorError}
                      onOpenSchema={openSchema}
                      onOpenShare={onOpenShare}
                    />
                  </Section>

                  <Section title="進め方" testId="state-flow-moves-section">
                    <Hint>
                      進められるのは、テーブルを編集できる人のうち、ここで選んだ人だけです。「承認を待つ」の承認者も、ここで許された人だけが判断できます。
                      {draft.creatorField
                        ? "「追加だけ」で共有した人は、「レコードを追加した人」を選んだ進め方だけを、自分が追加したレコードで進められます。"
                        : "「追加だけ」で共有した人は進められません。"}
                    </Hint>
                    {draft.moves.length === 0 ? (
                      <div className="space-y-1.5 rounded-lg border border-dashed p-3" data-testid="state-flow-template">
                        <Hint>
                          申請と承認に使うなら、ひな形から始められます。「申請中」「承認」「却下」「差し戻し」の状態と、承認する人が判断する進め方、差し戻されたレコードを追加した人が直して出し直す進め方を入れます。保存する前に変えられます。
                        </Hint>
                        <Button
                          size="sm"
                          variant="outline"
                          className="h-7 text-xs pointer-coarse:h-9"
                          onClick={() => setDraft((d) => (d ? applyTemplate(d, fields) : d))}
                          data-testid="state-flow-use-template"
                        >
                          申請・承認のひな形を使う
                        </Button>
                      </div>
                    ) : null}
                    <MovesTable
                      draft={draft}
                      fields={fields}
                      errors={issues.moveErrors}
                      pending={loaded.resp.pending_approvals}
                      onChange={(moves) => set({ moves })}
                      onAdd={addMove}
                    />
                    {issues.errors.map((e) => (
                      <Hint key={e} tone="error">
                        {e}
                      </Hint>
                    ))}
                  </Section>

                  {lost.length > 0 || workflowNotes.length > 0 ? (
                    <Section title="影響を受けるもの" testId="state-flow-impact">
                      {lost.length > 0 ? (
                        <div className="space-y-1 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2" data-testid="state-flow-lost">
                          <Hint tone="error">保存すると、承認を待っている次のレコードが判断できなくなります。</Hint>
                          <ul className="list-disc space-y-0.5 pl-5 text-[11px] text-destructive">
                            {lost.map((p) => (
                              <li key={`${p.from}>${p.to}`}>
                                「{p.from}」から「{p.to}」（{p.count.toLocaleString("ja-JP")} 件）
                              </li>
                            ))}
                          </ul>
                        </div>
                      ) : null}
                      {workflowNotes.length > 0 ? (
                        <ul className="space-y-1" data-testid="state-flow-workflows">
                          {workflowNotes.map((w) => (
                            <li key={w.id} className="flex items-start gap-1.5 text-[11px] leading-relaxed text-[var(--season-autumn)]">
                              <Workflow className="mt-0.5 size-3.5 shrink-0" aria-hidden />
                              <span>
                                <Link href={`/workflows/${w.id}`} className="font-medium underline-offset-2 hover:underline" target="_blank">
                                  {w.name}
                                </Link>
                                : {w.text}
                              </span>
                            </li>
                          ))}
                        </ul>
                      ) : null}
                      {workflowNotes.length > 0 ? (
                        <Hint>ワークフローは、あなたが見られるもののうち最近更新したものから探しています。ほかにもあることがあります。</Hint>
                      ) : null}
                    </Section>
                  ) : null}
                </>
              ) : null}
            </>
          )}
        </SheetBody>
        <div className="space-y-2 border-t px-4 py-3">
          {conflict ? (
            <div
              className="space-y-1.5 rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-[12px]"
              role="alert"
              data-testid="state-flow-save-error"
            >
              <p className="font-medium text-destructive">ほかの人が先にテーブルの設定を保存しました</p>
              <p className="text-destructive">
                あなたの変更は保存されていません。最新の内容を読み込むと、保存していない変更は消えます。残したい内容は先に控えてください。
              </p>
              <Button size="sm" variant="outline" className="h-7 text-xs pointer-coarse:h-9" onClick={load}>
                最新の内容を読み込む（変更は消えます）
              </Button>
            </div>
          ) : saveError ? (
            <p className="text-[12px] text-destructive" role="alert" data-testid="state-flow-save-error">
              {saveError}
            </p>
          ) : issues?.hasErrors ? (
            <p className="text-[12px] text-muted-foreground" role="status">
              直す箇所があるため、保存できません。欄の下の誤りを直してください。
            </p>
          ) : null}
          <div className="flex items-center justify-end gap-2">
            <Button variant="outline" onClick={() => requestClose(false)}>
              閉じる
            </Button>
            <Button
              onClick={trySave}
              disabled={!draft || !draft.statusField || !dirty || saving || Boolean(issues?.hasErrors)}
              data-testid="state-flow-save"
            >
              {saving ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
              保存
            </Button>
          </div>
        </div>
        <ConfirmDialog
          open={confirmClose !== null}
          onOpenChange={(o) => !o && setConfirmClose(null)}
          title="保存していない変更があります"
          description={
            confirmClose === "schema"
              ? "列の編集を開くと、この画面を閉じます。保存していない状態の進め方の変更は消えます。"
              : "状態の進め方の変更を保存せずに閉じますか？保存していない変更は消えます。"
          }
          confirmLabel={confirmClose === "schema" ? "変更を捨てて列の編集を開く" : "変更を捨てて閉じる"}
          destructive
          onConfirm={async () => {
            const then = confirmClose;
            setConfirmClose(null);
            onOpenChange(false);
            if (then === "schema") onOpenSchema?.();
          }}
        />
        <ConfirmDialog
          open={lostConfirm !== null}
          onOpenChange={(o) => !o && setLostConfirm(null)}
          title="承認を待っているレコードを判断できなくしますか？"
          description={`承認を待っている ${(lostConfirm ?? []).reduce((n, p) => n + p.count, 0).toLocaleString("ja-JP")} 件は、判断しても状態を進められなくなります。承認の期限か実行の期限が来るまで、「承認」の画面に残ります。`}
          confirmLabel="それでも保存"
          destructive
          onConfirm={async () => {
            setLostConfirm(null);
            await save({ pending: true });
          }}
        />
        <ConfirmDialog
          open={lockConfirm !== null}
          onOpenChange={(o) => !o && setLockConfirm(null)}
          title={`${lockConfirm?.count.toLocaleString("ja-JP") ?? 0} 件${lockConfirm?.more ? "以上" : ""}のレコードを書き換えられなくしますか？`}
          description={`「${label(draft?.applicantField ?? null)}」が空で「${initialName}」を過ぎたレコードは、保存すると誰も書き換えや削除ができなくなります。ワークフローからの書き換えと削除も断られます。「最初の状態を過ぎたら、申請者は書き換えられない」をオフにして保存し直すと戻せます。`}
          confirmLabel="それでも保存"
          destructive
          onConfirm={async () => {
            const pending = lockConfirm?.pending ?? false;
            setLockConfirm(null);
            await save({ pending, locked: true });
          }}
        />
      </SheetContent>
    </Sheet>
  );
}
