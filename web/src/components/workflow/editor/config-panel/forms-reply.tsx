"use client";

/// 「結果を画面に返す」（workflow.reply・GAP-09・D-28）のフォーム。
///
/// ボタン（ミニアプリ・会話の図）から起動した本人の画面に、処理の結果を出す。形（文章・Markdown・表・
/// ファイルへのリンク）・見出し・色合い・返す値を決める。表は列（見出しと行の中の位置）、リンクは ID と名前の
/// 位置を選べる。見出しなどの固定の文言はアプリを使う人全員に見える。

import * as React from "react";
import { Plus, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectValue } from "@/components/ui/select";
import type { NodeParamsByType } from "@/generated/workflow-catalog";
import type { ReplyColumn, ReplyFormat, ReplyTone, ValueExpr } from "@/generated/workflow-ir";
import { Field, FieldInput, FieldSelectTrigger } from "./common";
import type { FormProps } from "./forms-basic";
import { ANY_SHAPES, LIST_SHAPE } from "./value-shape";
import { paramsOf, patchParams } from "./params";
import { ValueExprInput } from "./value-expr-input";

type Params = NodeParamsByType["workflow.reply"];

const MAX_LABEL = 40;
const MAX_COLUMNS = 20;

const FORMATS: { value: ReplyFormat; label: string; hint: string }[] = [
  { value: "text", label: "文章", hint: "改行をそのまま出します" },
  { value: "markdown", label: "Markdown", hint: "見出し・箇条書き・表を使えます（ボタンや画面の表は無効になります）" },
  { value: "table", label: "表", hint: "リスト（例: データを調べるの items）の 1 件を 1 行にします" },
  { value: "file_link", label: "ファイルへのリンク", hint: "ドライブのファイルを開くリンクの一覧にします（開くときに権限を確かめます）" },
];

const TONES: { value: ReplyTone; label: string }[] = [
  { value: "neutral", label: "ふつう" },
  { value: "success", label: "うまくいった（緑）" },
  { value: "warning", label: "注意（黄）" },
  { value: "error", label: "うまくいかなかった（赤）" },
];

