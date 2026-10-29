"use client";

/// 「承認を待つ」（human.approval・GAP-08）のフォーム。
///
/// 承認は状態の遷移の規則（FSM）で管理するテーブルの 1 行に束縛する。判断は担当者本人がその行の状態を
/// 進めること（誰が判断できるかはテーブルの遷移の規則が決める）で、ここで決めるのは受信箱に出す相手・判断の
/// 行き先の状態・受信箱に出す列・期限だけ。テーブルは FSM で管理するものだけを選べる。

import * as React from "react";
import { Plus, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import type { NodeParamsByType } from "@/generated/workflow-catalog";
import type {
  ApprovalAssignee,
  ApprovalChoice,
  ApprovalComment,
  ValueExpr,
} from "@/generated/workflow-ir";
import { getViewerContext, type DataTable } from "@/lib/data-api";
import { cn } from "@/lib/utils";
import { Field, FieldInput, FieldTextarea, MoreInfo, NumberInput } from "./common";
import { DataTableSelect, isFsmTable, missingFsmTables, useDataTables, type TableLookup } from "./data-tables";
import { nodeErrorView } from "../validation";
import type { FormProps, ServerCheck } from "./forms-basic";
import { paramsOf, patchParams } from "./params";
import { ValueExprInput } from "./value-expr-input";

type Params = NodeParamsByType["human.approval"];
type ChoiceKey = "approve" | "reject" | "return";

const CHOICES: { key: ChoiceKey; title: string; defaultLabel: string; optional: boolean }[] = [
  { key: "approve", title: "承認したとき", defaultLabel: "承認する", optional: false },
  { key: "reject", title: "却下したとき", defaultLabel: "却下する", optional: true },
  { key: "return", title: "差し戻したとき", defaultLabel: "差し戻す", optional: true },
];

const MAX_ASSIGNEES = 20;
const MAX_SHOW_FIELDS = 30;

/// 承認するテーブルを束縛したのに、その定義を使えないときの、テーブルに依存する欄（進める状態・相手の列）の注記。
/// 束縛していなければ null（それぞれの欄の「先にテーブルを選んでください」のまま）。
function boundTableNote(found: TableLookup): string | null {
  switch (found.state) {
    case "none":
      return null;
    case "loading":
      return "読み込み中…";
    case "ok":
      return isFsmTable(found.table) ? null : "状態の遷移の規則があるテーブルを選んでください";
    default:
      return "テーブルを読めないため選べません";
  }
}

/// FSM で管理するテーブルの状態（状態の列の選択肢）。
function statesOf(table: DataTable | null): string[] {
  if (!table?.schema.fsm_ref || !table.schema.status_field) return [];
  return table.schema.fields.find((f) => f.key === table.schema.status_field)?.options ?? [];
}

/// FSM で管理するテーブルなのに、状態の列が自分に読めない（owner 以外に返る定義は読めない列を落とし、
/// 状態の列が読めなければ `status_field` も外す・data の redact_table）。状態を選べず、保存もサーバの照合
/// （保存者の見え方で行う）が ir.unknown_table で止める。
function statusUnreadable(table: DataTable | null): boolean {
  if (!table?.schema.fsm_ref) return false;
  const key = table.schema.status_field;
  return !key || !table.schema.fields.some((f) => f.key === key);
}

/// 承認するテーブルを変える 1 手（Undo 1 段）。前のテーブルの状態名を指す判断の行き先・列を指す受信箱の列と
/// 担当者は、新しいテーブルに無ければ空に戻す（残すと選択肢に無い値は空欄に見え、気づかないまま実行で
/// 無い状態への遷移になる）。新しいテーブルにもある値は残す。
function retargetTable(
  p: Partial<Params>,
  record: NonNullable<Params["record"]>,
  next: DataTable | null,
  id: string,
): Partial<Record<keyof Params, unknown>> {
  const patch: Partial<Record<keyof Params, unknown>> = { record: { ...record, table: id } };
  const states = statesOf(next);
  const keys = new Set((next?.schema.fields ?? []).map((f) => f.key));
  const refKeys = new Set(
    (next?.schema.fields ?? []).filter((f) => f.type === "user_ref" || f.type === "role_ref").map((f) => f.key),
  );
  for (const c of CHOICES) {
    const choice = p[c.key] as ApprovalChoice | undefined;
    if (choice && choice.to && !states.includes(choice.to)) patch[c.key] = { ...choice, to: "" };
  }
  if (p.show_fields?.length) {
    const kept = p.show_fields.filter((k) => keys.has(k) && k !== next?.schema.status_field);
    if (kept.length !== p.show_fields.length) patch.show_fields = kept.length ? kept : undefined;
  }
  if (p.assignees?.some((a) => kindOf(a) === "field" && a.field && !refKeys.has(a.field))) {
    patch.assignees = p.assignees.map((a) =>
      kindOf(a) === "field" && a.field && !refKeys.has(a.field) ? { field: "" } : a,
    );
  }
  return patch;
}

type AssigneeKind = "field" | "user" | "role";

function kindOf(a: ApprovalAssignee): AssigneeKind {
  if (a.user !== undefined) return "user";
  if (a.role !== undefined) return "role";
  return "field";
}

/// テーブルの定義の全体が見えているか（スキーマ管理者＝owner・viewer-context の `can_manage`）。
///
/// owner 以外に返る定義は読めない列を落としてあり（data の redact_table）、読めない列の名前も返らないので、
/// 定義に無い列が「テーブルに無い」のか「読めない」のかを画面では決められない。読み込み中・読めないときは
/// 見えていないものとして扱う（「無い」と断定しない側へ倒す）。
function useSeesWholeTable(tableId: string | null): boolean {
  const [whole, setWhole] = React.useState<{ id: string; value: boolean } | null>(null);
  React.useEffect(() => {
    if (!tableId) return;
    let active = true;
    getViewerContext(tableId)
      .then((v) => active && setWhole({ id: tableId, value: v.can_manage }))
      .catch(() => active && setWhole({ id: tableId, value: false }));
    return () => {
      active = false;
    };
  }, [tableId]);
  return whole?.id === tableId && whole.value;
}

/// 受信箱に出す列の欄の下に出すサーバの誤り（最初の 1 件）。
///
/// 選べない列の行の注記（外すボタン付き）が同じことを断定して言う列を指す誤りは、欄の下には重ねない（1 回だけ言う）:
/// - 状態の列（保存時の照合の `ir.bad_show_field`・BUG-82）。
/// - 定義の全体が見えているとき（`seesWholeTable`）の、テーブルに無い列（`ir.unknown_field`）。行の注記は
///   「このテーブルに無い列」と言う。全体が見えないときは、注記が断定しない（読めないだけかもしれない）ので、
///   サーバの文言を残す。
/// どの列を指すかは、誤りの path の添字を検証した時点の値で引く（検証の後に並びが変わっても、今の並びに当てて
/// 取り違えない）。
function showFieldsServerError(
  check: ServerCheck,
  table: DataTable | null,
  seesWholeTable: boolean,
): string | undefined {
  const checked = (check.node?.params as Partial<Params> | undefined)?.show_fields ?? [];
  const statusField = table?.schema.status_field;
  const keys = new Set((table?.schema.fields ?? []).map((f) => f.key));
  const saidByNote = (path: string | null | undefined) => {
    const m = /^\/params\/show_fields\/(\d+)$/.exec(path ?? "");
    const key = m ? checked[Number(m[1])] : undefined;
    if (key === undefined) return false;
    if (statusField && key === statusField) return true;
    return table !== null && seesWholeTable && !keys.has(key);
  };
  return nodeErrorView(check.errors.filter((e) => !saidByNote(e.path))).fields.get("show_fields");
}

/// 選べる列に無い保存済みの列の注記（`certain` は赤字で出す）。`missing`（定義の全体が見えている）でなければ、
/// 読めないだけで実在する列かもしれないので「無い」と断定しない（サーバの ir.unknown_field も、無い列と読めない列を
/// 区別せず「このテーブルに無いか、読めない列です」と返す）。
function unselectableNote(key: string, missing: boolean): { text: string; certain: boolean } {
  return missing
    ? { text: `このテーブルに無い列: ${key}`, certain: true }
    : { text: `この画面では選べない列（テーブルに無いか、あなたが読めない列）: ${key}`, certain: false };
}

export function HumanApprovalForm(props: FormProps) {
  const { node, dispatch, refCandidates, inMapRegion, fieldErrors, serverCheck } = props;
  const p = paramsOf<Params>(node);
  // 束縛済みのテーブルが一覧（200 件で打ち切る）に無くても、1 件取って足す（BUG-68）。
  const source = useDataTables({ bound: [p.record?.table] });
  const found = source.lookup(p.record?.table);
  const table = found.state === "ok" && isFsmTable(found.table) ? found.table : null;
  const tableNote = boundTableNote(found);
  // 読み込んだ分に状態の遷移の規則があるテーブルが無い。一覧を打ち切っている（続きがある）ときは、先頭のページの外に
  // あるかもしれないので「作ってください」と断定せず、名前で探すよう促す（GAP-42）。
  const noFsmTable = missingFsmTables(source);
  const seesWholeTable = useSeesWholeTable(table?.id ?? null);
  const states = statesOf(table);
  const unreadableStates = statusUnreadable(table);
  const set = (patch: Partial<Record<keyof Params, unknown>>) => patchParams(dispatch, node, patch);
  const record = p.record ?? { table: "", id: { $from: "trigger", path: "/record_id" } as ValueExpr };

  return (
    <div className="space-y-4">
      <Field label="受信箱の件名" error={fieldErrors.get("title")} hint="担当者の受信箱に出る名前（120 字まで）">
        <FieldInput
          value={p.title ?? ""}
          maxLength={120}
          placeholder="例: 経費申請の承認"
          onChange={(e) => set({ title: e.target.value })}
          className="h-8"
        />
      </Field>
      <Field label="担当者への説明（省略可）" error={fieldErrors.get("instructions")}>
        <FieldTextarea
          value={p.instructions ?? ""}
          maxLength={2000}
          rows={2}
          placeholder="例: 規程どおりか確認してください。"
          onChange={(e) => set({ instructions: e.target.value || undefined })}
          className="text-[13px]"
        />
      </Field>

      <Field
        label="承認するテーブル"
        error={fieldErrors.get("record")}
        hint="状態の遷移の規則（FSM）で管理するテーブルだけを選べます。判断は担当者がこの表の状態を進めることです"
      >
        <DataTableSelect
          value={record.table || null}
          source={source}
          eligible={isFsmTable}
          ineligibleNote="このテーブルは状態の遷移の規則（FSM）で管理していないので、承認には使えません"
          label="承認するテーブル"
          onChange={(id, t) => set(retargetTable(p, record, t, id))}
        />
      </Field>
      {noFsmTable ? (
        <p className="text-[11px] leading-relaxed text-muted-foreground" data-testid="approval-no-fsm-table">
          {noFsmTable === "none"
            ? "状態の遷移の規則があるテーブルがありません。テーブルの設定で状態の列と遷移の規則を作ってください。"
            : "読み込んだテーブルに、状態の遷移の規則があるものはありません。選ぶ欄で名前で探してください。"}
        </p>
      ) : null}
      <ValueExprInput
        label="承認する行"
        value={record.id}
        onChange={(v) => set({ record: { ...record, id: v } })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        placeholder="行の ID"
      />

      <AssigneesField
        {...props}
        table={table}
        assignees={p.assignees ?? []}
        seesWholeTable={seesWholeTable}
        tableNote={tableNote}
        error={fieldErrors.get("assignees")}
        onChange={(assignees) => set({ assignees })}
      />

      {CHOICES.map((c) => {
        const value = p[c.key] as ApprovalChoice | undefined;
        return (
          <ChoiceField
            key={c.key}
            title={c.title}
            defaultLabel={c.defaultLabel}
            optional={c.optional}
            value={value}
            states={states}
            statesUnreadable={unreadableStates}
            tableNote={tableNote}
            error={fieldErrors.get(c.key)}
            onChange={(next) => set({ [c.key]: next })}
          />
        );
      })}

      <ShowFieldsField
        table={table}
        value={p.show_fields ?? []}
        error={showFieldsServerError(serverCheck, table, seesWholeTable)}
        seesWholeTable={seesWholeTable}
        onChange={(v) => set({ show_fields: v.length ? v : undefined })}
      />

      <Field
        label="判断の期限（時間・省略可）"
        error={fieldErrors.get("timeout_sec")}
        // 空欄は「期限なし」ではない: 実行の期限が来ると承認は取り消され、実行は失敗になる。承認者の画面にも、
        // 実行の期限を期限として出す（D-92・付録 D の N3.4.1）。
        hint="空欄なら、実行の期限まで待ちます。実行の期限が来ると、承認を待つのをやめて実行は失敗になります。"
      >
        <NumberInput
          value={typeof p.timeout_sec === "number" ? Math.round((p.timeout_sec / 3600) * 10) / 10 : null}
          min={1}
          placeholder="例: 72"
          onChange={(h) =>
            set(
              h === null || h <= 0
                ? { timeout_sec: undefined, on_timeout: undefined }
                : { timeout_sec: Math.round(h * 3600) },
            )
          }
        />
      </Field>
      {typeof p.timeout_sec === "number" ? (
        <Field label="期限を過ぎたら" error={fieldErrors.get("on_timeout")}>
          <Select
            value={p.on_timeout ?? "fail"}
            onValueChange={(v) => set({ on_timeout: v === "fail" ? undefined : v })}
          >
            <SelectTrigger aria-label="期限を過ぎたら" className="h-8 text-[13px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="fail">実行を失敗にする</SelectItem>
              <SelectItem value="continue">「時間切れ」の出口へ進む</SelectItem>
            </SelectContent>
          </Select>
        </Field>
      ) : null}
      <p className="text-[11px] leading-relaxed text-muted-foreground">
        判断の外で行の状態が動いた・行が消えたときは「取り下げ」の出口へ進みます。
      </p>
    </div>
  );
}

/// 判断 1 つ（行き先の状態・ボタンの文言・コメントの要否）。省略できる判断はスイッチで足す。
///
/// `statesUnreadable`（選んだテーブルの状態の列が自分に読めない）なら、状態を選ぶ欄の代わりにそう出す。状態の
/// 名前を手で書く欄にすると、選べない状態を当て推量で書かせることになり、保存もサーバの照合が止める。
/// `tableNote`（束縛したテーブルの定義を使えない・読み込み中）も同じく、手で書く欄の代わりにその注記を出す。
function ChoiceField({
  title,
  defaultLabel,
  optional,
  value,
  states,
  statesUnreadable,
  tableNote,
  error,
  onChange,
}: {
  title: string;
  defaultLabel: string;
  optional: boolean;
  value: ApprovalChoice | undefined;
  states: string[];
  statesUnreadable: boolean;
  tableNote: string | null;
  error?: string;
  onChange: (next: ApprovalChoice | undefined) => void;
}) {
  const on = value !== undefined;
  const v = value ?? { to: "" };
  // 保存済みの値がテーブルの状態に無い（テーブルの状態が変わった・AI が書いた）と、選択の欄は空に見える。
  const unknownState = states.length > 0 && v.to !== "" && !states.includes(v.to) ? v.to : null;
  // 状態の列が読めないときのサーバの誤り（「進める状態を選んでください」など）は、選べない欄の下に出しても
  // 直しようがないので出さない（保存が止まる理由はこの欄の注記と、テーブルの欄の ir.unknown_table が言う）。
  const stateError =
    statesUnreadable || tableNote
      ? undefined
      : (error ?? (unknownState ? `このテーブルに無い状態: ${unknownState}` : undefined));
  // 使わない（スイッチを切った）任意の出口は、中身の無い枠を出さず見出しの行だけにする。枠の上辺に載る見出しが
  // 空の枠に重なって見えるため（BUG-83）。見出しの文字の位置は、枠を出すときと揃える。
  const framed = on || !optional;
  return (
    <fieldset className={cn("space-y-2", framed ? "rounded-lg border p-2.5" : "px-2.5")} data-state={framed ? "on" : "off"}>
      <legend className="flex w-full items-center justify-between gap-2 px-1 text-xs font-medium">
        <span className={cn(!framed && "text-muted-foreground")}>{title}</span>
        {optional ? (
          <Switch
            aria-label={`${title}の出口を使う`}
            checked={on}
            onCheckedChange={(checked) => onChange(checked ? { to: "" } : undefined)}
          />
        ) : null}
      </legend>
      {framed ? (
        <>
          <Field label="進める状態" error={stateError}>
            {statesUnreadable ? (
              <p className="text-[11px] text-destructive">このテーブルの状態の列を読めません</p>
            ) : tableNote ? (
              <p className="text-[11px] text-muted-foreground">{tableNote}</p>
            ) : states.length ? (
              <Select value={v.to || ""} onValueChange={(to) => onChange({ ...v, to })}>
                <SelectTrigger aria-label={`${title}に進める状態`} className="h-8 text-[13px]">
                  <SelectValue placeholder="状態を選ぶ" />
                </SelectTrigger>
                <SelectContent>
                  {states.map((s) => (
                    <SelectItem key={s} value={s}>
                      {s}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            ) : (
              <Input
                value={v.to}
                aria-label={`${title}に進める状態`}
                placeholder="先にテーブルを選んでください"
                onChange={(e) => onChange({ ...v, to: e.target.value })}
                className="h-8"
              />
            )}
          </Field>
          <Field label="ボタンの文言（省略可）">
            <Input
              value={v.label ?? ""}
              aria-label={`${title}のボタンの文言`}
              maxLength={40}
              placeholder={defaultLabel}
              onChange={(e) => onChange({ ...v, label: e.target.value || undefined })}
              className="h-8"
            />
          </Field>
          <label className="flex items-center justify-between gap-3 text-xs">
            <span>コメントを必須にする</span>
            <Switch
              aria-label={`${title}のコメントを必須にする`}
              checked={v.comment === "required"}
              onCheckedChange={(req) =>
                onChange({ ...v, comment: req ? ("required" as ApprovalComment) : undefined })
              }
            />
          </label>
        </>
      ) : null}
    </fieldset>
  );
}

/// 受信箱に出す相手（行のユーザー・ロールの列／固定のユーザー／固定のロール）。
///
/// 列が選べる列に無いとき、「テーブルに無い」と言うのは `seesWholeTable` が真のときだけ（受信箱に出す列と同じ）。
/// owner 以外に返る定義には読めない列が無いので、読めないだけのユーザーの列を「無い」と言わない。`error` は
/// 欄の下に出すサーバの誤り（サーバは、無い列と読めない列のどちらとも言わない文言で返す）。`tableNote` は
/// 束縛したテーブルの定義を使えないとき（読み込み中・読めない）の注記で、「先にテーブルを選んでください」の
/// 代わりに出す。
function AssigneesField({
  node,
  refCandidates,
  inMapRegion,
  table,
  assignees,
  seesWholeTable,
  tableNote,
  error,
  onChange,
}: FormProps & {
  table: DataTable | null;
  assignees: ApprovalAssignee[];
  seesWholeTable: boolean;
  tableNote: string | null;
  error?: string;
  onChange: (next: ApprovalAssignee[]) => void;
}) {
  const refFields = (table?.schema.fields ?? []).filter((f) => f.type === "user_ref" || f.type === "role_ref");
  // 見えるがユーザー・部署の列でない列（サーバは ir.bad_assignee で止める）は、無いとは言わずに種類で言う。
  const visible = new Set((table?.schema.fields ?? []).map((f) => f.key));
  const strayNote = (key: string) =>
    visible.has(key)
      ? { text: `ユーザー・部署の列ではありません: ${key}`, certain: true }
      : unselectableNote(key, seesWholeTable);
  const update = (i: number, next: ApprovalAssignee) => onChange(assignees.map((a, j) => (j === i ? next : a)));
  const blank = (kind: AssigneeKind): ApprovalAssignee =>
    kind === "field" ? { field: refFields[0]?.key ?? "" } : kind === "user" ? { user: "" } : { role: "" };
  return (
    <fieldset className="space-y-2" data-node={node.id}>
      <legend className="text-xs font-medium">承認者</legend>
      {/* 承認者は「承認」の画面に出す相手を決めるだけで、判断できるかは状態の進め方の規則が決める。規則が申請者を
          除いていなければ、申請者は自分の申請を承認できる（BUG-110・付録 D の N3.4.1。api の
          workflow_approval_it の approvers_only_choose_the_inbox… で確かめてある）。 */}
      <p className="text-[11px] leading-relaxed text-muted-foreground" data-testid="approval-assignees-note">
        選んだ人の「承認」の画面に依頼が出ます。判断できるかは状態の進め方で決まります。
      </p>
      <MoreInfo testId="approval-assignees-more">
        <p>
          ここで選ぶのは依頼を出す相手で、判断できる人を決めるものではありません。状態の進め方で許された人は、ここに選ばれていなくても、テーブルの画面から判断できます。
        </p>
        <p>
          そのため、状態の進め方によっては、申請者が自分の申請を承認できます。防ぐには、状態の進め方の条件から申請者本人を除き、承認者の列を申請者が書き換えられないようにします。状態の進め方と、テーブルの書き込みの決まりは、いまは画面では変えられません。管理者に頼んでください。
        </p>
        <p>
          「承認」の画面に出す相手は、承認を始めたときの承認者の列の値で決まります。いまはお知らせ（メールなど）は届きません。承認者には「承認」の画面を見るよう伝えてください。判断は承認者本人の権限で行い、変更の記録にも承認者の操作として残ります。
        </p>
      </MoreInfo>
      {assignees.map((a, i) => {
        const kind = kindOf(a);
        return (
          <div key={i} className="space-y-1.5 rounded-md border p-2">
            <div className="flex items-center gap-1.5">
              <Select value={kind} onValueChange={(k) => update(i, blank(k as AssigneeKind))}>
                <SelectTrigger aria-label={`相手 ${i + 1} の種類`} className="h-8 flex-1 text-[13px]">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="field">行の列（ユーザー・部署）</SelectItem>
                  <SelectItem value="user">ユーザーを指定</SelectItem>
                  <SelectItem value="role">部署（ロール）を指定</SelectItem>
                </SelectContent>
              </Select>
              <Button
                variant="ghost"
                size="icon"
                className="size-8 shrink-0"
                aria-label={`相手 ${i + 1} を外す`}
                onClick={() => onChange(assignees.filter((_, j) => j !== i))}
              >
                <X className="size-4" aria-hidden />
              </Button>
            </div>
            {kind === "field" ? (
              <>
                {refFields.length ? (
                  <Select value={a.field ?? ""} onValueChange={(field) => update(i, { field })}>
                    <SelectTrigger aria-label={`相手 ${i + 1} の列`} className="h-8 text-[13px]">
                      <SelectValue placeholder="列を選ぶ" />
                    </SelectTrigger>
                    <SelectContent>
                      {refFields.map((f) => (
                        <SelectItem key={f.key} value={f.key}>
                          {f.display_name || f.key}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                ) : (
                  <p className="text-[11px] text-muted-foreground">
                    {!table
                      ? (tableNote ?? "先にテーブルを選んでください")
                      : seesWholeTable
                        ? "このテーブルにユーザー・部署の列がありません"
                        : "あなたが読める列に、ユーザー・部署の列がありません"}
                  </p>
                )}
                {table && a.field && !refFields.some((f) => f.key === a.field) ? (
                  <StrayNote {...strayNote(a.field)} />
                ) : null}
              </>
            ) : (
              <ValueExprInput
                label={kind === "user" ? "ユーザーの ID" : "部署（ロール）の ID"}
                value={(kind === "user" ? a.user : a.role) as ValueExpr | undefined}
                onChange={(v) => update(i, kind === "user" ? { user: v } : { role: v })}
                refCandidates={refCandidates}
                inMapRegion={inMapRegion}
                placeholder={kind === "user" ? "例: 前のブロックの承認者" : "例: keiri"}
              />
            )}
          </div>
        );
      })}
      {error ? <p className="text-[11px] text-destructive">{error}</p> : null}
      <Button
        variant="outline"
        size="sm"
        className="w-full"
        disabled={assignees.length >= MAX_ASSIGNEES}
        onClick={() => onChange([...assignees, blank("field")])}
      >
        <Plus className="size-4" aria-hidden />
        相手を足す
      </Button>
    </fieldset>
  );
}

/// 受信箱に出す列（担当者に読めない列は出ない）。
///
/// 保存済みの値のうち選べる列に無いもの（テーブルの列が消えた・AI が書いた・状態の列・自分に読めない列）は
/// ボタンに出ないので、外すボタン付きで別に出す（出さないと見えないまま保存が ir.unknown_field で止まり、上限の
/// 数にも入る）。「テーブルに無い」と言うのは `seesWholeTable`（定義の全体が見えている）のときだけ。それ以外は
/// 読めないだけで実在する列かもしれないので、断定しない。
function ShowFieldsField({
  table,
  value,
  error,
  seesWholeTable,
  onChange,
}: {
  table: DataTable | null;
  value: string[];
  error?: string;
  seesWholeTable: boolean;
  onChange: (next: string[]) => void;
}) {
  if (!table) return null;
  const fields = table.schema.fields.filter((f) => f.key !== table.schema.status_field);
  const selectable = new Set(fields.map((f) => f.key));
  const stray = value
    .filter((k) => !selectable.has(k))
    .map((k) =>
      k === table.schema.status_field
        ? { key: k, text: `状態の列は選べません（状態は受信箱に別に出ます）: ${k}`, certain: true }
        : { key: k, ...unselectableNote(k, seesWholeTable) },
    );
  return (
    <fieldset className="space-y-1.5">
      <legend className="text-xs font-medium">受信箱に出す列</legend>
      <p className="text-[11px] text-muted-foreground">担当者に読めない列は出ません（{MAX_SHOW_FIELDS} 列まで）</p>
      <div className="flex flex-wrap gap-1.5">
        {fields.map((f) => {
          const on = value.includes(f.key);
          return (
            <button
              key={f.key}
              type="button"
              aria-pressed={on}
              disabled={!on && value.length >= MAX_SHOW_FIELDS}
              onClick={() => onChange(on ? value.filter((k) => k !== f.key) : [...value, f.key])}
              className="rounded-full border px-2.5 py-1 text-xs transition-colors hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50 aria-pressed:border-primary aria-pressed:bg-primary/10"
            >
              {f.display_name || f.key}
            </button>
          );
        })}
      </div>
      {stray.length ? (
        <ul className="space-y-1" aria-label="選べない列">
          {stray.map(({ key: k, text, certain }) => (
            <li
              key={k}
              className={cn(
                "flex items-center justify-between gap-2 text-[11px]",
                certain ? "text-destructive" : "text-muted-foreground",
              )}
            >
              <span className="min-w-0 break-all">{text}</span>
              <Button
                variant="ghost"
                size="icon"
                className="size-8 shrink-0"
                aria-label={`列 ${k} を外す`}
                onClick={() => onChange(value.filter((x) => x !== k))}
              >
                <X className="size-4" aria-hidden />
              </Button>
            </li>
          ))}
        </ul>
      ) : null}
      {error ? <p className="text-[11px] text-destructive">{error}</p> : null}
    </fieldset>
  );
}

/// 担当者の選べない列の注記（断定できるものだけ赤字）。
function StrayNote({ text, certain }: { text: string; certain: boolean }) {
  return (
    <p className={cn("break-all text-[11px]", certain ? "text-destructive" : "text-muted-foreground")}>{text}</p>
  );
}
