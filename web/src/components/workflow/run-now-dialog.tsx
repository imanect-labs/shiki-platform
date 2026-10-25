"use client";

/// 手動実行ダイアログ（対話トリガ・実行主体 = 自分）。
///
/// input_schema の properties があれば 1 階層の簡易フォーム、無ければ JSON 入力。
/// 項目の型に合わせて入力欄を変える（日付は日付の欄、選択肢と真偽は選ぶ欄・GAP-12）。表示名（title）があれば
/// それを見出しにし、必須の項目には印を付ける（入力の検証はサーバが行う）。
/// 実行後は実行履歴ページ（?run= deep-link）へ誘導する。

import * as React from "react";
import { useRouter } from "next/navigation";
import { Loader2, Play } from "lucide-react";

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
import { getWorkflow } from "@/lib/workflow-api";
import { startRun } from "@/lib/workflow-run-api";
import { inputFieldsOf, type InputFieldInfo } from "@/lib/workflow-input-schema";
import type { WorkflowIr } from "@/generated/workflow-ir";

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
}: {
  field: InputFieldInfo;
  value: string;
  onChange: (next: string) => void;
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
        <SelectTrigger className="h-8" aria-label={label}>
          <SelectValue placeholder={field.description ?? "選んでください"} />
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
      placeholder={field.description ?? undefined}
      aria-label={label}
      className="h-8"
    />
  );
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
  const [fields, setFields] = React.useState<Record<string, string>>({});
  const [jsonText, setJsonText] = React.useState("{}");
  const [busy, setBusy] = React.useState(false);

  const run = async () => {
    let input: unknown = {};
    if (props) {
      input = Object.fromEntries(
        Object.entries(fields)
          .filter(([, v]) => v !== "")
          .map(([k, v]) => [k, coerceValue(v, props.find((f) => f.key === k)?.kind)]),
      );
    } else {
      try {
        input = jsonText.trim() ? JSON.parse(jsonText) : {};
      } catch {
        toast({ variant: "destructive", title: "入力が JSON として読めません" });
        return;
      }
    }
    setBusy(true);
    try {
      // 実行は常に最新保存版で走る（backend 仕様）。別の編集者が保存していた場合、
      // このフォームは古いスキーマなので実行せず再読込を促す。
      const latest = await getWorkflow(workflowId);
      if (latest.version !== version) {
        toast({
          variant: "destructive",
          title: "ワークフローが更新されています",
          description: "ページを再読み込みして、最新の内容で実行してください。",
        });
        return;
      }
      const runId = await startRun(workflowId, input);
      onOpenChange(false);
      if (runId) {
        toast({ title: "実行を開始しました" });
        router.push(`/workflows/${workflowId}/runs?run=${runId}`);
      } else {
        toast({
          title: "実行は受け付けられませんでした",
          description: "同時実行の上限（skip 設定）の可能性があります。",
        });
      }
    } catch (e) {
      toast({
        variant: "destructive",
        title: "実行に失敗しました",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Play className="size-5 text-primary" aria-hidden />
            いま実行する
          </DialogTitle>
          <DialogDescription>
            あなたの権限で 1 回実行します（読める範囲・書ける範囲はあなたと同じ）。
          </DialogDescription>
        </DialogHeader>
        {props ? (
          <div className="space-y-3">
            {props.map((field) => (
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
                />
              </div>
            ))}
          </div>
        ) : (
          <div className="space-y-1">
            <label className="text-xs font-medium">入力（JSON・省略可）</label>
            <Textarea
              value={jsonText}
              rows={4}
              onChange={(e) => setJsonText(e.target.value)}
              className="font-mono text-xs"
            />
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
