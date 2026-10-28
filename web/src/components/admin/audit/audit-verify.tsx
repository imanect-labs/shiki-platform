"use client";

import * as React from "react";
import { AlertTriangle, CheckCircle2, Link2, Loader2 } from "lucide-react";

import { verifyAuditChain, type ChainReport } from "@/lib/audit-api";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { formatInt } from "../format";
import { formatTime } from "./audit-query";
import { actionLabel } from "./audit-vocab";

type Progress =
  | { state: "running"; checked: number; legacy: number }
  | { state: "ok"; checked: number; legacy: number }
  | { state: "broken"; checked: number; legacy: number; broken: NonNullable<ChainReport["broken"]> }
  | { state: "error"; checked: number; message: string };

const REASON: Record<string, string> = {
  prev_mismatch: "直前の記録とつながっていません（記録の削除・差し込みの疑い）",
  hash_mismatch: "記録の内容が記録時と違います（書き換えの疑い）",
};

/// 監査ログのハッシュチェーンを先頭から確かめる（#828）。何を保証し何を保証しないかも添える（PIT-12）。
export function VerifyChainButton() {
  const [open, setOpen] = React.useState(false);
  const [progress, setProgress] = React.useState<Progress | null>(null);
  // 実行ごとの番号。閉じる・もう一度確かめると番号が進み、古い実行は結果を書かず続きも取りに行かない。
  const runId = React.useRef(0);
  React.useEffect(() => () => void (runId.current += 1), []);

  const run = React.useCallback(async () => {
    const id = ++runId.current;
    const stale = () => id !== runId.current;
    let checked = 0;
    let legacy = 0;
    let after: number | null = null;
    setProgress({ state: "running", checked, legacy });
    try {
      // 1 回で確かめる行数に上限があるので、末尾か崩れた行まで続ける。
      for (;;) {
        const r: ChainReport = await verifyAuditChain(after);
        if (stale()) return;
        checked += r.checked;
        legacy += r.legacy;
        if (r.broken) {
          setProgress({ state: "broken", checked, legacy, broken: r.broken });
          return;
        }
        if (r.complete || r.last_id == null) {
          setProgress({ state: "ok", checked, legacy });
          return;
        }
        after = r.last_id;
        setProgress({ state: "running", checked, legacy });
      }
    } catch (e) {
      if (!stale()) {
        setProgress({ state: "error", checked, message: e instanceof Error ? e.message : String(e) });
      }
    }
  }, []);

  const onOpenChange = (o: boolean) => {
    setOpen(o);
    if (o) void run();
    else runId.current += 1;
  };

  return (
    <>
      <Button variant="outline" size="sm" onClick={() => onOpenChange(true)}>
        <Link2 aria-hidden />
        チェーンを検証
      </Button>
      <Dialog open={open} onOpenChange={onOpenChange}>
        <DialogContent className="max-w-md gap-4" data-testid="audit-verify">
          <DialogHeader>
            <DialogTitle>監査ログのチェーンを検証</DialogTitle>
            <DialogDescription>
              実データを変える操作の記録を、古い順につながりと内容が記録時のままかを確かめます。検証したことも監査ログに残ります。
            </DialogDescription>
          </DialogHeader>
          <div aria-live="polite" className="flex flex-col gap-2 text-sm">
            {progress?.state === "running" || progress === null ? (
              <p className="flex items-center gap-2">
                <Loader2 className="size-4 animate-spin" aria-hidden />
                確かめています…（{formatInt(progress?.checked ?? 0)} 件）
              </p>
            ) : progress.state === "ok" && progress.checked === 0 ? (
              <p className="flex items-start gap-2">
                <CheckCircle2 className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
                連結された記録はまだありません（ファイルの変更や共有など、実データを変える操作から記録されます）。
              </p>
            ) : progress.state === "ok" ? (
              <p className="flex items-start gap-2">
                <CheckCircle2 className="mt-0.5 size-4 shrink-0 text-emerald-600 dark:text-emerald-400" aria-hidden />
                <span>
                  崩れていません（{formatInt(progress.checked)} 件を確認）。
                  {progress.legacy > 0
                    ? ` うち ${formatInt(progress.legacy)} 件は作成時刻を含まない古い形式の記録です。`
                    : null}
                </span>
              </p>
            ) : progress.state === "broken" ? (
              <div role="alert" className="flex items-start gap-2 text-destructive">
                <AlertTriangle className="mt-0.5 size-4 shrink-0" aria-hidden />
                <div className="flex flex-col gap-1">
                  <span className="font-medium">チェーンが崩れている記録があります</span>
                  <span>{REASON[progress.broken.reason] ?? progress.broken.reason}</span>
                  <span className="font-mono text-xs text-foreground">
                    #{progress.broken.id}・{formatTime(progress.broken.created_at)}・
                    {actionLabel(progress.broken.action) ?? progress.broken.action}
                  </span>
                  <span className="text-xs text-muted-foreground">
                    それより前の {formatInt(Math.max(progress.checked - 1, 0))} 件は崩れていません。
                  </span>
                </div>
              </div>
            ) : (
              <p role="alert" className="flex items-start gap-2 text-destructive">
                <AlertTriangle className="mt-0.5 size-4 shrink-0" aria-hidden />
                検証できませんでした: {progress.message}
              </p>
            )}
            <p className="text-xs leading-relaxed text-muted-foreground">
              記録の書き換え・削除・差し込みを検知できます。DB に書き込める人が、それ以降の記録をすべて計算し直す改ざんは検知できません。読み取り・拒否の記録はチェーンに入りません。
            </p>
          </div>
          <DialogFooter>
            <Button variant="outline" size="sm" onClick={() => onOpenChange(false)}>
              閉じる
            </Button>
            {progress && progress.state !== "running" ? (
              <Button size="sm" onClick={() => void run()}>
                もう一度確かめる
              </Button>
            ) : null}
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
