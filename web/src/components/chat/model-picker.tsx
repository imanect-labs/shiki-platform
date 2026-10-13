"use client";

/// チャットのモデル選択（#553）。
///
/// - 既定は Auto（組織の設定で決まるモデル）で、入力欄には何も出さない。
/// - 「＋」メニューの「モデル」からモデルを選ぶと、送信ボタンの左にピルが出る。
/// - ピルを開くとモデルの切替と思考の深さ（3 段のスライダー）を変えられ、Auto に戻すとピルが消える。
/// 選べるのは組織の管理者が公開したモデルだけ（サーバも投稿時に検証する）。

import * as React from "react";
import { Check, ChevronDown, Globe2, Sparkles } from "lucide-react";

import { cn } from "@/lib/utils";
import type { EffortLevel, LlmModelOption, LlmModelsResponse } from "@/lib/admin-ai-api";
import { useLlmModels, useModelChoice, type ModelChoice } from "@/lib/model-choice";
import {
  DropdownMenuItem,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
} from "@/components/ui/dropdown-menu";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";

const EFFORTS: { value: EffortLevel; label: string; hint: string }[] = [
  { value: "low", label: "速い", hint: "短く素早く答えます" },
  { value: "medium", label: "標準", hint: "ふだんの業務に" },
  { value: "high", label: "じっくり", hint: "時間をかけて考えます" },
];

const effortLabel = (e: EffortLevel) => EFFORTS.find((x) => x.value === e)?.label ?? e;

/// 国外処理バッジ（NFR-10）。
function AbroadBadge() {
  return (
    <span className="inline-flex items-center gap-0.5 rounded bg-[oklch(0.94_0.06_80)] px-1 py-px text-[10px] font-medium text-[oklch(0.4_0.1_70)] dark:bg-[oklch(0.32_0.06_80)] dark:text-[oklch(0.87_0.09_85)]">
      <Globe2 className="size-2.5" aria-hidden />
      国外処理
    </span>
  );
}

/// 選択肢の行（モデル名・バッジ・説明）。
function ModelRow({ m }: { m: LlmModelOption }) {
  return (
    <span className="flex min-w-0 flex-1 flex-col gap-0.5">
      <span className="flex flex-wrap items-center gap-1.5 text-[13px] font-medium">
        {m.label}
        {m.processed_abroad ? <AbroadBadge /> : null}
      </span>
      <span className="truncate text-[12px] text-muted-foreground">
        {m.description ?? m.provider_label}
      </span>
    </span>
  );
}

/// Auto の説明（何を使うか）。
function autoHint(data: LlmModelsResponse): string {
  if (!data.auto) return "組織の設定に合わせて選びます";
  return `${data.auto.label}（${effortLabel(data.auto.effort)}）を使います`;
}

/// 選んだモデルが公開から外れていたら Auto に戻す（送信時の 400 を避ける）。
function useValidChoice(scope: string, data: LlmModelsResponse | null) {
  const [choice, setChoice] = useModelChoice(scope);
  const model = choice ? data?.models.find((m) => m.id === choice.model) : undefined;
  React.useEffect(() => {
    if (data && choice && (!data.allow_user_choice || !model)) setChoice(null);
  }, [data, choice, model, setChoice]);
  return { choice, setChoice, model };
}

