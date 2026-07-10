"use client";

/// 制御ノードのフォーム（条件分岐・振り分け・繰り返し・待機・合流）。
///
/// 条件は「比較の並び（すべて/いずれか）」の 1 段編集を基本にし、深いネストは
/// 保存済み JSON をそのまま維持する（高度な条件は AI 編集/JSON で作る想定）。

import * as React from "react";
import { Plus, Trash2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import type { NodeParamsByType } from "@/generated/workflow-catalog";
import type {
  CmpOp,
  Comparison,
  Condition,
  ValueExpr,
} from "@/generated/workflow-ir";
import { Field, NumberInput } from "./common";
import { paramsOf, patchParams } from "./params";
import { ValueExprInput } from "./value-expr-input";
import type { FormProps } from "./forms-basic";

const OP_LABELS: [CmpOp, string][] = [
  ["eq", "と等しい"],
  ["neq", "と等しくない"],
  ["gt", "より大きい"],
  ["gte", "以上"],
  ["lt", "より小さい"],
  ["lte", "以下"],
  ["contains", "を含む"],
  ["starts_with", "で始まる"],
  ["ends_with", "で終わる"],
  ["in", "のいずれか"],
  ["exists", "が存在する"],
  ["is_null", "が空である"],
  ["matches", "がパターンに一致"],
];

const NO_RIGHT: CmpOp[] = ["exists", "is_null"];

type FlatCondition = { mode: "all" | "any"; comparisons: Comparison[] } | null;

/// 1 段の all/any＋比較列に平坦化できる条件だけ編集対象にする（それ以外は不透明維持）。
function flatten(cond: Condition | undefined): FlatCondition {
  if (!cond || typeof cond !== "object") return { mode: "all", comparisons: [] };
  if ("cmp" in cond) return { mode: "all", comparisons: [cond.cmp as Comparison] };
  if ("all" in cond || "any" in cond) {
    const mode = "all" in cond ? "all" : "any";
    const list = (cond as { all?: Condition[]; any?: Condition[] })[mode] ?? [];
    const comparisons: Comparison[] = [];
    for (const c of list) {
      if (c && typeof c === "object" && "cmp" in c) {
        comparisons.push((c as { cmp: Comparison }).cmp);
      } else {
        return null; // ネスト条件は簡易エディタ対象外。
      }
    }
    return { mode, comparisons };
  }
  return null;
}

function build(flat: NonNullable<FlatCondition>): Condition {
  const cmps = flat.comparisons.map((cmp) => ({ cmp }) as Condition);
  if (cmps.length === 1) return cmps[0];
  return (flat.mode === "all" ? { all: cmps } : { any: cmps }) as Condition;
}

export function ConditionEditor({
  value,
  onChange,
  refCandidates,
  inMapRegion,
}: {
  value: Condition | undefined;
  onChange: (next: Condition) => void;
} & Pick<FormProps, "refCandidates" | "inMapRegion">) {
  const flat = flatten(value);
  if (flat === null) {
    return (
      <p className="rounded-md border bg-muted/40 p-2 text-[11px] text-muted-foreground">
        入れ子になった高度な条件が設定されています（AI 編集で変更できます）。
        ここで編集すると単純な条件に置き換わります。
        <Button
          variant="outline"
          size="sm"
          className="mt-2 h-7 w-full text-xs"
          onClick={() => onChange(build({ mode: "all", comparisons: [] }))}
        >
          単純な条件で作り直す
        </Button>
      </p>
    );
  }
  const update = (next: NonNullable<FlatCondition>) => onChange(build(next));
  return (
    <div className="space-y-2">
      {flat.comparisons.length > 1 ? (
        <Select
          value={flat.mode}
          onValueChange={(v) => update({ ...flat, mode: v as "all" | "any" })}
        >
          <SelectTrigger className="h-8">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">すべて満たすとき</SelectItem>
            <SelectItem value="any">どれか 1 つ満たすとき</SelectItem>
          </SelectContent>
        </Select>
      ) : null}
      {flat.comparisons.map((cmp, i) => (
        <div key={i} className="space-y-1.5 rounded-md border p-2">
          <ValueExprInput
            label="調べる値"
            value={cmp.left as ValueExpr | undefined}
            onChange={(v) => {
              const comparisons = [...flat.comparisons];
              comparisons[i] = { ...cmp, left: v };
              update({ ...flat, comparisons });
            }}
            refCandidates={refCandidates}
            inMapRegion={inMapRegion}
          />
          <div className="flex items-center gap-1.5">
            <Select
              value={cmp.op}
              onValueChange={(v) => {
                const comparisons = [...flat.comparisons];
                comparisons[i] = {
                  ...cmp,
                  op: v as CmpOp,
                  right: NO_RIGHT.includes(v as CmpOp) ? undefined : cmp.right,
                };
                update({ ...flat, comparisons });
              }}
            >
              <SelectTrigger className="h-8 flex-1">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {OP_LABELS.map(([op, label]) => (
                  <SelectItem key={op} value={op}>
                    {label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Button
              variant="ghost"
              size="icon"
              className="size-8"
              aria-label="条件を削除"
              onClick={() => {
                const comparisons = flat.comparisons.filter((_, j) => j !== i);
                update({ ...flat, comparisons });
              }}
            >
              <Trash2 className="size-3.5" aria-hidden />
            </Button>
          </div>
          {!NO_RIGHT.includes(cmp.op) ? (
            <ValueExprInput
              label="比べる値"
              value={cmp.right as ValueExpr | undefined}
              onChange={(v) => {
                const comparisons = [...flat.comparisons];
                comparisons[i] = { ...cmp, right: v };
                update({ ...flat, comparisons });
              }}
              refCandidates={refCandidates}
              inMapRegion={inMapRegion}
            />
          ) : null}
        </div>
      ))}
      <Button
        variant="outline"
        size="sm"
        className="h-7 w-full text-xs"
        onClick={() =>
          update({
            ...flat,
            comparisons: [
              ...flat.comparisons,
              {
                left: { $from: "input", path: "/" } as unknown as ValueExpr,
                op: "eq",
                right: "" as unknown as ValueExpr,
              } as Comparison,
            ],
          })
        }
      >
        <Plus className="size-3.5" aria-hidden />
        条件を追加
      </Button>
    </div>
  );
}

export function BranchForm({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
  const p = paramsOf<NodeParamsByType["control.branch"]>(node);
  return (
    <Field label="分ける条件" hint="満たすと「はい」・満たさないと「いいえ」へ" error={fieldErrors.get("condition")}>
      <ConditionEditor
        value={p.condition as Condition | undefined}
        onChange={(condition) => patchParams(dispatch, node, { condition })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
      />
    </Field>
  );
}

export function SwitchForm({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
  const p = paramsOf<NodeParamsByType["control.switch"]>(node);
  const cases = (p.cases ?? []) as { port: string; equals: unknown }[];
  return (
    <div className="space-y-3">
      <ValueExprInput
        label="振り分ける値"
        value={p.value as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { value: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        error={fieldErrors.get("value")}
      />
      <Field
        label="行き先"
        hint="値が一致した行き先へ。どれにも一致しないと「default」へ"
        error={fieldErrors.get("cases")}
      >
        <div className="space-y-1.5">
          {cases.map((c, i) => (
            <div key={i} className="flex items-center gap-1.5">
              <Input
                value={String(c.equals ?? "")}
                onChange={(e) => {
                  const next = [...cases];
                  next[i] = { ...c, equals: e.target.value };
                  patchParams(dispatch, node, { cases: next });
                }}
                placeholder="この値のとき"
                className="h-8 flex-1"
                aria-label="一致する値"
              />
              <span className="text-xs text-muted-foreground">→</span>
              <Input
                value={c.port}
                onChange={(e) => {
                  const next = [...cases];
                  next[i] = { ...c, port: e.target.value };
                  patchParams(dispatch, node, { cases: next });
                }}
                placeholder="出口名"
                className="h-8 w-24 font-mono text-xs"
                aria-label="出口名"
              />
              <Button
                variant="ghost"
                size="icon"
                className="size-8"
                aria-label="行き先を削除"
                onClick={() =>
                  patchParams(dispatch, node, {
                    cases: cases.filter((_, j) => j !== i),
                  })
                }
              >
                <Trash2 className="size-3.5" aria-hidden />
              </Button>
            </div>
          ))}
          <Button
            variant="outline"
            size="sm"
            className="h-7 w-full text-xs"
            onClick={() =>
              patchParams(dispatch, node, {
                cases: [...cases, { port: `case_${cases.length + 1}`, equals: "" }],
              })
            }
          >
            <Plus className="size-3.5" aria-hidden />
            行き先を追加
          </Button>
        </div>
      </Field>
    </div>
  );
}

export function MapForm({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
  const p = paramsOf<NodeParamsByType["control.map"]>(node);
  return (
    <div className="space-y-3">
      <ValueExprInput
        label="繰り返すリスト"
        value={p.items as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { items: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        error={fieldErrors.get("items")}
      />
      <Field label="失敗した要素の扱い">
        <Select
          value={(p.on_item_error as string | undefined) ?? "fail_map"}
          onValueChange={(v) => patchParams(dispatch, node, { on_item_error: v })}
        >
          <SelectTrigger className="h-8">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="fail_map">1 つでも失敗したら全体を失敗に</SelectItem>
            <SelectItem value="collect">失敗も結果に集めて続行</SelectItem>
          </SelectContent>
        </Select>
      </Field>
      <p className="rounded-md border bg-muted/40 p-2 text-[11px] text-muted-foreground">
        繰り返しの中に入れるブロックは、追加後にこのブロックを「親」に指定します
        （現在は AI 編集での作成が確実です）
      </p>
    </div>
  );
}

export function WaitForm({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
  const p = paramsOf<NodeParamsByType["control.wait"]>(node);
  const kind = (p.kind as string | undefined) ?? "duration";
  return (
    <div className="space-y-3">
      <Field label="待ち方" error={fieldErrors.get("kind")}>
        <Select
          value={kind}
          onValueChange={(v) => {
            if (v === "duration") {
              patchParams(dispatch, node, {
                kind: v,
                duration_sec: 60,
                until: undefined,
                source: undefined,
                scope: undefined,
                filter: undefined,
                timeout_sec: undefined,
                on_timeout: undefined,
              });
            } else if (v === "until") {
              patchParams(dispatch, node, {
                kind: v,
                until: "",
                duration_sec: undefined,
                source: undefined,
                scope: undefined,
                filter: undefined,
                timeout_sec: undefined,
                on_timeout: undefined,
              });
            } else {
              patchParams(dispatch, node, {
                kind: v,
                source: "storage.write",
                duration_sec: undefined,
                until: undefined,
              });
            }
          }}
        >
          <SelectTrigger className="h-8">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="duration">決めた時間だけ待つ</SelectItem>
            <SelectItem value="until">決めた日時まで待つ</SelectItem>
            <SelectItem value="event">できごとが起きるまで待つ</SelectItem>
          </SelectContent>
        </Select>
      </Field>
      {kind === "duration" ? (
        <ValueExprInput
          label="待つ秒数"
          value={p.duration_sec as ValueExpr | undefined}
          onChange={(v) => patchParams(dispatch, node, { duration_sec: v })}
          refCandidates={refCandidates}
          inMapRegion={inMapRegion}
          placeholder="例: 3600"
          error={fieldErrors.get("duration_sec")}
        />
      ) : null}
      {kind === "until" ? (
        <ValueExprInput
          label="待つ日時（RFC3339）"
          value={p.until as ValueExpr | undefined}
          onChange={(v) => patchParams(dispatch, node, { until: v })}
          refCandidates={refCandidates}
          inMapRegion={inMapRegion}
          placeholder="例: 2026-08-01T09:00:00+09:00"
          error={fieldErrors.get("until")}
        />
      ) : null}
      {kind === "event" ? (
        <div className="space-y-3">
          <Field label="待つできごと" error={fieldErrors.get("source")}>
            <Select
              value={(p.source as string | undefined) ?? "storage.write"}
              onValueChange={(v) => patchParams(dispatch, node, { source: v })}
            >
              <SelectTrigger className="h-8">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="storage.write">ファイルが保存されたとき</SelectItem>
              </SelectContent>
            </Select>
          </Field>
          <Field
            label="対象フォルダ ID（省略可）"
            hint="指定するとそのフォルダ配下の保存だけ待ちます"
            error={fieldErrors.get("scope")}
          >
            <Input
              value={
                ((p.scope as { folder?: string } | undefined)?.folder as string | undefined) ?? ""
              }
              onChange={(e) =>
                patchParams(dispatch, node, {
                  scope: e.target.value ? { folder: e.target.value } : undefined,
                })
              }
              placeholder="フォルダ ID"
              className="h-8 font-mono text-xs"
            />
          </Field>
          <Field label="待ちの上限（秒・省略可）" hint="空欄で無期限" error={fieldErrors.get("timeout_sec")}>
            <NumberInput
              value={typeof p.timeout_sec === "number" ? p.timeout_sec : null}
              min={1}
              onChange={(n) =>
                patchParams(dispatch, node, { timeout_sec: n ?? undefined })
              }
            />
          </Field>
          <Field label="時間切れのとき">
            <Select
              value={(p.on_timeout as string | undefined) ?? "fail"}
              onValueChange={(v) => patchParams(dispatch, node, { on_timeout: v })}
            >
              <SelectTrigger className="h-8">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="fail">失敗にする</SelectItem>
                <SelectItem value="continue">「時間切れ」の出口に流す</SelectItem>
              </SelectContent>
            </Select>
          </Field>
        </div>
      ) : null}
    </div>
  );
}

export function JoinForm({ node, dispatch }: FormProps) {
  const p = paramsOf<NodeParamsByType["control.join"]>(node);
  return (
    <Field label="待ち合わせ" hint="分かれた流れをどう待つか">
      <Select
        value={(p.mode as string | undefined) ?? "all"}
        onValueChange={(v) => patchParams(dispatch, node, { mode: v })}
      >
        <SelectTrigger className="h-8">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="all">全部そろってから進む</SelectItem>
          <SelectItem value="any">最初の 1 つで進む</SelectItem>
        </SelectContent>
      </Select>
    </Field>
  );
}
