"use client";

/// 「日付を計算」（transform.date）のフォーム（GAP-15）。
///
/// 基準の日（省略すると実行の予定の日時）をずらし、その日を含む週・月の最初と最後の日を求める。欄は codegen の
/// typed 契約（NodeParamsByType["transform.date"]）に対応する。既定の値（日・月曜・日本時間）は IR に書かない
/// （省略と同じ動きで、保存した定義を小さく保つ）。

import * as React from "react";

import type { NodeParamsByType } from "@/generated/workflow-catalog";
import { DATE_DEFAULT_TZ } from "@/generated/workflow-catalog";
import type { DateUnit, ValueExpr, WeekStart } from "@/generated/workflow-ir";
import { Select, SelectContent, SelectItem, SelectValue } from "@/components/ui/select";
import { Field, FieldSelectTrigger } from "./common";
import type { FormProps } from "./forms-basic";
import { paramsOf, patchParams } from "./params";
import { ValueExprInput } from "./value-expr-input";

/// ずらす単位の呼び名（「1 か月」と言う）。
const SHIFT_UNITS: { value: DateUnit; label: string }[] = [
  { value: "day", label: "日" },
  { value: "week", label: "週" },
  { value: "month", label: "か月" },
  { value: "year", label: "年" },
];

/// 期間の呼び名。
const PERIODS: { value: DateUnit; label: string }[] = [
  { value: "day", label: "日（その日だけ）" },
  { value: "week", label: "週" },
  { value: "month", label: "月" },
  { value: "year", label: "年" },
];

/// 空の値（欄を消した）か。空なら項目ごと省く（省略と同じ動き）。
function isBlank(v: ValueExpr | undefined): boolean {
  return v === undefined || v === null || (typeof v === "string" && v.trim() === "");
}

/// 日本時間と UTC のほかの時間帯を時差で言う（名前は出さない・付録 C）。読めない名前は時差を添えない。
function otherZoneLabel(tz: string): string {
  try {
    const part = new Intl.DateTimeFormat("en-US", { timeZone: tz, timeZoneName: "shortOffset" })
      .formatToParts(new Date())
      .find((x) => x.type === "timeZoneName")?.value;
    const offset = part?.replace(/^GMT/, "UTC");
    return offset && offset !== "UTC" ? `そのほか（${offset}）` : "そのほか";
  } catch {
    return "そのほか";
  }
}

export function TransformDateForm({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
  const p = paramsOf<NodeParamsByType["transform.date"]>(node);
  const shift = p.shift ?? null;
  const period = p.period ?? "day";
  const tz = p.tz ?? DATE_DEFAULT_TZ;
  const knownZone = tz === DATE_DEFAULT_TZ || tz === "UTC";
  return (
    <div className="space-y-3">
      <ValueExprInput
        label="基準の日（省略可）"
        value={p.base as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { base: isBlank(v) ? undefined : v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        placeholder="空欄なら予定の日時"
        noJson
        hint="空欄なら、この実行の予定の日時から数えます（スケジュールでは予定の時刻、手動では指定した日時か実行した日時）。日付（2026-11-02）か、時差つきの日時を入れられます。"
        error={fieldErrors.get("base")}
      />
      <ValueExprInput
        label="ずらす数（省略可）"
        value={shift?.amount as ValueExpr | undefined}
        onChange={(v) =>
          patchParams(dispatch, node, {
            shift: isBlank(v) ? undefined : { amount: v, unit: shift?.unit ?? "day" },
          })
        }
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        numeric="always"
        numericNote={null}
        noJson
        placeholder="例: -1"
        hint="前へずらすときはマイナスの数にします。例: -1 と「週」で先週、1 と「か月」で来月。"
        error={fieldErrors.get("shift")}
      />
      {shift ? (
        <Field label="ずらす単位">
          <Select
            value={shift.unit}
            onValueChange={(v) => patchParams(dispatch, node, { shift: { ...shift, unit: v as DateUnit } })}
          >
            <FieldSelectTrigger className="h-8">
              <SelectValue />
            </FieldSelectTrigger>
            <SelectContent>
              {SHIFT_UNITS.map((u) => (
                <SelectItem key={u.value} value={u.value}>
                  {u.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </Field>
      ) : null}
      <Field
        label="期間"
        hint="週・月・年を選ぶと、その日を含む週・月・年の最初の日と最後の日（月末など）も求めます。"
        error={fieldErrors.get("period")}
      >
        <Select
          value={period}
          onValueChange={(v) =>
            patchParams(dispatch, node, {
              period: v === "day" ? undefined : v,
              // 週の初めは期間が週のときだけ使う。
              week_starts_on: v === "week" ? p.week_starts_on : undefined,
            })
          }
        >
          <FieldSelectTrigger className="h-8">
            <SelectValue />
          </FieldSelectTrigger>
          <SelectContent>
            {PERIODS.map((u) => (
              <SelectItem key={u.value} value={u.value}>
                {u.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </Field>
      {period === "week" ? (
        <Field label="週の初め" error={fieldErrors.get("week_starts_on")}>
          <Select
            value={p.week_starts_on ?? "monday"}
            onValueChange={(v) =>
              patchParams(dispatch, node, { week_starts_on: v === "monday" ? undefined : (v as WeekStart) })
            }
          >
            <FieldSelectTrigger className="h-8">
              <SelectValue />
            </FieldSelectTrigger>
            <SelectContent>
              <SelectItem value="monday">月曜日</SelectItem>
              <SelectItem value="sunday">日曜日</SelectItem>
            </SelectContent>
          </Select>
        </Field>
      ) : null}
      <Field label="時間帯" error={fieldErrors.get("tz")}>
        <Select
          value={tz}
          onValueChange={(v) => patchParams(dispatch, node, { tz: v === DATE_DEFAULT_TZ ? undefined : v })}
        >
          <FieldSelectTrigger className="h-8">
            <SelectValue />
          </FieldSelectTrigger>
          <SelectContent>
            <SelectItem value={DATE_DEFAULT_TZ}>日本時間（おすすめ）</SelectItem>
            <SelectItem value="UTC">UTC（日本時間の 9 時間前）</SelectItem>
            {knownZone ? null : <SelectItem value={tz}>{otherZoneLabel(tz)}</SelectItem>}
          </SelectContent>
        </Select>
      </Field>
      <p className="text-[11px] leading-relaxed text-muted-foreground" data-testid="date-result-note">
        後のブロックでは、このブロックの結果から、ずらした日（date）、期間の最初の日（start）と最後の日（end）を選べます。日付は
        2026-11-02 の形で、テーブルの日付の列と比べられます。
      </p>
    </div>
  );
}
