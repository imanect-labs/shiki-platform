"use client";

/// ノード共通の設定セクション（ブロック名・ID・リトライ・タイムアウト・エラー時の動作）と
/// フォーム部品（フィールド枠・数値入力）。

import * as React from "react";

import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { cn } from "@/lib/utils";
import type { Node as IrNode } from "@/generated/workflow-ir";
import { NODE_CATALOG } from "@/generated/workflow-catalog";
import type { EditorAction } from "../ir-state";

export function Field({
  label,
  hint,
  error,
  children,
}: {
  label: string;
  hint?: string;
  error?: string | null;
  children: React.ReactNode;
}) {
  return (
    <div className="space-y-1.5">
      <label className="text-xs font-medium text-foreground">{label}</label>
      {children}
      {error ? (
        <p className="text-[11px] text-[oklch(0.55_0.15_25)]">{error}</p>
      ) : hint ? (
        <p className="text-[11px] text-muted-foreground">{hint}</p>
      ) : null}
    </div>
  );
}

export function NumberInput({
  value,
  onChange,
  min,
  max,
  placeholder,
  error,
}: {
  value: number | null | undefined;
  onChange: (next: number | null) => void;
  min?: number;
  max?: number;
  placeholder?: string;
  error?: boolean;
}) {
  return (
    <Input
      type="number"
      inputMode="numeric"
      value={value ?? ""}
      min={min}
      max={max}
      placeholder={placeholder}
      onChange={(e) => {
        const raw = e.target.value;
        if (raw === "") return onChange(null);
        const n = Number(raw);
        if (Number.isFinite(n)) onChange(n);
      }}
      className={cn("h-8", error && "border-[oklch(0.6_0.15_25)]")}
    />
  );
}

/// 共通セクション（全ノード種）。
export function CommonSection({
  node,
  dispatch,
}: {
  node: IrNode;
  dispatch: React.Dispatch<EditorAction>;
}) {
  const entry = NODE_CATALOG.find((c) => c.type === node.type);
  const [idDraft, setIdDraft] = React.useState(node.id);
  React.useEffect(() => setIdDraft(node.id), [node.id]);

  return (
    <div className="space-y-3">
      <Field label="ブロック名" hint="キャンバスに表示される名前">
        <Input
          value={node.label ?? ""}
          placeholder={entry?.label_ja}
          onChange={(e) =>
            dispatch({
              type: "update_node",
              id: node.id,
              patch: { label: e.target.value || null } as Partial<IrNode>,
            })
          }
          className="h-8"
        />
      </Field>
      <Field
        label="ID"
        hint="他のブロックから参照するときの名前（小文字・数字・_）"
      >
        <Input
          value={idDraft}
          onChange={(e) => setIdDraft(e.target.value)}
          onBlur={() => {
            if (idDraft !== node.id) {
              dispatch({ type: "rename_node", id: node.id, nextId: idDraft });
              setIdDraft(node.id); // 不正なら reducer が無視 → 元に戻す（成功時は selection 経由で追随）。
            }
          }}
          className="h-8 font-mono text-xs"
        />
      </Field>
      <div className="grid grid-cols-2 gap-3">
        <Field label="リトライ回数" hint="失敗時にやり直す回数">
          <NumberInput
            value={node.retry?.max_attempts ?? 1}
            min={1}
            max={10}
            onChange={(n) =>
              dispatch({
                type: "update_node",
                id: node.id,
                patch: {
                  retry: {
                    ...(node.retry ?? {
                      backoff: { base_sec: 2, max_sec: 300, jitter: true },
                    }),
                    max_attempts: n ?? 1,
                  },
                } as Partial<IrNode>,
              })
            }
          />
        </Field>
        <Field
          label="時間制限（秒）"
          hint={
            entry?.timeout_default_sec
              ? `空欄で標準 ${entry.timeout_default_sec} 秒`
              : "空欄で標準"
          }
        >
          <NumberInput
            value={node.timeout_sec ?? null}
            min={1}
            max={entry?.timeout_max_sec ?? undefined}
            onChange={(n) =>
              dispatch({
                type: "update_node",
                id: node.id,
                patch: { timeout_sec: n } as Partial<IrNode>,
              })
            }
          />
        </Field>
      </div>
      <Field
        label="失敗したとき"
        hint={
          node.on_error === "continue"
            ? "「エラー時」の出口が増え、そちらに流れます"
            : "このブロックが失敗するとフロー全体が失敗します"
        }
      >
        <Select
          value={node.on_error ?? "fail_run"}
          onValueChange={(v) =>
            dispatch({
              type: "update_node",
              id: node.id,
              patch: { on_error: v } as Partial<IrNode>,
            })
          }
        >
          <SelectTrigger className="h-8">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="fail_run">フローを失敗にする</SelectItem>
            <SelectItem value="continue">エラー用の出口に流す</SelectItem>
          </SelectContent>
        </Select>
      </Field>
    </div>
  );
}
