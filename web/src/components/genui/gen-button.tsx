"use client";

/// generative UI のボタン（Task 6.6）。押下は宣言済みアクションへの dispatch のみ。

import * as React from "react";

import { CheckCircle2, Loader2 } from "lucide-react";

import type { ButtonProps as GenButtonProps } from "@/generated/gui-spec";
import { Button } from "@/components/ui/button";
import { UiActionAlreadyInvoked } from "@/lib/artifact-api";
import { useGenUiAction } from "./action-context";
import { ActionResultNote, describeActionError, describeActionResult } from "./action-result";

export function GenUiButton({ button }: { button: GenButtonProps }) {
  const { dispatch, invoked, onActionCompleted } = useGenUiAction();
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const [note, setNote] = React.useState<string | null>(null);
  // サーバが 409 を返した経路（別タブで先に実行された等）はここで覚える。この経路は
  // 会話の再読込を起こさないので、`invoked` だけに頼るとボタンが有効なまま残る。
  const [alreadyInvoked, setAlreadyInvoked] = React.useState(false);
  // 1 回だけ実行できる束縛（chat.submit）は実行済みなら押せない（#410）。繰り返せる
  // 束縛はサーバが記録しないので常に false ＝何度でも押せる（従来どおり）。
  const spent = alreadyInvoked || invoked.has(button.on_click.action);

  const onClick = async () => {
    if (busy || spent) return;
    setBusy(true);
    setError(null);
    setNote(null);
    try {
      const result = await dispatch(button.on_click.action, {});
      setNote(describeActionResult(result));
      onActionCompleted?.(result);
    } catch (err) {
      // 既に送信済みは失敗ではない（表示が追いつく前の二度押し）。押せない状態へ倒す。
      if (err instanceof UiActionAlreadyInvoked) setAlreadyInvoked(true);
      else setError(describeActionError(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="space-y-1.5">
      <div className="flex items-center gap-2">
        <Button
          type="button"
          size="sm"
          variant={button.variant === "secondary" ? "secondary" : "default"}
          onClick={() => void onClick()}
          disabled={busy || spent}
        >
          {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
          {button.label}
        </Button>
        {(note || spent) && !error ? (
          <span className="inline-flex items-center gap-1 text-xs text-primary">
            <CheckCircle2 className="size-3.5" aria-hidden />
            {spent && !note ? "送信済みです" : "完了"}
          </span>
        ) : null}
      </div>
      <ActionResultNote error={error} note={note} />
    </div>
  );
}