/// 送信ボタンの左に出すピル（モデルを選んだときだけ）。
export function ModelPill({ scope }: { scope: string }) {
  const data = useLlmModels();
  const { choice, setChoice, model } = useValidChoice(scope, data);
  const [open, setOpen] = React.useState(false);
  if (!data || !choice || !model) return null;

  const pick = (m: LlmModelOption) => setChoice({ model: m.id, effort: choice.effort });
  const effortIndex = EFFORTS.findIndex((e) => e.value === choice.effort);
  const summary = model.supports_effort
    ? `${model.label}・${effortLabel(choice.effort)}`
    : model.label;

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button
          type="button"
          aria-label={`モデル: ${summary}（変更する）`}
          title="モデルと思考の深さ"
          data-testid="model-pill"
          className={cn(
            "inline-flex h-9 min-w-0 max-w-[300px] items-center gap-1.5 rounded-full border px-3 text-[13px] font-medium transition-colors",
            "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-card",
            open
              ? "border-foreground/30 bg-secondary text-foreground"
              : "border-border text-foreground/75 hover:bg-secondary hover:text-foreground",
          )}
        >
          <Sparkles className="size-[15px] shrink-0 text-primary" aria-hidden />
          {/* 狭い幅ではアイコンだけにして、隣のボタンを折り返させない。 */}
          <span className="hidden truncate sm:inline">{model.label}</span>
          {model.supports_effort ? (
            <span className="hidden shrink-0 text-muted-foreground sm:inline">
              ・{effortLabel(choice.effort)}
            </span>
          ) : null}
          <ChevronDown className="size-3.5 shrink-0 opacity-60" aria-hidden />
        </button>
      </PopoverTrigger>
      <PopoverContent side="top" align="end" sideOffset={8} className="w-80 p-1.5">
        <div className="px-2.5 pb-1 pt-1.5 text-xs font-medium uppercase tracking-wide text-muted-foreground">
          モデル
        </div>
        <div role="radiogroup" aria-label="モデル" className="flex flex-col">
          <button
            type="button"
            role="radio"
            aria-checked={false}
            onClick={() => {
              setChoice(null);
              setOpen(false);
            }}
            className="flex items-start gap-2.5 rounded-md px-2.5 py-2 text-left outline-none hover:bg-accent focus-visible:bg-accent"
          >
            <span className="flex min-w-0 flex-1 flex-col gap-0.5">
              <span className="text-[13px] font-medium">Auto（おまかせ）</span>
              <span className="truncate text-[12px] text-muted-foreground">{autoHint(data)}</span>
            </span>
          </button>
          {data.models.map((m) => {
            const current = m.id === model.id;
            return (
              <button
                key={m.id}
                type="button"
                role="radio"
                aria-checked={current}
                onClick={() => pick(m)}
                data-testid={`model-option-${m.id}`}
                className={cn(
                  "flex items-start gap-2.5 rounded-md px-2.5 py-2 text-left outline-none hover:bg-accent focus-visible:bg-accent",
                  current && "bg-accent/60",
                )}
              >
                <ModelRow m={m} />
                {current ? <Check className="mt-0.5 size-4 shrink-0 text-primary" aria-hidden /> : null}
              </button>
            );
          })}
        </div>

        {model.supports_effort ? (
          <div className="mt-1 border-t px-2.5 pb-2 pt-3">
            <div className="flex items-baseline justify-between">
              <label htmlFor="effort-slider" className="text-[13px] font-medium">
                思考の深さ
              </label>
              <span className="text-[12px] text-muted-foreground">
                {EFFORTS[effortIndex]?.hint}
              </span>
            </div>
            <input
              id="effort-slider"
              type="range"
              min={0}
              max={2}
              step={1}
              value={effortIndex < 0 ? 1 : effortIndex}
              aria-valuetext={effortLabel(choice.effort)}
              onChange={(e) =>
                setChoice({ model: model.id, effort: EFFORTS[Number(e.target.value)].value })
              }
              data-testid="effort-slider"
              className="mt-2.5 w-full cursor-pointer accent-[var(--primary)]"
            />
            <div className="mt-1 flex justify-between text-[11.5px] text-muted-foreground" aria-hidden>
              {EFFORTS.map((e) => (
                <span key={e.value} className={cn(e.value === choice.effort && "font-medium text-foreground")}>
                  {e.label}
                </span>
              ))}
            </div>
          </div>
        ) : null}
        <p className="border-t px-2.5 py-2 text-[11.5px] text-muted-foreground">
          組織の管理者が公開したモデルだけが表示されます。
        </p>
      </PopoverContent>
    </Popover>
  );
}

/// 「＋」メニュー内の「モデル」サブメニュー（選ぶとピルが出る）。
export function ModelSubmenu({ scope }: { scope: string }) {
  const data = useLlmModels();
  const { choice, setChoice } = useValidChoice(scope, data);
  if (!data || !data.allow_user_choice || data.models.length === 0) return null;
  const select = (next: ModelChoice) => setChoice(next);
  return (
    <DropdownMenuSub>
      <DropdownMenuSubTrigger className="gap-2.5 px-2.5 py-2" data-testid="composer-model-menu">
        <Sparkles className="text-muted-foreground" />
        <span className="flex-1">モデル</span>
        <span className="max-w-[110px] truncate text-[12px] text-muted-foreground">
          {choice ? (data.models.find((m) => m.id === choice.model)?.label ?? "Auto") : "Auto"}
        </span>
      </DropdownMenuSubTrigger>
      <DropdownMenuSubContent className="w-72 p-1.5">
        <DropdownMenuItem className="items-start gap-2.5 px-2.5 py-2" onSelect={() => select(null)}>
          <span className="flex min-w-0 flex-1 flex-col gap-0.5">
            <span className="text-[13px] font-medium">Auto（おまかせ）</span>
            <span className="truncate text-[12px] text-muted-foreground">{autoHint(data)}</span>
          </span>
          {!choice ? <Check className="mt-0.5 size-4 text-primary" aria-hidden /> : null}
        </DropdownMenuItem>
        {data.models.map((m) => (
          <DropdownMenuItem
            key={m.id}
            className="items-start gap-2.5 px-2.5 py-2"
            onSelect={() =>
              select({ model: m.id, effort: choice?.effort ?? data.default_effort })
            }
            data-testid={`composer-model-${m.id}`}
          >
            <ModelRow m={m} />
            {choice?.model === m.id ? <Check className="mt-0.5 size-4 text-primary" aria-hidden /> : null}
          </DropdownMenuItem>
        ))}
      </DropdownMenuSubContent>
    </DropdownMenuSub>
  );
}
