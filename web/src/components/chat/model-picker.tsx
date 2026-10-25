"use client";

/// チャットのモデル選択（#553・#583）。
///
/// - 送信ボタンの左に「モデル名 ＋ 思考の深さ ⌄」を出す（既定は「Auto」・#768）。押すと一覧を開き、
///   モデルの切替と思考の深さ（3 段のスライダー）を変えられる。Auto に戻すのも一覧から。
///   スマホで既定のまま（モデルを選んでいない）ときは入力欄に出さず、「＋」のシートから開く。
/// - 一覧はプロバイダごとの見出しで分け、1 モデル 1 行（名前・国外処理・選択中の印を列で揃える）。
///   モデルが多いときは検索欄を出し、高さは画面に残っている分で抑えてスクロールさせる。
///   一覧そのもの（`ModelList`）は components/llm/model-list.tsx にあり、ワークフローのモデル欄と共用する。
/// - 狭い画面では横に開くポップオーバーが画面からはみ出すので、下からのシートで出す。
/// 選べるのは組織の管理者が公開したモデルだけ（サーバも投稿時に検証する）。

import * as React from "react";
import { ChevronDown } from "lucide-react";

import { cn } from "@/lib/utils";
import { useMediaQuery } from "@/hooks/use-media-query";
import type { EffortLevel, LlmModelOption, LlmModelsResponse } from "@/lib/admin-ai-api";
import { useLlmModels, useModelChoice, type ModelChoice } from "@/lib/model-choice";
import { ModelList, SEARCH_MIN_MODELS, type ModelListDefaultRow } from "@/components/llm/model-list";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Sheet, SheetContent, SheetTitle, SheetTrigger } from "@/components/ui/sheet";

const EFFORTS: { value: EffortLevel; label: string; hint: string }[] = [
  { value: "low", label: "速い", hint: "短く素早く答えます" },
  { value: "medium", label: "標準", hint: "ふだんの業務に" },
  { value: "high", label: "じっくり", hint: "時間をかけて考えます" },
];

/// ポップオーバーを出す幅（これ未満は下からのシート）。
const DESKTOP_QUERY = "(min-width: 640px)";

const effortLabel = (e: EffortLevel) => EFFORTS.find((x) => x.value === e)?.label ?? e;

/// 「選ばない」選択肢の表示。モデルを指定しない投稿は組織の既定で動くので、既定が Auto なら
/// Auto（振り先を添える）、管理者が特定のモデルを既定にしていればそのモデルとして見せる。
/// 既定の行は「auto」「おまかせ」「既定」でも検索で引ける。
function defaultOption(data: LlmModelsResponse): ModelListDefaultRow {
  if (data.default_model !== "auto") {
    const m = data.models.find((x) => x.id === data.default_model);
    return { label: "組織の既定", target: m?.label ?? null, keywords: "auto 既定" };
  }
  return {
    label: "Auto（おまかせ）",
    target: data.auto ? `${data.auto.label}・${effortLabel(data.auto.effort)}` : null,
    keywords: "auto 既定",
  };
}

/// 組織の既定が特定のモデルならそのモデル（Auto なら null）。
function defaultModelOf(data: LlmModelsResponse): LlmModelOption | null {
  if (data.default_model === "auto") return null;
  return data.models.find((m) => m.id === data.default_model) ?? null;
}

/// 選んだモデルが公開から外れていたら Auto に戻す（送信時の 400 を避ける）。
/// 既定のまま思考の深さだけ変えた選択（`model: null`）は、既定が思考の深さを選べるモデルで
/// なくなったら（Auto に変わった等）外す。
function useValidChoice(scope: string, data: LlmModelsResponse | null) {
  const [choice, setChoice] = useModelChoice(scope);
  const model = choice?.model ? data?.models.find((m) => m.id === choice.model) : undefined;
  const valid =
    !data ||
    !choice ||
    (data.allow_user_choice &&
      (choice.model === null ? Boolean(defaultModelOf(data)?.supports_effort) : Boolean(model)));
  React.useEffect(() => {
    if (!valid) setChoice(null);
  }, [valid, setChoice]);
  return { choice: valid ? choice : null, setChoice, model };
}

