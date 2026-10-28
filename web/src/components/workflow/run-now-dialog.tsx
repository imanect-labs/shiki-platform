"use client";

/// 手動実行ダイアログ（対話トリガ・実行主体 = 自分）。
///
/// input_schema の properties があれば 1 階層の簡易フォーム、無ければ JSON 入力。
/// 項目の型に合わせて入力欄を変える（日付は日付の欄、選択肢と真偽は選ぶ欄・GAP-12）。表示名（title）があれば
/// それを見出しにし、必須の項目には印を付ける。
/// 実行後は実行履歴ページ（?run= deep-link）へ誘導する。
///
/// - 説明は付録 C の §3.6 の決まった文（使ってよい操作でさらに絞ることを言う・BUG-109）。
/// - 必須の欄が空なら送らずに欄の下で知らせる。サーバーの入力の誤りは、項目のキーを見出しに直して欄の下に出し、
///   ほかの理由（実行できない設定・見つからない・使えない）はダイアログの中の帯に出す（本文を捨てない・BUG-109）。

import * as React from "react";
import { useRouter } from "next/navigation";
import { AlertTriangle, Loader2, Play } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { toast } from "@/components/ui/use-toast";
import { cn } from "@/lib/utils";
import { getWorkflow } from "@/lib/workflow-api";
import { RunStartError, startRun } from "@/lib/workflow-run-api";
import { inputFieldsOf, type InputFieldInfo } from "@/lib/workflow-input-schema";
import type { WorkflowIr } from "@/generated/workflow-ir";

/// 付録 C の §3.6 の決まった文（手動の実行のダイアログ）。
const RUN_NOTE = "あなたの権限で 1 回実行します。このワークフローが使ってよい操作のうち、あなたにできることだけを行います。";
/// 実行履歴に残る値（付録 D の N4.7 の 3 文目）。
const HISTORY_NOTE = "実行履歴には、各ブロックが読んだ値と結果が残り、このワークフローを見られる人が読めます。";
const JSON_SYNTAX = "JSON の書き方が正しくありません。引用符、カンマ、括弧の対応を確かめてください。";

/// input_schema の型宣言に合わせて文字列入力を変換する（number/boolean）。
/// 変換できない値は文字列のまま渡す（実行側の検証・エラーに委ねる）。
function coerceValue(raw: string, kind: InputFieldInfo["kind"] | undefined): unknown {
  if (kind === "number" || kind === "integer") {
    const n = Number(raw);
    return Number.isFinite(n) && raw.trim() !== "" ? n : raw;
  }
  if (kind === "boolean") {
    if (raw === "true") return true;
    if (raw === "false") return false;
    return raw;
  }
  return raw;
}

function schemaFields(ir: WorkflowIr): InputFieldInfo[] | null {
  const fields = inputFieldsOf(ir.input_schema);
  return fields.length > 0 ? fields : null;
}

