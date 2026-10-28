"use client";

/// チャットのモデル選択（#553・#583）。
///
/// - 送信ボタンの左に「モデル名 ＋ 思考の深さ ⌄」を出す（既定は「Auto」・#768）。押すと一覧を開き、
///   モデルの切替と思考の深さ（3 段のスライダー）を変えられる。Auto に戻すのも一覧から。
///   スマホで既定のまま（モデルを選んでいない）ときは入力欄に出さず、「＋」のシートから開く。
/// - 一覧はプロバイダごとの見出しで分け、1 モデル 1 行（名前・国外処理・選択中の印を列で揃える）。
///   モデルが多いときは検索欄を出し、高さは画面に残っている分で抑えてスクロールさせる。
/// - 狭い画面では横に開くポップオーバーが画面からはみ出すので、下からのシートで出す。
/// 選べるのは組織の管理者が公開したモデルだけ（サーバも投稿時に検証する）。

import * as React from "react";
import { Check, ChevronDown, Search } from "lucide-react";

import { cn } from "@/lib/utils";
import { useMediaQuery } from "@/hooks/use-media-query";
import type { EffortLevel, LlmModelOption, LlmModelsResponse } from "@/lib/admin-ai-api";
import { useLlmModels, useModelChoice, type ModelChoice } from "@/lib/model-choice";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Sheet, SheetContent, SheetTitle, SheetTrigger } from "@/components/ui/sheet";

export const EFFORTS: { value: EffortLevel; label: string; hint: string }[] = [
  { value: "low", label: "速い", hint: "短く素早く答えます" },
  { value: "medium", label: "標準", hint: "ふだんの業務に" },
  { value: "high", label: "じっくり", hint: "時間をかけて考えます" },
];

/// これ以上のモデル数なら検索欄を出す（数件なら一覧を見渡せるので要らない）。
const SEARCH_MIN_MODELS = 8;

/// ポップオーバーを出す幅（これ未満は下からのシート）。
const DESKTOP_QUERY = "(min-width: 640px)";

const DEFAULT_KEY = "__default";

const effortLabel = (e: EffortLevel) => EFFORTS.find((x) => x.value === e)?.label ?? e;

/// 「選ばない」選択肢の表示。モデルを指定しない投稿は組織の既定で動くので、既定が Auto なら
/// Auto（振り先を添える）、管理者が特定のモデルを既定にしていればそのモデルとして見せる。
export function defaultOption(data: LlmModelsResponse): { label: string; target: string | null } {
  if (data.default_model !== "auto") {
    const m = data.models.find((x) => x.id === data.default_model);
    return { label: "組織の既定", target: m?.label ?? null };
  }
  return {
    label: "Auto（おまかせ）",
    target: data.auto ? `${data.auto.label}・${effortLabel(data.auto.effort)}` : null,
  };
}