/// 一覧の入れ物。広い画面はポップオーバー、狭い画面は下からのシート。
///
/// `trigger` は押すと開く要素（モデルのボタン）。`sheetOnly` のとき（ボタンを隠して「＋」のシート
/// から開く）は、隠れたボタンに寄せないよう常に下からのシートにする。
function PickerSurface({
  open,
  onOpenChange,
  trigger,
  sheetOnly = false,
  align,
  tall,
  children,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  trigger: React.ReactElement;
  sheetOnly?: boolean;
  align: "start" | "end";
  /// 検索欄を出すほど多いか。狭い画面のシートは、絞り込みで高さが跳ねないよう高さを固定する。
  tall: boolean;
  children: (
    surface: "popover" | "sheet",
    listRef: React.RefObject<HTMLDivElement | null>,
  ) => React.ReactNode;
}) {
  const desktop = useMediaQuery(DESKTOP_QUERY);
  const listRef = React.useRef<HTMLDivElement | null>(null);
  // 中身はポータルで描画されるが、React のイベントは React ツリーをたどって入力欄まで届く。
  // 入力欄はクリックで textarea にフォーカスするので、そのままだと一覧の外へのフォーカスと
  // 見なされて閉じる。一覧の中のクリックはここで止める。
  const stop = (e: React.MouseEvent) => e.stopPropagation();

  if (desktop && !sheetOnly) {
    return (
      <Popover open={open} onOpenChange={onOpenChange}>
        <PopoverTrigger asChild>{trigger}</PopoverTrigger>
        <PopoverContent
          side="top"
          align={align}
          sideOffset={8}
          collisionPadding={12}
          className="flex max-h-[min(var(--radix-popover-content-available-height),32rem)] w-80 flex-col p-1.5"
          data-testid="model-picker"
          onClick={stop}
        >
          {children("popover", listRef)}
        </PopoverContent>
      </Popover>
    );
  }
  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetTrigger asChild>{trigger}</SheetTrigger>
      <SheetContent
        side="bottom"
        // 開いた瞬間に検索欄へフォーカスするとソフトキーボードが一覧を隠すので、一覧へ移す。
        onOpenAutoFocus={(e) => {
          e.preventDefault();
          listRef.current?.focus({ preventScroll: true });
        }}
        // 少ないときは中身の高さ。多い（検索欄を出す）ときは 85% に固定し、絞り込んでも跳ねない。
        className={cn(
          "max-h-[85dvh] rounded-t-xl px-3 pb-[max(0.75rem,env(safe-area-inset-bottom))] pt-4",
          tall && "h-[85dvh]",
        )}
        data-testid="model-picker"
        onClick={stop}
      >
        <SheetTitle className="mb-3 px-1.5 pr-10">モデル</SheetTitle>
        {children("sheet", listRef)}
      </SheetContent>
    </Sheet>
  );
}

/// 思考の深さ（3 段のスライダー）。
function EffortSlider({
  effort,
  onChange,
}: {
  effort: EffortLevel;
  onChange: (e: EffortLevel) => void;
}) {
  const index = EFFORTS.findIndex((e) => e.value === effort);
  return (
    <div className="mt-1 shrink-0 border-t px-2.5 pb-1 pt-3">
      <div className="flex items-baseline justify-between">
        <label htmlFor="effort-slider" className="text-[13px] font-medium">
          思考の深さ
        </label>
        <span className="text-[12px] text-muted-foreground">{EFFORTS[index]?.hint}</span>
      </div>
      <input
        id="effort-slider"
        type="range"
        min={0}
        max={2}
        step={1}
        value={index < 0 ? 1 : index}
        aria-valuetext={effortLabel(effort)}
        onChange={(e) => onChange(EFFORTS[Number(e.target.value)].value)}
        data-testid="effort-slider"
        className="mt-2.5 w-full cursor-pointer accent-[var(--primary)]"
      />
      <div className="mt-1 flex justify-between text-[11.5px] text-muted-foreground" aria-hidden>
        {EFFORTS.map((e) => (
          <span key={e.value} className={cn(e.value === effort && "font-medium text-foreground")}>
            {e.label}
          </span>
        ))}
      </div>
    </div>
  );
}

/// 送信ボタンの左に常に出すモデルの選択（ChatGPT の入力欄と同じ置き場所）。
///
/// 今のモデルの表示名（と思考の深さ）。選んでいないときは、既定が Auto なら「Auto」、
/// 特定のモデルならその名前。
///
/// 思考の深さは実際に動くモデルで決める。既定が特定のモデルなら、選ばなくても思考の深さを
/// 出す・変えられる（Auto は振り先が思考の深さも決めるので出さない）。
function summarize(data: LlmModelsResponse, model: LlmModelOption | undefined, choice: ModelChoice) {
  const fallback = defaultModelOf(data);
  const name = model?.label ?? fallback?.label ?? "Auto";
  const effective = model ?? fallback;
  const currentEffort = choice?.effort ?? data.default_effort;
  const effort = effective?.supports_effort ? effortLabel(currentEffort) : null;
  return {
    name,
    effort,
    summary: effort ? `${name}・${effort}` : name,
    fallback,
    effective,
    currentEffort,
  };
}

