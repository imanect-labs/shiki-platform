"use client";

/// 「AI に聞く」（llm.invoke）のモデル欄（BUG-4）。
///
/// 選べるのは組織の管理者が公開したモデル（`GET /llm/models`・チャットのモデル選択と同じ一覧と部品）。
/// ボタンに今のモデル名を出し、押すと一覧を開く。PC はポップオーバー、設定パネルが下からのシートの
/// ときはシートの中に一覧を広げる（シートの上にシートを重ねない）。
///
/// - 読み込み中: Skeleton
/// - 一覧を取れない: 自由入力に戻し、注意書きを出す（保存時の照合はサーバが行う）
/// - 保存済みのモデルが一覧に無い: 「このモデルは使えません: X」を赤字で出す（値は消さない）
///
/// 「モデルを指定しない（組織の既定に従う）」の行は出さない。省略したときの意味（D-5）が実装に入るまでは、
/// モデルカタログを設定した環境では保存時の検証（V3）がモデルを必須にしているため。

import * as React from "react";
import { ChevronDown } from "lucide-react";

import { Input } from "@/components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Skeleton } from "@/components/ui/skeleton";
import { ModelList } from "@/components/llm/model-list";
import { useMediaQuery } from "@/hooks/use-media-query";
import { useLlmModelsState } from "@/lib/model-choice";
import { cn } from "@/lib/utils";
import { Field } from "./common";

/// 設定パネルがシートになる幅（config-panel/index.tsx と同じ）。
const PANEL_QUERY = "(min-width: 768px)";

export function ModelField({
  value,
  onChange,
  error,
}: {
  value: string | undefined;
  onChange: (next: string | undefined) => void;
  /// サーバの検証の誤り（`/params/model`）。
  error?: string | null;
}) {
  const state = useLlmModelsState();
  const desktop = useMediaQuery(PANEL_QUERY);
  const [open, setOpen] = React.useState(false);
  const listRef = React.useRef<HTMLDivElement | null>(null);

  if (state.status === "loading") {
    return (
      <Field label="モデル">
        <div aria-busy="true" aria-label="モデルの一覧を読み込み中">
          <Skeleton className="h-8 w-full" />
        </div>
      </Field>
    );
  }

  if (state.status === "error" || state.data.models.length === 0) {
    return (
      <Field
        label="モデル"
        error={error}
        hint={
          state.status === "error"
            ? "モデルの一覧を読み込めませんでした。管理者が登録したモデル ID をそのまま書いてください"
            : "使えるモデルがありません。管理者に AI の設定を確かめてもらってください"
        }
      >
        <Input
          value={value ?? ""}
          onChange={(e) => onChange(e.target.value || undefined)}
          placeholder="モデル ID"
          aria-label="モデル ID"
          aria-invalid={error ? true : undefined}
          className="h-8 font-mono text-xs max-md:text-base"
        />
      </Field>
    );
  }

  const data = state.data;
  const current = value ? data.models.find((m) => m.id === value) : undefined;
  // サーバの誤りがあればそちらを出す（使えるモデルの一覧も添えてある）。無ければ手元の一覧で先に知らせる。
  const message = error ?? (value && !current ? `このモデルは使えません: ${value}` : null);
  const pick = (id: string | null) => {
    if (id) onChange(id);
    setOpen(false);
  };

  const button = (
    <button
      type="button"
      aria-label={`モデル: ${current?.label ?? value ?? "未選択"}（変更する）`}
      aria-expanded={open}
      data-testid="workflow-model-button"
      onClick={desktop ? undefined : () => setOpen((o) => !o)}
      className={cn(
        "flex h-8 w-full items-center gap-2 rounded-md border border-input bg-background px-3 text-left text-sm shadow-xs pointer-coarse:h-11",
        "transition-colors hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
        message && "border-destructive",
      )}
    >
      <span className={cn("min-w-0 flex-1 truncate", !current && !value && "text-muted-foreground")}>
        {current?.label ?? value ?? "モデルを選ぶ"}
      </span>
      {current ? <span className="shrink-0 text-[11px] text-muted-foreground">{current.provider_label}</span> : null}
      <ChevronDown className={cn("size-3.5 shrink-0 text-muted-foreground transition-transform", open && "rotate-180")} aria-hidden />
    </button>
  );

  return (
    <Field
      label="モデル"
      error={message}
      hint={current?.description || "管理者が使えるようにしたモデルから選びます"}
    >
      {desktop ? (
        <Popover open={open} onOpenChange={setOpen}>
          <PopoverTrigger asChild>{button}</PopoverTrigger>
          <PopoverContent
            align="end"
            collisionPadding={12}
            className="flex max-h-[min(var(--radix-popover-content-available-height),24rem)] w-80 flex-col p-1.5"
            data-testid="workflow-model-picker"
          >
            <ModelList data={data} current={current?.id ?? null} onPick={pick} surface="popover" listRef={listRef} />
          </PopoverContent>
        </Popover>
      ) : (
        <>
          {button}
          {open ? (
            <InlineList onClose={() => setOpen(false)}>
              <ModelList data={data} current={current?.id ?? null} onPick={pick} surface="sheet" listRef={listRef} />
            </InlineList>
          ) : null}
        </>
      )}
    </Field>
  );
}

/// シートの中に広げる一覧の枠。開いたら一覧（または検索欄）へフォーカスを移す。
function InlineList({ children, onClose }: { children: React.ReactNode; onClose: () => void }) {
  const ref = React.useRef<HTMLDivElement | null>(null);
  React.useEffect(() => {
    // ソフトキーボードを出さないよう、検索欄ではなく一覧へ移す（チャットのシートと同じ）。
    ref.current?.querySelector<HTMLElement>('[role="listbox"]')?.focus({ preventScroll: true });
    ref.current?.scrollIntoView({ block: "nearest" });
  }, []);
  return (
    <div
      ref={ref}
      className="flex max-h-72 flex-col rounded-md border bg-background p-1"
      data-testid="workflow-model-picker"
      // 一覧の中の Esc は一覧だけを閉じる（シートごと閉じない・local-escape.ts）。
      data-local-escape="true"
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.stopPropagation();
          onClose();
        }
      }}
    >
      {children}
    </div>
  );
}
