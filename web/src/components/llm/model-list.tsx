"use client";

/// 選べるモデルの一覧（チャットのモデル選択・ワークフローの「AI に聞く」のモデル欄で共用する）。
///
/// - 一覧はプロバイダごとの見出しで分け、1 モデル 1 行（名前・国外処理・選択中の印を列で揃える）。
/// - モデルが多いときは検索欄を出す。矢印キー／Enter／Home／End で選べ、日本語入力の変換中は奪わない。
/// - 先頭の「既定」の行（モデルを指定しない選択）は呼び出し側が文言ごと渡す（渡さなければ出さない）。
/// - 組織がモデルの選択を禁じているときは、モデルの行を選べない行として見せる（`modelsDisabled`・
///   キーボードでも止まらない）。選べるのは既定の行だけになる。
/// 一覧は `GET /llm/models`（組織の管理者が公開したモデル）をそのまま使う。

import * as React from "react";
import { Check, Search } from "lucide-react";

import { cn } from "@/lib/utils";
import type { LlmModelOption, LlmModelsResponse } from "@/lib/admin-ai-api";

/// これ以上のモデル数なら検索欄を出す（数件なら一覧を見渡せるので要らない）。
export const SEARCH_MIN_MODELS = 8;

const DEFAULT_KEY = "__default";

/// 先頭に出す「既定」の行（`null` を選ぶ行）。
export type ModelListDefaultRow = {
  label: string;
  /// 既定で実際に動くモデルなど（薄い字で添える）。
  target: string | null;
  /// 検索で引ける語（表示名に加えて）。
  keywords?: string;
};

/// 検索の正規化（全角半角・大小を寄せる）。
const norm = (s: string) => s.normalize("NFKC").toLowerCase();

/// 空白区切りの語がすべて `text` に含まれるか。
export function matchesText(text: string, query: string): boolean {
  const hay = norm(text);
  return norm(query)
    .split(/\s+/)
    .filter(Boolean)
    .every((t) => hay.includes(t));
}

/// 名前・ID・プロバイダのどれかで引けるか。
export const matches = (m: LlmModelOption, query: string) =>
  matchesText(`${m.label} ${m.id} ${m.provider_label}`, query);

export type ProviderGroup = { provider: string; models: LlmModelOption[] };

/// プロバイダの見出しごとにまとめる（カタログの並び順を保つ）。
export function groupByProvider(models: LlmModelOption[]): ProviderGroup[] {
  const groups: ProviderGroup[] = [];
  for (const m of models) {
    const g = groups.find((x) => x.provider === m.provider_label);
    if (g) g.models.push(m);
    else groups.push({ provider: m.provider_label, models: [m] });
  }
  return groups;
}

/// 国外処理バッジ（NFR-10）。プロバイダ単位で国外なら見出しに 1 つ、混在するときだけ行に出す。
export function AbroadBadge() {
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
export function TrailingColumns({
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
export function ModelList({
  data,
  current,
  onPick,
  surface,
  listRef,
  defaultRow = null,
  modelsDisabled = false,
}: {
  data: LlmModelsResponse;
  /// 選択中のモデル ID（`null` は既定の行）。
  current: string | null;
  onPick: (model: string | null) => void;
  /// 見出しを貼り付けるときの背景（親の面と揃える）。
  surface: "popover" | "sheet";
  listRef: React.RefObject<HTMLDivElement | null>;
  /// 先頭の「既定」の行。`null` なら出さない（モデルを必ず選ぶ欄）。
  defaultRow?: ModelListDefaultRow | null;
  /// モデルの行を選べなくする（組織がモデルの選択を禁じている・既定の行だけ選べる）。
  modelsDisabled?: boolean;
}) {
  const listId = React.useId();
  const [rawQuery, setQuery] = React.useState("");
  const query = rawQuery.trim();
  const showSearch = data.models.length >= SEARCH_MIN_MODELS;
  const filtered = query ? data.models.filter((m) => matches(m, query)) : data.models;
  const groups = groupByProvider(filtered);
  // 既定の行も検索で引ける（呼び出し側が渡した語でも）。
  const showDefault =
    defaultRow !== null &&
    (!query || matchesText(`${defaultRow.label} ${defaultRow.keywords ?? ""}`, query));

  // キーボードで動かす順（画面の並びと同じ・選べない行は飛ばす）。
  const keys = [
    ...(showDefault ? [DEFAULT_KEY] : []),
    ...(modelsDisabled ? [] : groups.flatMap((g) => g.models.map((m) => m.id))),
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
    const disabled = modelsDisabled && key !== DEFAULT_KEY;
    return (
      <div
        key={key}
        id={optionId(key)}
        role="option"
        aria-selected={selected}
        aria-disabled={disabled || undefined}
        data-active={key === activeKey}
        data-testid={testId}
        onMouseMove={() => {
          if (disabled) return;
          scrollToActive.current = false;
          setActive(keys.indexOf(key));
        }}
        onClick={() => {
          if (!disabled) pickKey(key);
        }}
        className={cn(
          // 指で押す狭い画面は 44px、ポインタの広い画面は詰める。scroll-mt は上に貼り付く
          // プロバイダ見出しの高さ分（キーボードで上へ動いたとき行が見出しの下に隠れない）。
          "flex h-11 scroll-mt-10 cursor-pointer select-none items-center gap-2 rounded-md px-2.5 text-sm sm:h-9 sm:text-[13px]",
          "data-[active=true]:bg-accent",
          selected && "font-medium",
          disabled && "cursor-not-allowed text-muted-foreground",
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
        {!showDefault || !defaultRow
          ? null
          : row(
              DEFAULT_KEY,
              <span className="flex min-w-0 flex-1 items-baseline gap-2">
                <span className="shrink-0">{defaultRow.label}</span>
                {defaultRow.target ? (
                  <span className="truncate text-[12px] text-muted-foreground">{defaultRow.target}</span>
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