/// 1 項目の入力欄（型ごと）。値は文字で持ち、送るときに型へ変換する。
function FieldInput({
  field,
  value,
  onChange,
  invalid,
  describedBy,
}: {
  field: InputFieldInfo;
  value: string;
  onChange: (next: string) => void;
  invalid: boolean;
  describedBy: string | undefined;
}) {
  const label = field.title ?? field.key;
  if (field.kind === "enum" || field.kind === "boolean") {
    const options =
      field.kind === "boolean"
        ? [
            { value: "true", label: "はい" },
            { value: "false", label: "いいえ" },
          ]
        : field.options.map((o) => ({ value: o, label: o }));
    return (
      <Select value={value} onValueChange={onChange}>
        <SelectTrigger
          className={cn("h-8", invalid && "border-destructive")}
          aria-label={label}
          aria-invalid={invalid || undefined}
          aria-describedby={describedBy}
        >
          <SelectValue placeholder="選んでください" />
        </SelectTrigger>
        <SelectContent>
          {options.map((o) => (
            <SelectItem key={o.value} value={o.value}>
              {o.label}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    );
  }
  return (
    <Input
      type={field.kind === "date" ? "date" : field.kind === "number" || field.kind === "integer" ? "number" : "text"}
      inputMode={field.kind === "integer" ? "numeric" : field.kind === "number" ? "decimal" : undefined}
      value={value}
      onChange={(e) => onChange(e.target.value)}
      aria-label={label}
      aria-invalid={invalid || undefined}
      aria-describedby={describedBy}
      className={cn("h-8", invalid && "border-destructive")}
    />
  );
}

/// サーバーの入力の誤りのキー（`addr.zip`・`tags[2]`）から、表の項目のキー（最上位）を取り出す。
function topKey(key: string): string {
  return key.split(/[.[]/)[0] ?? "";
}

export function RunNowDialog({
  open,
  onOpenChange,
  workflowId,
  ir,
  version,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  workflowId: string;
  ir: WorkflowIr;
  /// フォームの元になった保存済みバージョン（実行前に最新と一致することを確認する）。
  version: number;
}) {
  const router = useRouter();
  const props = schemaFields(ir);
  const idBase = React.useId();
  const [fields, setFields] = React.useState<Record<string, string>>({});
  const [jsonText, setJsonText] = React.useState("{}");
  const [busy, setBusy] = React.useState(false);
  /// 欄の下の誤り（項目のキー → 文・`$input` は入力の JSON）。
  const [fieldErrors, setFieldErrors] = React.useState<Record<string, string>>({});
  /// 欄に写せない誤り（ダイアログの中の帯）。
  const [failure, setFailure] = React.useState<{ message: string; lines: string[] } | null>(null);
  const labels = React.useMemo(() => new Map(ir.nodes.map((n) => [n.id, n.label || n.id])), [ir.nodes]);

  React.useEffect(() => {
    if (!open) return;
    setFieldErrors({});
    setFailure(null);
  }, [open]);

  const run = async () => {
    const errors: Record<string, string> = {};
    let input: unknown = {};
    if (props) {
      for (const f of props) {
        if (f.required && !(fields[f.key] ?? "").trim()) {
          errors[f.key] = `「${f.title ?? f.key}」を${f.kind === "enum" || f.kind === "boolean" ? "選んで" : "入れて"}ください。`;
        }
      }
      input = Object.fromEntries(
        Object.entries(fields)
          .filter(([, v]) => v !== "")
          .map(([k, v]) => [k, coerceValue(v, props.find((f) => f.key === k)?.kind)]),
      );
    } else {
      try {
        input = jsonText.trim() ? JSON.parse(jsonText) : {};
      } catch {
        errors.$input = JSON_SYNTAX;
      }
    }
    setFieldErrors(errors);
    setFailure(null);
    if (Object.keys(errors).length > 0) return;
    setBusy(true);
    try {
      // 実行は常に最新保存版で走る（backend 仕様）。別の編集者が保存していた場合、
      // このフォームは古いスキーマなので実行せず再読込を促す。
      const latest = await getWorkflow(workflowId);
      if (latest.version !== version) {
        toast({
          variant: "destructive",
          title: "ほかの人が先にワークフローを保存しました",
          description:
            "実行は最新のバージョンで動くため、入力項目が変わっていることがあります。最新の内容を読み込んでから、もう一度実行してください。",
        });
        return;
      }
      const runId = await startRun(workflowId, input);
      if (runId) {
        onOpenChange(false);
        toast({ title: "実行を始めました", description: "実行履歴で進み具合を見られます。" });
        router.push(`/workflows/${workflowId}/runs?run=${runId}`);
      } else {
        setFailure({
          message:
            "同時に動かせる実行の数に達していて、このワークフローは上限を超えた実行を見送る設定です。いまの実行が終わってから、もう一度実行してください。",
          lines: [],
        });
      }
    } catch (e) {
      if (!(e instanceof RunStartError)) {
        setFailure({ message: e instanceof Error ? e.message : String(e), lines: [] });
        return;
      }
      // 入力の誤りは欄の下へ（表に無い項目・入力全体の誤りは帯へ）。
      const shown: Record<string, string> = {};
      const rest: string[] = [];
      for (const issue of e.issues) {
        const key = topKey(issue.key);
        const field = props?.find((f) => f.key === key);
        if (field && !shown[key]) {
          shown[key] = `${issue.message}。`;
        } else if (!props && !shown.$input) {
          shown.$input = `${issue.key || "入力"}: ${issue.message}。`;
        } else {
          rest.push(`${issue.key || "入力"}: ${issue.message}`);
        }
      }
      setFieldErrors(shown);
      const lines = [
        ...rest,
        ...e.reasons.map((r) => (r.nodeId ? `「${labels.get(r.nodeId) ?? r.nodeId}」: ${r.message}` : r.message)),
      ];
      if (lines.length > 0 || e.issues.length === 0) {
        setFailure({ message: e.message, lines });
      }
    } finally {
      setBusy(false);
    }
  };

  const errorId = (key: string) => `${idBase}-${key}-error`;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Play className="size-5 text-primary" aria-hidden />
            いま実行する
          </DialogTitle>
          <DialogDescription className="space-y-1">
            <span className="block">{RUN_NOTE}</span>
            <span className="block">{HISTORY_NOTE}</span>
          </DialogDescription>
        </DialogHeader>
        {failure ? (
          <div
            role="alert"
            className="space-y-1 rounded-md border border-destructive/40 bg-destructive/5 p-2.5 text-xs"
            data-testid="run-now-failure"
          >
            <p className="flex items-center gap-1.5 font-medium text-destructive">
              <AlertTriangle className="size-4 shrink-0" aria-hidden />
              実行できませんでした
            </p>
            <p className="text-foreground/80">{failure.message}</p>
            {failure.lines.length > 0 ? (
              <ul className="list-disc space-y-0.5 pl-5 text-foreground/80">
                {failure.lines.map((l, i) => (
                  <li key={i}>{l}</li>
                ))}
              </ul>
            ) : null}
          </div>
        ) : null}
        {props ? (
          <div className="space-y-3">
            {props.map((field) => {
              const error = fieldErrors[field.key];
              const noteId = field.description ? `${idBase}-${field.key}-note` : undefined;
              return (
                <div key={field.key} className="space-y-1">
                  <label className="text-xs font-medium">
                    {field.title ?? field.key}
                    {field.required ? (
                      <>
                        <span className="ml-1 text-destructive" aria-hidden>
                          *
                        </span>
                        <span className="sr-only">（必須）</span>
                      </>
                    ) : null}
                  </label>
                  <FieldInput
                    field={field}
                    value={fields[field.key] ?? ""}
                    onChange={(v) => setFields({ ...fields, [field.key]: v })}
                    invalid={Boolean(error)}
                    describedBy={[error ? errorId(field.key) : null, noteId].filter(Boolean).join(" ") || undefined}
                  />
                  {error ? (
                    <p id={errorId(field.key)} className="text-[11px] text-destructive">
                      {error}
                    </p>
                  ) : null}
                  {field.description ? (
                    <p id={noteId} className="text-[11px] text-muted-foreground">
                      {field.description}
                    </p>
                  ) : null}
                </div>
              );
            })}
          </div>
        ) : (
          <div className="space-y-1">
            <label className="text-xs font-medium" htmlFor={`${idBase}-input`}>
              入力（JSON・省略可）
            </label>
            <Textarea
              id={`${idBase}-input`}
              value={jsonText}
              rows={4}
              onChange={(e) => setJsonText(e.target.value)}
              aria-invalid={fieldErrors.$input ? true : undefined}
              aria-describedby={fieldErrors.$input ? errorId("input") : undefined}
              className={cn("font-mono text-xs", fieldErrors.$input && "border-destructive")}
            />
            {fieldErrors.$input ? (
              <p id={errorId("input")} className="text-[11px] text-destructive">
                {fieldErrors.$input}
              </p>
            ) : null}
          </div>
        )}
        <div className="flex justify-end">
          <Button onClick={run} disabled={busy}>
            {busy ? (
              <Loader2 className="size-4 animate-spin" aria-hidden />
            ) : (
              <Play className="size-4" aria-hidden />
            )}
            実行する
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
