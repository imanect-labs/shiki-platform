"use client";

/// 編集画面の読み込み中プレースホルダ（human 要望: ぐるぐるスピナーだけは安っぽい）。
///
/// 開こうとしている文書の「形」をスケルトンで先に見せ、shimmer を流して待ち時間を
/// 演出する。種別ごとに骨格を変える（doc=段落・slide=キャンバス＋フィルムストリップ・
/// sheet=グリッド）。ブランドの季節色をアクセントに小さく添える。

import { seasonVar, currentSeasonIndex } from "@/lib/season";
import { cn } from "@/lib/utils";

type Kind = "doc" | "slide" | "sheet";

function Bar({ w, className }: { w: string; className?: string }) {
  return <div className={cn("shiki-shimmer h-3.5 rounded-md", className)} style={{ width: w }} />;
}

function DocSkeleton() {
  return (
    <div className="mx-auto flex w-full max-w-2xl flex-col gap-3.5 px-2">
      <div className="shiki-shimmer mb-2 h-7 w-1/2 rounded-md" />
      <Bar w="100%" />
      <Bar w="96%" />
      <Bar w="88%" />
      <div className="h-2" />
      <div className="shiki-shimmer h-5 w-1/3 rounded-md" />
      <Bar w="92%" />
      <Bar w="99%" />
      <Bar w="70%" />
    </div>
  );
}

function SlideSkeleton() {
  return (
    <div className="mx-auto flex w-full max-w-3xl gap-4 px-2">
      <div className="hidden w-28 shrink-0 flex-col gap-2.5 sm:flex">
        {[0, 1, 2].map((i) => (
          <div key={i} className="shiki-shimmer aspect-video w-full rounded-md" />
        ))}
      </div>
      <div className="shiki-shimmer aspect-video flex-1 rounded-xl" />
    </div>
  );
}

function SheetSkeleton() {
  return (
    <div className="mx-auto w-full max-w-3xl px-2">
      <div className="overflow-hidden rounded-lg border border-border/60">
        {[0, 1, 2, 3, 4, 5].map((r) => (
          <div key={r} className="flex gap-px bg-border/40">
            {[0, 1, 2, 3].map((c) => (
              <div
                key={c}
                className={cn(
                  "h-8 flex-1 bg-card",
                  r === 0 && "bg-muted",
                )}
              >
                <div className="shiki-shimmer m-1.5 h-4 rounded-sm" style={{ opacity: r === 0 ? 0.9 : 0.6 }} />
              </div>
            ))}
          </div>
        ))}
      </div>
    </div>
  );
}

export function EditorLoading({
  kind = "doc",
  message,
}: {
  kind?: Kind;
  message?: string;
}) {
  const accent = seasonVar(currentSeasonIndex());
  return (
    <div
      className="flex h-full w-full flex-col items-center justify-center gap-8 px-6 py-10"
      aria-busy
      role="status"
      data-testid="editor-loading"
    >
      <div className="w-full opacity-70">
        {kind === "slide" ? <SlideSkeleton /> : kind === "sheet" ? <SheetSkeleton /> : <DocSkeleton />}
      </div>
      <div className="flex items-center gap-2.5 text-sm text-muted-foreground">
        {/* 季節色の小さなドットが脈打つ（スピナーより落ち着いた"生きている"合図）。 */}
        <span className="relative flex size-2.5">
          <span
            className="absolute inline-flex h-full w-full animate-ping rounded-full opacity-60"
            style={{ backgroundColor: accent }}
          />
          <span className="relative inline-flex size-2.5 rounded-full" style={{ backgroundColor: accent }} />
        </span>
        {message ?? "開いています…"}
      </div>
    </div>
  );
}