export function WorkflowReplyForm({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
  const p = paramsOf<Params>(node);
  const format: ReplyFormat = p.format ?? "text";
  const set = (patch: Partial<Record<keyof Params, unknown>>) => patchParams(dispatch, node, patch);
  const formatHint = FORMATS.find((f) => f.value === format)?.hint;
  const textual = format === "text" || format === "markdown";

  return (
    <div className="space-y-4">
      <Field label="形" error={fieldErrors.get("format")} hint={formatHint}>
        <Select
          value={format}
          onValueChange={(v) =>
            // 形に合わない設定は外す（表の列・リンクの取り出し方）。
            set({
              format: v,
              columns: v === "table" ? p.columns : undefined,
              file: v === "file_link" ? p.file : undefined,
            })
          }
        >
          <FieldSelectTrigger className="h-8 text-[13px]">
            <SelectValue />
          </FieldSelectTrigger>
          <SelectContent>
            {FORMATS.map((f) => (
              <SelectItem key={f.value} value={f.value}>
                {f.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </Field>
      <Field label="見出し（省略可）" error={fieldErrors.get("label")} hint={`${MAX_LABEL} 字まで（例: 回答・指摘・参考にした資料）`}>
        <FieldInput
          value={p.label ?? ""}
          maxLength={MAX_LABEL}
          placeholder="例: 回答"
          onChange={(e) => set({ label: e.target.value.trim() ? e.target.value : undefined })}
          className="h-8"
        />
      </Field>
      <Field label="色合い" error={fieldErrors.get("tone")} hint="業務上うまくいかなかったこと（先に借りられた等）は赤で返します">
        <Select value={p.tone ?? "neutral"} onValueChange={(v) => set({ tone: v === "neutral" ? undefined : v })}>
          <FieldSelectTrigger className="h-8 text-[13px]">
            <SelectValue />
          </FieldSelectTrigger>
          <SelectContent>
            {TONES.map((t) => (
              <SelectItem key={t.value} value={t.value}>
                {t.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </Field>
      <ValueExprInput
        label={textual ? "返す内容" : format === "table" ? "表にするリスト" : "ファイルのリスト"}
        text={textual}
        multiline={textual}
        shapes={format === "table" ? LIST_SHAPE : format === "file_link" ? ANY_SHAPES : undefined}
        value={p.value as ValueExpr | undefined}
        onChange={(v) => set({ value: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        placeholder={textual ? "例: {name} を受け付けました" : "前の結果から選んでください"}
        error={fieldErrors.get("value")}
      />
      {format === "table" ? (
        <ColumnsField
          value={p.columns ?? []}
          error={fieldErrors.get("columns")}
          onChange={(columns) => set({ columns: columns.length ? columns : undefined })}
        />
      ) : null}
      {format === "file_link" ? (
        <fieldset className="space-y-2 rounded-lg border p-2.5">
          <legend className="px-1 text-xs font-medium">ファイルの取り出し方</legend>
          <Field label="ファイルの ID の位置" error={fieldErrors.get("file")} hint="空欄なら /file_id を使い、無ければ /id を使います。">
            <FieldInput
              value={p.file?.id_path ?? ""}
              placeholder="/file_id"
              onChange={(e) => set({ file: { ...p.file, id_path: e.target.value || undefined } })}
              className="h-8 font-mono text-xs"
            />
          </Field>
          <Field label="ファイル名の位置" hint="空欄なら /name を使い、無ければ /file_name を使います。">
            <FieldInput
              value={p.file?.name_path ?? ""}
              placeholder="/name"
              onChange={(e) => set({ file: { ...p.file, name_path: e.target.value || undefined } })}
              className="h-8 font-mono text-xs"
            />
          </Field>
        </fieldset>
      ) : null}
      <p className="text-[11px] leading-relaxed text-muted-foreground" data-testid="reply-visibility-note">
        結果が出るのは、ボタンや会話から起動した本人の画面だけです（スケジュール・できごとで動いたときは出ません）。
        見出しなどの固定の文言は、アプリを使う人全員に見えます。成功を伝える結果は、データの追加などの後に置いてください。
      </p>
    </div>
  );
}

/// 表の列（見出しと、行の中の位置）。省略すると行の項目をそのまま使う。
function ColumnsField({
  value,
  error,
  onChange,
}: {
  value: ReplyColumn[];
  error?: string;
  onChange: (next: ReplyColumn[]) => void;
}) {
  const update = (i: number, patch: Partial<ReplyColumn>) =>
    onChange(value.map((c, j) => (j === i ? { ...c, ...patch } : c)));
  return (
    <fieldset className="space-y-2 rounded-lg border p-2.5">
      <legend className="px-1 text-xs font-medium">表の列（省略可）</legend>
      {value.length === 0 ? (
        <p className="text-[11px] text-muted-foreground">省略すると、行の項目をそのまま列にします（並びは名前の順）。</p>
      ) : null}
      <ul className="space-y-1.5">
        {value.map((c, i) => (
          <li key={i} className="flex min-w-0 items-center gap-1.5">
            <Input
              value={c.label}
              maxLength={MAX_LABEL}
              placeholder="見出し"
              aria-label={`${i + 1} 列目の見出し`}
              onChange={(e) => update(i, { label: e.target.value })}
              className="h-8 min-w-0 flex-1"
            />
            <Input
              value={c.path}
              placeholder="/data/name"
              aria-label={`${i + 1} 列目の位置`}
              onChange={(e) => update(i, { path: e.target.value })}
              className="h-8 min-w-0 flex-1 font-mono text-xs"
            />
            <Button
              variant="ghost"
              size="icon"
              className="size-8 shrink-0"
              aria-label={`${i + 1} 列目を外す`}
              onClick={() => onChange(value.filter((_, j) => j !== i))}
            >
              <X className="size-4" aria-hidden />
            </Button>
          </li>
        ))}
      </ul>
      {error ? <p className="text-[11px] text-destructive">{error}</p> : null}
      <Button
        variant="outline"
        size="sm"
        className="h-7 text-xs"
        disabled={value.length >= MAX_COLUMNS}
        onClick={() => onChange([...value, { label: "", path: "" }])}
      >
        <Plus className="size-3.5" aria-hidden />
        列を足す
      </Button>
    </fieldset>
  );
}