/// 組織の既定が特定のモデルならそのモデル（Auto なら null）。
export function defaultModelOf(data: LlmModelsResponse): LlmModelOption | null {
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

/// 検索の正規化（全角半角・大小を寄せる）。
const norm = (s: string) => s.normalize("NFKC").toLowerCase();

/// 空白区切りの語がすべて `text` に含まれるか。
function matchesText(text: string, query: string): boolean {
  const hay = norm(text);
  return norm(query)
    .split(/\s+/)
    .filter(Boolean)
    .every((t) => hay.includes(t));
}

/// 名前・ID・プロバイダのどれかで引けるか。
const matches = (m: LlmModelOption, query: string) =>
  matchesText(`${m.label} ${m.id} ${m.provider_label}`, query);

type ProviderGroup = { provider: string; models: LlmModelOption[] };

/// プロバイダの見出しごとにまとめる（カタログの並び順を保つ）。
function groupByProvider(models: LlmModelOption[]): ProviderGroup[] {
  const groups: ProviderGroup[] = [];
  for (const m of models) {
    const g = groups.find((x) => x.provider === m.provider_label);
    if (g) g.models.push(m);
    else groups.push({ provider: m.provider_label, models: [m] });
  }
  return groups;
}

/// 国外処理バッジ（NFR-10）。プロバイダ単位で国外なら見出しに 1 つ、混在するときだけ行に出す。
function AbroadBadge() {
  return (
    <span className="shrink-0 rounded bg-[oklch(0.94_0.06_80)] px-1.5 py-px text-[10.5px] font-medium leading-4 text-[oklch(0.4_0.1_70)] dark:bg-[oklch(0.32_0.06_80)] dark:text-[oklch(0.87_0.09_85)]">
      国外処理
    </span>
  );
}

/// 1 行の右端の列（国外処理・選択中の印）。幅を固定して縦に揃える。
///
/// `badge` は行にバッジを出すか（プロバイダ全体が国外なら見出しに出すので行には出さない）。
/// 読み上げでは見出し（グループ名）が読まれないことが多いので、国外処理なら行にも必ず文で残す。
function TrailingColumns({
  abroad,
  badge,
  selected,
}: {
  abroad: boolean;
  badge: boolean;
  selected: boolean;
}) {
  return (
    <>
      {abroad && badge ? <AbroadBadge /> : null}
      {abroad && !badge ? <span className="sr-only">（国外処理）</span> : null}
      <span className="flex w-4 shrink-0 justify-center">
        {selected ? <Check className="size-4 text-primary" aria-hidden /> : null}
      </span>
    </>
  );
}

/// モデルの一覧（検索欄・プロバイダ見出し・キーボード操作）。ポップオーバーとシートで共用する。
/// 閉じると中身ごと外れるので、検索語と注目行は開くたびに初期化される。
function ModelList({
  data,
  current,
  onPick,
  surface,
  listRef,
}: {
  data: LlmModelsResponse;
  /// 選択中のモデル ID（`null` は既定＝Auto）。
  current: string | null;
  onPick: (model: string | null) => void;
  /// 見出しを貼り付けるときの背景（親の面と揃える）。
  surface: "popover" | "sheet";
  listRef: React.RefObject<HTMLDivElement | null>;
}) {
  const listId = React.useId();
  const [rawQuery, setQuery] = React.useState("");
  const query = rawQuery.trim();
  const showSearch = data.models.length >= SEARCH_MIN_MODELS;
  const filtered = query ? data.models.filter((m) => matches(m, query)) : data.models;
  const groups = groupByProvider(filtered);
  const fallback = defaultOption(data);
  // 既定（Auto）も検索で引ける（「auto」「おまかせ」「既定」）。
  const showDefault = !query || matchesText(`${fallback.label} auto 既定`, query);

  // キーボードで動かす順（画面の並びと同じ）。
  const keys = [
    ...(showDefault ? [DEFAULT_KEY] : []),
    ...groups.flatMap((g) => g.models.map((m) => m.id)),
  ];
  const currentKey = current ?? DEFAULT_KEY;
  const [active, setActive] = React.useState(() => Math.max(0, keys.indexOf(currentKey)));
  const activeKey = keys[Math.min(active, keys.length - 1)];
  // 行の id はモデル ID をそのまま使わない（空白や記号で aria の参照が壊れないよう、カタログ上の位置）。
  const optionId = (key: string) =>
    key === DEFAULT_KEY ? `${listId}-d` : `${listId}-m${data.models.findIndex((m) => m.id === key)}`;
  const activeOptionId = activeKey ? optionId(activeKey) : undefined;

  // 開いたときとキーボードで動かしたときだけ、注目中の行を見える位置へ（マウスで動かしたときに
  // 一覧がずれないように）。開いた直後は一覧の高さがまだ決まっていない（ポップオーバーの位置計算で
  // max-height が後から入る）ので、描画を待つ。
  // 印は effect の中では消さない（開発時の StrictMode で effect が 2 回走っても効くように）。
  // 合わせ終わったとき（タイマー）とマウスで動かしたときに消す。
  const scrollToActive = React.useRef(true);
  React.useEffect(() => {
    if (!activeOptionId || !scrollToActive.current) return;
    const reveal = () => document.getElementById(activeOptionId)?.scrollIntoView({ block: "nearest" });
    reveal();
    // 開いた直後は高さが決まるまで何度か大きさが変わるので、変わるたびに合わせ直す（短い間だけ）。
    const list = listRef.current;
    const ro = list && typeof ResizeObserver !== "undefined" ? new ResizeObserver(reveal) : null;
    if (list) ro?.observe(list);
    const done = window.setTimeout(() => {
      ro?.disconnect();
      scrollToActive.current = false;
    }, 500);
    return () => {
      window.clearTimeout(done);
      ro?.disconnect();
    };
  }, [activeOptionId, listRef]);

  const pickKey = (key: string) => onPick(key === DEFAULT_KEY ? null : key);

  const moveTo = (i: number) => {
    scrollToActive.current = true;
    setActive(i);
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    // 日本語入力の変換中の Enter・矢印は IME のもの（確定・候補選択）。奪わない。
    if (e.nativeEvent.isComposing || keys.length === 0) return;
    const at = Math.min(active, keys.length - 1);
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const step = e.key === "ArrowDown" ? 1 : -1;
      moveTo((at + step + keys.length) % keys.length);
    } else if (e.key === "Home" && !showSearch) {
      e.preventDefault();
      moveTo(0);
    } else if (e.key === "End" && !showSearch) {
      e.preventDefault();
      moveTo(keys.length - 1);
    } else if (e.key === "Enter") {
      e.preventDefault();
      pickKey(keys[at]);
    }
  };

  const row = (
    key: string,
    label: React.ReactNode,
    abroad: { abroad: boolean; badge: boolean },
    testId: string,
  ) => {
    const selected = key === currentKey;
    return (
      <div
        key={key}
        id={optionId(key)}
        role="option"
        aria-selected={selected}
        data-active={key === activeKey}
        data-testid={testId}
        onMouseMove={() => {
          scrollToActive.current = false;
          setActive(keys.indexOf(key));
        }}
        onClick={() => pickKey(key)}
        className={cn(
          // 指で押す狭い画面は 44px、ポインタの広い画面は詰める。scroll-mt は上に貼り付く
          // プロバイダ見出しの高さ分（キーボードで上へ動いたとき行が見出しの下に隠れない）。
          "flex h-11 scroll-mt-10 cursor-pointer select-none items-center gap-2 rounded-md px-2.5 text-sm sm:h-9 sm:text-[13px]",
          "data-[active=true]:bg-accent",
          selected && "font-medium",
        )}
      >
        {label}
        <TrailingColumns abroad={abroad.abroad} badge={abroad.badge} selected={selected} />
      </div>
    );
  };


  return (
    <>
      {showSearch ? (
        <div className="relative shrink-0 px-1 pb-1.5">
          <Search
            className="pointer-events-none absolute left-3.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground"
            aria-hidden
          />
          <input
            type="text"
            role="combobox"
            aria-expanded
            aria-controls={listId}
            aria-activedescendant={activeOptionId}
            aria-autocomplete="list"
            aria-label="モデルを検索"
            placeholder="モデルを検索"
            value={rawQuery}
            onChange={(e) => {
              setQuery(e.target.value);
              moveTo(0);
            }}
            onKeyDown={onKeyDown}
            data-testid="model-search"
            className={cn(
              // iOS は 16px 未満の入力欄にフォーカスすると画面を拡大するので、狭い画面は 16px。
              "h-10 w-full rounded-md border bg-transparent pl-8 pr-2.5 text-base outline-none sm:h-9 sm:text-[13px]",
              "placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring",
            )}
          />
        </div>
      ) : null}
      {/* 絞り込みの結果を読み上げる。 */}
      <span className="sr-only" aria-live="polite">
        {query ? `${keys.length} 件` : ""}
      </span>
      <div
        ref={listRef}
        id={listId}
        role="listbox"
        aria-label="モデル"
        // 検索欄があっても、狭い画面では開いた直後のフォーカスはここ（ソフトキーボードを出さない）
        // なので、一覧自体もキー操作を受ける。検索欄は兄弟なのでキーが二重に処理されることはない。
        aria-activedescendant={activeOptionId}
        tabIndex={showSearch ? -1 : 0}
        onKeyDown={onKeyDown}
        className="min-h-0 flex-1 overflow-y-auto overscroll-contain outline-none"
      >
        {!showDefault
          ? null
          : row(
              DEFAULT_KEY,
              <span className="flex min-w-0 flex-1 items-baseline gap-2">
                <span className="shrink-0">{fallback.label}</span>
                {fallback.target ? (
                  <span className="truncate text-[12px] text-muted-foreground">{fallback.target}</span>
                ) : null}
              </span>,
              { abroad: false, badge: false },
              "model-option-default",
            )}
        {groups.map((g) => {
          // 国外処理はふつうプロバイダ単位で決まるので、全部そうなら見出しに 1 つだけ出す。
          const allAbroad = g.models.every((m) => m.processed_abroad);
          return (
            <div
              key={g.provider}
              role="group"
              aria-label={allAbroad ? `${g.provider}（国外処理）` : g.provider}
            >
              {/* 見出しはスクロールしても上に残す（いまどのプロバイダの中かが分かるように）。
                  右端の余白は行の「選択中の印」の列に揃える。 */}
              <div
                className={cn(
                  "sticky top-0 z-10 flex items-center gap-2 pb-1 pl-2.5 pr-[34px] pt-2.5 text-[11.5px] font-medium text-muted-foreground",
                  surface === "popover" ? "bg-popover" : "bg-background",
                )}
                aria-hidden
              >
                <span className="min-w-0 flex-1 truncate">{g.provider}</span>
                {allAbroad ? <AbroadBadge /> : null}
              </div>
              {g.models.map((m) =>
                row(
                  m.id,
                  <span className="min-w-0 flex-1 truncate" data-label>
                    {m.label}
                  </span>,
                  { abroad: m.processed_abroad, badge: !allAbroad },
                  `model-option-${m.id}`,
                ),
              )}
            </div>
          );
        })}
        {query && keys.length === 0 ? (
          <p className="px-2.5 py-6 text-center text-[13px] text-muted-foreground">
            「{query}」に一致するモデルはありません
          </p>
        ) : null}
      </div>
    </>
  );
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
