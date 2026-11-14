"use client";

/// 「AI に聞く」（llm.invoke）の思考の深さ（`effort`・GAP-13）。
///
/// 選べるのは「組織の既定に従う」（params から外す）と 3 段階（速い・標準・じっくり・チャットと同じ呼び名）。
/// 書くと組織の既定の思考の深さより優先する。ただし、モデルを省略して組織の既定の Auto で動くときと、組織が
/// モデルの選択を禁じているときは使わず、組織の設定に従う（チャットと揃える・D-75・ir.md §7.3）。そのときは
/// サーバの保存時の警告（`WARNING_CODES.EFFORT_IGNORED`）を欄の下に出す（`warning`）。思考に対応しないモデルでは
/// gateway が送らないので、欄は選んでいるモデル（省略なら組織の既定・Auto の振り先）が対応するときだけ出す
/// （`GET /llm/models?purpose=workflow` の `supports_effort`）。対応しないモデルに書いた値は消さずに、
/// 送られないことを欄の下で知らせる（外せるように欄は残す）。

import * as React from "react";

import { Select, SelectContent, SelectItem, SelectValue } from "@/components/ui/select";
import { EFFORT_LABEL } from "@/components/admin/format";
import type { LlmModelsResponse } from "@/lib/admin-ai-api";
import { useLlmModelsState } from "@/lib/model-choice";
import type { LlmEffort } from "@/generated/workflow-ir";
import { Field, FieldSelectTrigger } from "./common";

const EFFORTS: LlmEffort[] = ["low", "medium", "high"];
/// 「組織の既定に従う」の選択肢の値（Radix の Select は空文字の値を持てない）。
const ORG_DEFAULT = "org-default";

/// 選んでいるモデル（省略なら組織の既定・既定が Auto なら振り先）が思考の深さに対応するか。分からなければ null。
export function modelSupportsEffort(data: LlmModelsResponse, model: string | undefined): boolean | null {
  const id = model || (data.default_model === "auto" ? data.auto?.model : data.default_model);
  if (!id) return null;
  return data.models.find((m) => m.id === id)?.supports_effort ?? null;
}

export function EffortField({
  value,
  model,
  onChange,
  error,
  warning,
}: {
  value: LlmEffort | undefined;
  /// このブロックのモデル（省略は組織の既定）。
  model: string | undefined;
  onChange: (next: LlmEffort | undefined) => void;
  error?: string | null;
  /// 書いた値が使われないことの保存時の警告（組織の設定に従う・D-75）。
  warning?: string | null;
}) {
  const state = useLlmModelsState("workflow");
  const supports = state.status === "ready" ? modelSupportsEffort(state.data, model) : null;
  // 対応しないと分かっているモデルで、値も書いていなければ欄を出さない（効かない設定は出さない）。
  if (supports === false && value === undefined && !error) return null;
  const hint =
    supports === false
      ? "このモデルは思考の深さに対応しないため、この設定は送られません（組織の既定に従うに戻せます）"
      : value === undefined
        ? "省略すると組織の既定で動きます。深いほど時間と費用がかかります"
        : "組織の既定より優先します。深いほど時間と費用がかかります";
  return (
    <Field label="思考の深さ（省略可）" hint={hint} error={error} warning={value === undefined ? null : warning}>
      <Select
        value={value ?? ORG_DEFAULT}
        onValueChange={(v) => onChange(v === ORG_DEFAULT ? undefined : (v as LlmEffort))}
      >
        <FieldSelectTrigger className="h-8" data-testid="llm-effort">
          <SelectValue />
        </FieldSelectTrigger>
        <SelectContent>
          <SelectItem value={ORG_DEFAULT}>組織の既定に従う</SelectItem>
          {EFFORTS.map((e) => (
            <SelectItem key={e} value={e}>
              {EFFORT_LABEL[e]}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </Field>
  );
}
