"use client";

/// チャットのモデル選択（#553・#583）。
///
/// - 既定（組織の設定。ふつうは Auto）では入力欄には何も出さない。
/// - 「＋」メニューの「モデル」から選ぶと、送信ボタンの左にピルが出る。ピルからも同じ一覧を開き、
///   モデルの切替と思考の深さ（3 段のスライダー）を変えられる。Auto に戻すとピルが消える。
/// - 一覧はプロバイダごとの見出しで分け、1 モデル 1 行（名前・国外処理・選択中の印を列で揃える）。
///   モデルが多いときは検索欄を出し、高さは画面に残っている分で抑えてスクロールさせる。
/// - 狭い画面では横に開くポップオーバーが画面からはみ出すので、下からのシートで出す。
/// 選べるのは組織の管理者が公開したモデルだけ（サーバも投稿時に検証する）。

import * as React from "react";
import { Check, ChevronDown, Search, Sparkles } from "lucide-react";

import { cn } from "@/lib/utils";
import { useMediaQuery } from "@/hooks/use-media-query";
import type { EffortLevel, LlmModelOption, LlmModelsResponse } from "@/lib/admin-ai-api";
import { useLlmModels, useModelChoice, type ModelChoice } from "@/lib/model-choice";
import { DropdownMenuItem } from "@/components/ui/dropdown-menu";
import { Popover, PopoverAnchor, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Sheet, SheetContent, SheetTitle, SheetTrigger } from "@/components/ui/sheet";
import { usePromptInput } from "@/components/prompt-kit/prompt-input";