/// 今のモデルの表示（「＋」のシートの「モデル」の行）。利用者が選べない組織では null。
export function useModelSummary(scope: string): string | null {
  const data = useLlmModels();
  const { choice, model } = useValidChoice(scope, data);
  if (!data || !data.allow_user_choice || data.models.length === 0) return null;
  return summarize(data, model, choice).summary;
}

/// 入力欄の送信の左に置くモデルのボタン。
/// 「モデル名 ＋ 思考の深さ（薄い字）⌄」。何も選んでいなければ組織の既定（ふつうは「Auto」）。
/// 押すとモデルの一覧と思考の深さのスライダーを開く。Auto に戻すのも一覧から。
/// ユーザーに選ばせない組織・モデルが無いときは出さない。
///
/// スマホで既定のままのときは、入力欄を 1 行に保つため呼び出し側がボタンを CSS で隠し、
/// 「＋」のシートの「モデル」から開く（`open` を渡す）。そのときは `sheetOnly` で常にシートにする。
export function ModelButton({
  scope,
  className,
  sheetOnly = false,
  open: openProp,
  onOpenChange,
}: {
  scope: string;
  className?: string;
  sheetOnly?: boolean;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
}) {
  const data = useLlmModels();
  const { choice, setChoice, model } = useValidChoice(scope, data);
  const [openState, setOpenState] = React.useState(false);
  const open = openProp ?? openState;
  const setOpen = onOpenChange ?? setOpenState;
  if (!data || !data.allow_user_choice || data.models.length === 0) return null;

  const { name, effort, summary, fallback, effective, currentEffort } = summarize(
    data,
    model,
    choice,
  );
  // 既定のモデルのまま思考の深さだけ変える。既定の深さに戻したら選択ごと外す（Auto と同じ扱い）。
  const setDefaultEffort = (e: EffortLevel) =>
    setChoice(e === data.default_effort ? null : { model: null, effort: e });

  return (
    <PickerSurface
      open={open}
      onOpenChange={setOpen}
      align="end"
      tall={data.models.length >= SEARCH_MIN_MODELS}
      sheetOnly={sheetOnly}
      trigger={
        <button
          type="button"
          aria-label={`モデル: ${summary}（変更する）`}
          title="モデルと思考の深さ"
          data-testid="model-button"
          className={cn(
            "inline-flex h-9 min-w-0 shrink items-center gap-1 rounded-full px-2.5 text-[14px] transition-colors md:h-8 md:text-[13px]",
            "text-foreground/85 hover:bg-secondary hover:text-foreground",
            "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
            open && "bg-secondary text-foreground",
            className,
          )}
        >
          <span className="min-w-0 truncate font-medium">{name}</span>
          {effort ? <span className="shrink-0 text-muted-foreground">{effort}</span> : null}
          <ChevronDown className="size-3.5 shrink-0 text-muted-foreground" aria-hidden />
        </button>
      }
    >
      {(surface, listRef) => (
        <>
          <ModelList
            data={data}
            current={model?.id ?? null}
            surface={surface}
            listRef={listRef}
            defaultRow={defaultOption(data)}
            onPick={(id) => {
              if (id === null) {
                // 既定が思考の深さを選べるモデルなら、選んでいた深さを引き継いで開いたままにする。
                if (fallback?.supports_effort) {
                  setDefaultEffort(currentEffort);
                  return;
                }
                setChoice(null);
                setOpen(false);
                return;
              }
              const picked = data.models.find((m) => m.id === id);
              setChoice({ model: id, effort: currentEffort });
              // 思考の深さを選べるモデルなら、続けて変えられるよう開いたままにする。
              if (!picked?.supports_effort) setOpen(false);
            }}
          />
          {effective?.supports_effort ? (
            <EffortSlider
              effort={currentEffort}
              onChange={(e) => (model ? setChoice({ model: model.id, effort: e }) : setDefaultEffort(e))}
            />
          ) : null}
        </>
      )}
    </PickerSurface>
  );
}
