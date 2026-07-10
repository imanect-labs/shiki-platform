"use client";

/// 手動実行ダイアログ（対話トリガ・実行主体 = 自分）。
///
/// input_schema の properties があれば 1 階層の簡易フォーム、無ければ JSON 入力。
/// 実行後は実行履歴ページ（?run= deep-link）へ誘導する。

import * as React from "react";
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
import { Textarea } from "@/components/ui/textarea";
import { toast } from "@/components/ui/use-toast";
import { startRun } from "@/lib/workflow-run-api";
import type { WorkflowIr } from "@/generated/workflow-ir";

type SchemaProps = Record<string, { type?: string; description?: string }>;

function schemaProps(ir: WorkflowIr): SchemaProps | null {
  const schema = ir.input_schema as
    | { type?: string; properties?: SchemaProps }
    | null
    | undefined;
  if (schema?.properties && Object.keys(schema.properties).length > 0) {
    return schema.properties;
  }
  return null;
}

export function RunNowDialog({
  open,
  onOpenChange,
  workflowId,
  ir,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  workflowId: string;
  ir: WorkflowIr;
}) {
  const props = schemaProps(ir);
  const [fields, setFields] = React.useState<Record<string, string>>({});
  const [jsonText, setJsonText] = React.useState("{}");
  const [busy, setBusy] = React.useState(false);

  const run = async () => {
    let input: unknown = {};
    if (props) {
      input = Object.fromEntries(
        Object.entries(fields).filter(([, v]) => v !== ""),
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
      const runId = await startRun(workflowId, input);
      onOpenChange(false);
      if (runId) {
        // 実行履歴ページ（次 PR）が入り次第 ?run= の deep-link へ遷移させる。
        toast({ title: "実行を開始しました", description: "実行履歴で進行を確認できます。" });
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
            {Object.entries(props).map(([name, spec]) => (
              <div key={name} className="space-y-1">
                <label className="text-xs font-medium">{name}</label>
                <Input
                  value={fields[name] ?? ""}
                  onChange={(e) => setFields({ ...fields, [name]: e.target.value })}
                  placeholder={spec.description}
                  className="h-8"
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