const EFFORTS: { value: EffortLevel; label: string; hint: string }[] = [
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
function defaultOption(data: LlmModelsResponse): { label: string; target: string | null } {
  if (data.default_model !== "auto") {
    const m = data.models.find((x) => x.id === data.default_model);
    return { label: "組織の既定", target: m?.label ?? null };
  }
  return {
    label: "Auto（おまかせ）",
    target: data.auto ? `${data.auto.label}・${effortLabel(data.auto.effort)}` : null,
  };
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
        aria-activedescendant={showSearch ? undefined : activeOptionId}
        tabIndex={showSearch ? -1 : 0}
        onKeyDown={showSearch ? undefined : onKeyDown}
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
/// `trigger` は押すと開く要素（ピル）、`anchor` は位置合わせだけに使う要素（「＋」ボタン。
/// 開閉は呼び出し側が持つ）。どちらか一方を渡す。
function PickerSurface({
  open,
  onOpenChange,
  trigger,
  anchor,
  align,
  tall,
  children,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  trigger?: React.ReactElement;
  anchor?: React.ReactElement;
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
  // 閉じたら入力欄へフォーカスを戻す。「＋」から開いた一覧には戻り先のトリガーが無く、ピルは
  // Auto に戻すと消えるので、既定の戻し先（トリガー）では body に落ちる。選んだらそのまま打てる
  // 入力欄が自然な戻り先。
  const { textareaRef } = usePromptInput();
  const returnFocus = (e: Event) => {
    if (!textareaRef.current) return;
    e.preventDefault();
    textareaRef.current.focus({ preventScroll: true });
  };
  // 中身はポータルで描画されるが、React のイベントは React ツリーをたどって入力欄まで届く。
  // 入力欄はクリックで textarea にフォーカスするので、そのままだと一覧の外へのフォーカスと
  // 見なされて閉じる。一覧の中のクリックはここで止める。
  const stop = (e: React.MouseEvent) => e.stopPropagation();

  if (desktop) {
    return (
      <Popover open={open} onOpenChange={onOpenChange}>
        {trigger ? <PopoverTrigger asChild>{trigger}</PopoverTrigger> : null}
        {anchor ? <PopoverAnchor asChild>{anchor}</PopoverAnchor> : null}
        <PopoverContent
          side="top"
          align={align}
          sideOffset={8}
          collisionPadding={12}
          className="flex max-h-[min(var(--radix-popover-content-available-height),32rem)] w-80 flex-col p-1.5"
          data-testid="model-picker"
          onClick={stop}
          onCloseAutoFocus={returnFocus}
        >
          {children("popover", listRef)}
        </PopoverContent>
      </Popover>
    );
  }
  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      {trigger ? <SheetTrigger asChild>{trigger}</SheetTrigger> : null}
      {anchor ?? null}
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
        onCloseAutoFocus={returnFocus}
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

/// 送信ボタンの左に出すピル（モデルを選んだときだけ）。開くと一覧と思考の深さ。
export function ModelPill({ scope }: { scope: string }) {
  const data = useLlmModels();
  const { choice, setChoice, model } = useValidChoice(scope, data);
  const [open, setOpen] = React.useState(false);
  if (!data || !choice || !model) return null;

  const summary = model.supports_effort
    ? `${model.label}・${effortLabel(choice.effort)}`
    : model.label;

  return (
    <PickerSurface
      open={open}
      onOpenChange={setOpen}
      align="end"
      tall={data.models.length >= SEARCH_MIN_MODELS}
      trigger={
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
      }
    >
      {(surface, listRef) => (
        <>
          <ModelList
            data={data}
            current={model.id}
            surface={surface}
            listRef={listRef}
            onPick={(id) => {
              // Auto に戻すとピルごと消えるので閉じる。モデルの切替では思考の深さも触れるよう開いたまま。
              if (id === null) {
                setChoice(null);
                setOpen(false);
              } else {
                setChoice({ model: id, effort: choice.effort });
              }
            }}
          />
          {model.supports_effort ? (
            <EffortSlider
              effort={choice.effort}
              onChange={(effort) => setChoice({ model: model.id, effort })}
            />
          ) : null}
        </>
      )}
    </PickerSurface>
  );
}

/// 「＋」メニューの「モデル」項目。選ぶとメニューを閉じて一覧（{@link ModelPicker}）を開く。
export function ModelMenuItem({ scope, onOpen }: { scope: string; onOpen: () => void }) {
  const data = useLlmModels();
  const { choice } = useValidChoice(scope, data);
  if (!data || !data.allow_user_choice || data.models.length === 0) return null;
  return (
    <DropdownMenuItem className="gap-2.5 px-2.5 py-2" onSelect={onOpen} data-testid="composer-model-menu">
      <Sparkles className="text-muted-foreground" />
      <span className="flex-1">モデル</span>
      <span className="max-w-[110px] truncate text-[12px] text-muted-foreground">
        {(choice && data.models.find((m) => m.id === choice.model)?.label) ||
          (data.default_model === "auto" ? "Auto" : "既定")}
      </span>
    </DropdownMenuItem>
  );
}

/// 「＋」メニューから開くモデル一覧。`anchor`（「＋」ボタン）の上に出す。選ぶと閉じてピルが出る。
export function ModelPicker({
  scope,
  open,
  onOpenChange,
  anchor,
}: {
  scope: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  anchor: React.ReactElement;
}) {
  const data = useLlmModels();
  const { choice, setChoice } = useValidChoice(scope, data);
  // 選べない（読み込み前・選択不可・モデル 0 件）ときも同じ構造で描く。分岐で `anchor` だけを
  // 返すと、読み込みが終わった瞬間に「＋」ボタンが作り直されてフォーカスが外れる。
  const usable = !!data && data.allow_user_choice && data.models.length > 0;
  return (
    <PickerSurface
      open={usable && open}
      onOpenChange={onOpenChange}
      anchor={anchor}
      align="start"
      tall={!!data && data.models.length >= SEARCH_MIN_MODELS}
    >
      {(surface, listRef) =>
        usable ? (
          <ModelList
            data={data}
            current={choice?.model ?? null}
            onPick={(id) => {
              const next: ModelChoice = id
                ? { model: id, effort: choice?.effort ?? data.default_effort }
                : null;
              setChoice(next);
              onOpenChange(false);
            }}
            surface={surface}
            listRef={listRef}
          />
        ) : null
      }
    </PickerSurface>
  );
}
