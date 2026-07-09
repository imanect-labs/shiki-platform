"use client";

/// generative UI のボタン（Task 6.6）。押下は宣言済みアクションへの dispatch のみ。

import * as React from "react";

import { CheckCircle2, Loader2 } from "lucide-react";

import type { ButtonProps as GenButtonProps } from "@/generated/gui-spec";
import { Button } from "@/components/ui/button";
import { useGenUiAction } from "./action-context";
import { ActionResultNote, describeActionError, describeActionResult } from "./action-result";

export function GenUiButton({ button }: { button: GenButtonProps }) {
  const { dispatch, onActionCompleted } = useGenUiAction();
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const [note, setNote] = React.useState<string | null>(null);

  const onClick = async () => {
    if (busy) return;
    setBusy(true);
    setError(null);
    setNote(null);
    try {
      const result = await dispatch(button.on_click.action, {});
      setNote(describeActionResult(result));
      onActionCompleted?.(result);
    } catch (err) {
      setError(describeActionError(err));
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
          disabled={busy}
        >
          {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
          {button.label}
        </Button>
        {note && !error ? (
          <span className="inline-flex items-center gap-1 text-xs text-primary">
            <CheckCircle2 className="size-3.5" aria-hidden />
            完了
          </span>
        ) : null}
      </div>
      <ActionResultNote error={error} note={note} />
    </div>
  );
}
