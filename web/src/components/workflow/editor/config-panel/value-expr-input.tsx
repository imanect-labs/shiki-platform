"use client";

/// 値の入力ウィジェット（IR の ValueExpr = 固定値 / $from 参照 / $template）。
///
/// IT に詳しくない人向けに「固定値」「前の結果から」「文章を組み立て」の 3 モードで見せる。
/// $from の source は閉集合（input / trigger / each / nodes.<id>.output）で、ブロックの結果は
/// 祖先候補（呼び出し側が渡す）からセレクトする。

import * as React from "react";
import { Plus, Trash2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import type { FromRef, TemplateExpr, ValueExpr } from "@/generated/workflow-ir";

export type RefCandidate = { id: string; label: string };

type Mode = "literal" | "from" | "template";

function modeOf(value: ValueExpr | undefined): Mode {
  if (value && typeof value === "object" && value !== null) {
    if ("$from" in value) return "from";
    if ("$template" in value) return "template";
  }
  return "literal";
}

function literalText(value: unknown): { text: string; isJson: boolean } {
  if (typeof value === "string") return { text: value, isJson: false };
  if (value === undefined) return { text: "", isJson: false };
  return { text: JSON.stringify(value), isJson: true };
}

const SOURCE_LABELS: Record<string, string> = {
  input: "この実行の入力",
  trigger: "きっかけの内容",
  each: "繰り返し中の要素",
  run: "実行情報",
};

type Props = {
  label: string;
  value: ValueExpr | undefined;
  onChange: (next: ValueExpr) => void;
  /// `nodes.<id>.output` の参照候補（祖先ブロック）。
  refCandidates: RefCandidate[];
  /// map 領域内なら each を出す。
  inMapRegion?: boolean;
  placeholder?: string;
  /// 複数行の固定値（プロンプト等）。
  multiline?: boolean;
  error?: string | null;
};

export function ValueExprInput({
  label,
  value,
  onChange,
  refCandidates,
  inMapRegion,
  placeholder,
  multiline,
  error,
}: Props) {
  const mode = modeOf(value);
  const from = mode === "from" ? (value as unknown as FromRef) : null;
  const template = mode === "template" ? (value as unknown as TemplateExpr) : null;
  const literal = mode === "literal" ? literalText(value) : { text: "", isJson: false };
  const [jsonMode, setJsonMode] = React.useState(literal.isJson);

  const setMode = (next: Mode) => {
    if (next === mode) return;
    if (next === "literal") onChange("" as unknown as ValueExpr);
    // 全体参照は path 省略が正（JSON Pointer の "/" は空文字キーであり全体ではない）。
    if (next === "from") onChange({ $from: "input" } as unknown as ValueExpr);
    if (next === "template")
      onChange({ $template: "", vars: {} } as unknown as ValueExpr);
  };

  const sources: { value: string; label: string }[] = [
    { value: "input", label: SOURCE_LABELS.input },
    { value: "trigger", label: SOURCE_LABELS.trigger },
    ...(inMapRegion ? [{ value: "each", label: SOURCE_LABELS.each }] : []),
    ...refCandidates.map((c) => ({
      value: `nodes.${c.id}.output`,
      label: `「${c.label}」の結果`,
    })),
  ];

  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between gap-2">
        <label className="text-xs font-medium text-foreground">{label}</label>
        <Select value={mode} onValueChange={(v) => setMode(v as Mode)}>
          <SelectTrigger className="h-6 w-auto gap-1 border-none bg-transparent px-1.5 text-[11px] text-muted-foreground shadow-none">
            <SelectValue />
          </SelectTrigger>
          <SelectContent align="end">
            <SelectItem value="literal">固定値</SelectItem>
            <SelectItem value="from">前の結果から</SelectItem>
            <SelectItem value="template">文章を組み立て</SelectItem>
          </SelectContent>
        </Select>
      </div>

      {mode === "literal" ? (
        <div className="space-y-1">
          {multiline ? (
            <Textarea
              value={literal.text}
              rows={3}
              placeholder={placeholder}
              onChange={(e) => {
                const text = e.target.value;
                if (jsonMode) {
                  try {
                    onChange(JSON.parse(text) as ValueExpr);
                    return;
                  } catch {
                    // 不完全な JSON 入力中は文字列のまま保持。
                  }
                }
                onChange(text as unknown as ValueExpr);
              }}
              className={cn(error && "border-[oklch(0.6_0.15_25)]")}
            />
          ) : (
            <Input
              value={literal.text}
              placeholder={placeholder}
              onChange={(e) => {
                const text = e.target.value;
                if (jsonMode) {
                  try {
                    onChange(JSON.parse(text) as ValueExpr);
                    return;
                  } catch {
                    // 入力途中。
                  }
                }
                onChange(text as unknown as ValueExpr);
              }}
              className={cn("h-8", error && "border-[oklch(0.6_0.15_25)]")}
            />
          )}
          <label className="flex items-center gap-1.5 text-[11px] text-muted-foreground">
            <Switch
              checked={jsonMode}
              onCheckedChange={(on) => {
                setJsonMode(on);
                // 切替時に IR 内の値も追随させる（ON: 今のテキストを JSON として解釈、
                // OFF: 文字列に戻す）。トグルだけで型が変わらない齟齬を防ぐ。
                if (on) {
                  try {
                    onChange(JSON.parse(literal.text) as ValueExpr);
                  } catch {
                    // 解釈できないテキストは文字列のまま（入力継続で解釈される）。
                  }
                } else if (literal.isJson) {
                  onChange(literal.text as unknown as ValueExpr);
                }
              }}
              className="scale-75"
            />
            数値やリストとして扱う（JSON）
          </label>
        </div>
      ) : null}

      {mode === "from" && from ? (
        <div className="space-y-1.5 rounded-md border bg-muted/40 p-2">
          <Select
            value={from.$from}
            onValueChange={(v) =>
              onChange({ ...from, $from: v } as unknown as ValueExpr)
            }
          >
            <SelectTrigger className="h-8">
              <SelectValue placeholder="参照元" />
            </SelectTrigger>
            <SelectContent>
              {sources.map((s) => (
                <SelectItem key={s.value} value={s.value}>
                  {s.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Input
            value={from.path ?? ""}
            onChange={(e) =>
              onChange({
                ...from,
                path: e.target.value || undefined,
              } as unknown as ValueExpr)
            }
            placeholder="取り出す場所（例 /items/0/name・空で全体）"
            className="h-8 font-mono text-xs"
          />
        </div>
      ) : null}

      {mode === "template" && template ? (
        <TemplateEditor
          template={template}
          sources={sources}
          onChange={(next) => onChange(next as unknown as ValueExpr)}
        />
      ) : null}

      {error ? <p className="text-[11px] text-[oklch(0.55_0.15_25)]">{error}</p> : null}
    </div>
  );
}

function TemplateEditor({
  template,
  sources,
  onChange,
}: {
  template: TemplateExpr;
  sources: { value: string; label: string }[];
  onChange: (next: TemplateExpr) => void;
}) {
  const vars = Object.entries(template.vars ?? {});
  return (
    <div className="space-y-1.5 rounded-md border bg-muted/40 p-2">
      <Textarea
        value={template.$template}
        rows={3}
        placeholder={"例: {name} さんの申請が届きました"}
        onChange={(e) => onChange({ ...template, $template: e.target.value })}
      />
      <p className="text-[11px] text-muted-foreground">
        {"{名前}"} の部分に下の値が入ります
      </p>
      {vars.map(([name, expr], i) => {
        const ref =
          expr && typeof expr === "object" && "$from" in (expr as object)
            ? (expr as unknown as FromRef)
            : null;
        return (
          <div key={i} className="flex items-center gap-1.5">
            <Input
              value={name}
              onChange={(e) => {
                const next = { ...(template.vars ?? {}) };
                delete next[name];
                next[e.target.value] = expr;
                onChange({ ...template, vars: next });
              }}
              placeholder="名前"
              className="h-7 w-20 text-xs"
              aria-label="差し込み名"
            />
            <Select
              value={ref?.$from ?? "input"}
              onValueChange={(v) => {
                const next = { ...(template.vars ?? {}) };
                next[name] = {
                  $from: v,
                  path: ref?.path,
                } as unknown as ValueExpr;
                onChange({ ...template, vars: next });
              }}
            >
              <SelectTrigger className="h-7 flex-1 text-xs">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {sources.map((s) => (
                  <SelectItem key={s.value} value={s.value}>
                    {s.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Input
              value={ref?.path ?? ""}
              onChange={(e) => {
                const next = { ...(template.vars ?? {}) };
                next[name] = {
                  $from: ref?.$from ?? "input",
                  path: e.target.value || undefined,
                } as unknown as ValueExpr;
                onChange({ ...template, vars: next });
              }}
              placeholder="/path"
              className="h-7 w-24 font-mono text-[11px]"
              aria-label="取り出す場所"
            />
            <Button
              variant="ghost"
              size="icon"
              className="size-7"
              aria-label="差し込みを削除"
              onClick={() => {
                const next = { ...(template.vars ?? {}) };
                delete next[name];
                onChange({ ...template, vars: next });
              }}
            >
              <Trash2 className="size-3.5" aria-hidden />
            </Button>
          </div>
        );
      })}
      <Button
        variant="outline"
        size="sm"
        className="h-7 text-xs"
        onClick={() => {
          const next = { ...(template.vars ?? {}) };
          let n = vars.length + 1;
          while (next[`v${n}`]) n += 1;
          next[`v${n}`] = { $from: "input" } as unknown as ValueExpr;
          onChange({ ...template, vars: next });
        }}
      >
        <Plus className="size-3.5" aria-hidden />
        差し込みを追加
      </Button>
    </div>
  );
}
