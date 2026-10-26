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
import { Textarea } from "@/components/ui/textarea";
import type { NodeParamsByType } from "@/generated/workflow-catalog";
import type {
  ApprovalAssignee,
  ApprovalChoice,
  ApprovalComment,
  ValueExpr,
} from "@/generated/workflow-ir";
import type { DataTable } from "@/lib/data-api";
import { Field, NumberInput } from "./common";
import { DataTableSelect, useDataTables } from "./forms-data";
import type { FormProps } from "./forms-basic";
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

/// FSM で管理するテーブルの状態（状態の列の選択肢）。
function statesOf(table: DataTable | null): string[] {
  if (!table?.schema.fsm_ref || !table.schema.status_field) return [];
  return table.schema.fields.find((f) => f.key === table.schema.status_field)?.options ?? [];
}

type AssigneeKind = "field" | "user" | "role";

function kindOf(a: ApprovalAssignee): AssigneeKind {
  if (a.user !== undefined) return "user";
  if (a.role !== undefined) return "role";
  return "field";
}

export function HumanApprovalForm(props: FormProps) {
  const { node, dispatch, refCandidates, inMapRegion, fieldErrors } = props;
  const { tables } = useDataTables();
  const p = paramsOf<Params>(node);
  const fsmTables = React.useMemo(() => (tables ?? []).filter((t) => Boolean(t.schema.fsm_ref)), [tables]);
  const table = fsmTables.find((t) => t.id === p.record?.table) ?? null;
  const states = statesOf(table);
  const set = (patch: Partial<Record<keyof Params, unknown>>) => patchParams(dispatch, node, patch);
  const record = p.record ?? { table: "", id: { $from: "trigger", path: "/record_id" } as ValueExpr };

  return (
    <div className="space-y-4">
      <Field label="受信箱の件名" error={fieldErrors.get("title")} hint="担当者の受信箱に出る名前（120 字まで）">
        <Input
          value={p.title ?? ""}
          maxLength={120}
          placeholder="例: 経費申請の承認"
          onChange={(e) => set({ title: e.target.value })}
          className="h-8"
        />
      </Field>
      <Field label="担当者への説明（省略可）" error={fieldErrors.get("instructions")}>
        <Textarea
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
          tables={tables === null ? null : fsmTables}
          label="承認するテーブル"
          onChange={(id) => set({ record: { ...record, table: id } })}
        />
      </Field>
      {tables !== null && fsmTables.length === 0 ? (
        <p className="text-[11px] leading-relaxed text-muted-foreground">
          状態の遷移の規則があるテーブルがありません。テーブルの設定で状態の列と遷移の規則を作ってください。
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

      <AssigneesField {...props} table={table} assignees={p.assignees ?? []} onChange={(assignees) => set({ assignees })} />

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
            error={fieldErrors.get(c.key)}
            onChange={(next) => set({ [c.key]: next })}
          />
        );
      })}

      <ShowFieldsField table={table} value={p.show_fields ?? []} onChange={(v) => set({ show_fields: v.length ? v : undefined })} />

      <Field
        label="判断の期限（時間・省略可）"
        error={fieldErrors.get("timeout_sec")}
        hint="空欄なら期限なし（実行の期限が来ると取り消されます）"
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
function ChoiceField({
  title,
  defaultLabel,
  optional,
  value,
  states,
  error,
  onChange,
}: {
  title: string;
  defaultLabel: string;
  optional: boolean;
  value: ApprovalChoice | undefined;
  states: string[];
  error?: string;
  onChange: (next: ApprovalChoice | undefined) => void;
}) {
  const on = value !== undefined;
  const v = value ?? { to: "" };
  return (
    <fieldset className="space-y-2 rounded-lg border p-2.5">
      <legend className="flex w-full items-center justify-between gap-2 px-1 text-xs font-medium">
        <span>{title}</span>
        {optional ? (
          <Switch
            aria-label={`${title}の出口を使う`}
            checked={on}
            onCheckedChange={(checked) => onChange(checked ? { to: "" } : undefined)}
          />
        ) : null}
      </legend>
      {on || !optional ? (
        <>
          <Field label="進める状態" error={error}>
            {states.length ? (
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
function AssigneesField({
  node,
  refCandidates,
  inMapRegion,
  fieldErrors,
  table,
  assignees,
  onChange,
}: FormProps & {
  table: DataTable | null;
  assignees: ApprovalAssignee[];
  onChange: (next: ApprovalAssignee[]) => void;
}) {
  const refFields = (table?.schema.fields ?? []).filter((f) => f.type === "user_ref" || f.type === "role_ref");
  const update = (i: number, next: ApprovalAssignee) => onChange(assignees.map((a, j) => (j === i ? next : a)));
  const blank = (kind: AssigneeKind): ApprovalAssignee =>
    kind === "field" ? { field: refFields[0]?.key ?? "" } : kind === "user" ? { user: "" } : { role: "" };
  return (
    <fieldset className="space-y-2" data-node={node.id}>
      <legend className="text-xs font-medium">受信箱に出す相手</legend>
      <p className="text-[11px] leading-relaxed text-muted-foreground">
        受信箱に出すだけで、判断できるかは表の状態の遷移の規則が決めます（代理の人も規則に入れてください）。
      </p>
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
              refFields.length ? (
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
                  {table ? "このテーブルにユーザー・部署の列がありません" : "先にテーブルを選んでください"}
                </p>
              )
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
      {fieldErrors.get("assignees") ? (
        <p className="text-[11px] text-destructive">{fieldErrors.get("assignees")}</p>
      ) : null}
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
function ShowFieldsField({
  table,
  value,
  onChange,
}: {
  table: DataTable | null;
  value: string[];
  onChange: (next: string[]) => void;
}) {
  if (!table) return null;
  const fields = table.schema.fields.filter((f) => f.key !== table.schema.status_field);
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
    </fieldset>
  );
}
