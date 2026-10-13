import { FlaskConical } from "lucide-react";

import { cn } from "@/lib/utils";

/// API が未実装のモック画面であることを示す小バッジ。
export function MockBadge({ className }: { className?: string }) {
  return (
    <span
      className={cn(
        "rounded border border-dashed border-border px-1.5 py-px font-mono text-[10px] text-muted-foreground",
        className,
      )}
    >
      モック
    </span>
  );
}

/// モック画面の先頭に出す注意書き（表示はサンプルで、操作は保存されない）。
export function MockBanner({ what }: { what: string }) {
  return (
    <div className="flex items-start gap-2.5 rounded-lg border border-dashed border-border bg-muted/40 px-3.5 py-2.5 text-[13px] text-muted-foreground">
      <FlaskConical className="mt-0.5 size-4 shrink-0" aria-hidden />
      <p>
        この画面はモックです。表示はサンプルで、操作しても保存されません（{what}）。
      </p>
    </div>
  );
}
