// prompt-kit (https://www.prompt-kit.com) 由来の Reasoning を本リポジトリ向けに実装。MIT License。
// LLM の思考（reasoning/thinking）を折りたたみ表示する。ストリーミング中は自動展開＋シマーで
// 「考えている」感を出す。
"use client";

import * as React from "react";
import { ChevronRight, Brain } from "lucide-react";

import { cn } from "@/lib/utils";

export function Reasoning({
  text,
  streaming = false,
}: {
  text: string;
  streaming?: boolean;
}) {
  const [open, setOpen] = React.useState(false);
  // ストリーミング中は自動で開き、完了後はユーザー操作に委ねる。
  const [userToggled, setUserToggled] = React.useState(false);
  const expanded = userToggled ? open : streaming || open;

  if (!text && !streaming) return null;

  return (
    <div className="mb-2">
      <button
        type="button"
        onClick={() => {
          setUserToggled(true);
          setOpen((v) => !v);
        }}
        className="flex items-center gap-1.5 rounded-md py-0.5 text-[13px] text-muted-foreground transition-colors hover:text-foreground"
      >
        <Brain className="size-3.5" aria-hidden />
        <span className={cn(streaming && "animate-pulse")}>
          {streaming ? "考えています…" : "思考プロセス"}
        </span>
        <ChevronRight
          className={cn("size-3.5 transition-transform", expanded && "rotate-90")}
          aria-hidden
        />
      </button>
      {expanded && text ? (
        <div className="mt-1.5 whitespace-pre-wrap border-l-2 border-border pl-3 text-[13px] leading-relaxed text-muted-foreground">
          {text}
        </div>
      ) : null}
    </div>
  );
}
