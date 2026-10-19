"use client";

/// メタデータ（プロパティ）パネル（Task 11P.3）。
///
/// Yjs Map "meta"（title/icon/tags/任意 key-value）を直接編集する。frontmatter への
/// 反映は保存時のシリアライズ（Task 11P.2）が行うため、ここは Yjs だけを触る。
/// thread_id は 11P.5 が管理する（このパネルには出さない）。
///
/// スマホでは題名の下を「＋ タグ・プロパティ」の 1 行に畳み（タグは 1 行で見せる）、押すと
/// 全体を開く。題名は折り返して全文を見せる（input だと「営業戦略ミーティン」で切れていた）。

import { ChevronUp, Plus, SmilePlus, Tag, X } from "lucide-react";
import * as React from "react";
import type * as Y from "yjs";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { useMediaQuery } from "@/hooks/use-media-query";
import { cn } from "@/lib/utils";

/// Tailwind の `md`。これ以上の幅では畳まず、常に全体を見せる。
const DESKTOP_QUERY = "(min-width: 768px)";

/// パネルが扱う予約キー（title/icon は専用 UI・thread_id は非表示）。
const RESERVED_KEYS = new Set(["title", "icon", "tags", "thread_id", "active_thread_id"]);

interface MetaState {
  title: string;
  icon: string;
  tags: string[];
  extra: Array<[string, string]>;
}

function readMeta(map: Y.Map<unknown>): MetaState {
  const tags = map.get("tags");
  const extra: Array<[string, string]> = [];
  for (const [key, value] of map.entries()) {
    if (!RESERVED_KEYS.has(key) && typeof value === "string") {
      extra.push([key, value]);
    }
  }
  extra.sort(([a], [b]) => a.localeCompare(b));
  return {
    title: typeof map.get("title") === "string" ? (map.get("title") as string) : "",
    icon: typeof map.get("icon") === "string" ? (map.get("icon") as string) : "",
    tags: Array.isArray(tags) ? tags.filter((t): t is string => typeof t === "string") : [],
    extra,
  };
}

export function MetadataPanel({
  meta,
  editable,
  fallbackTitle,
}: {
  meta: Y.Map<unknown>;
  editable: boolean;
  /// meta.title 未設定時にプレースホルダとして見せる名前（通常は拡張子を除いたファイル名）。
  /// アップロードした .md は frontmatter を持たないため meta.title が空で、
  /// 固定文言だけだと中身のあるノートが「無題」に見えていた（#467）。
  /// **表示だけのフォールバックで、Yjs には書かない**（ユーザーが実際に付けた題と区別する）。
  fallbackTitle?: string;
}) {
  const [state, setState] = React.useState<MetaState>(() => readMeta(meta));
  const [tagDraft, setTagDraft] = React.useState("");
  // プロパティ追加行は既定で畳む（Notion 風・常時ダッシュ枠を出さない）。
  const [adding, setAdding] = React.useState(false);
  // スマホでタグ・プロパティを開いているか（PC は常に開いた表示）。
  const [expanded, setExpanded] = React.useState(false);
  const [kvDraft, setKvDraft] = React.useState<{ key: string; value: string }>({
    key: "",
    value: "",
  });

  React.useEffect(() => {
    const update = () => setState(readMeta(meta));
    update();
    meta.observe(update);
    return () => meta.unobserve(update);
  }, [meta]);

  const setKey = React.useCallback(
    (key: string, value: string) => {
      if (value === "") meta.delete(key);
      else meta.set(key, value);
    },
    [meta],
  );

  const addTag = () => {
    const tag = tagDraft.trim();
    if (!tag || state.tags.includes(tag)) return;
    meta.set("tags", [...state.tags, tag]);
    setTagDraft("");
  };
  const removeTag = (tag: string) => {
    const next = state.tags.filter((t) => t !== tag);
    if (next.length === 0) meta.delete("tags");
    else meta.set("tags", next);
  };
  const addKv = () => {
    const key = kvDraft.key.trim();
    if (!key || RESERVED_KEYS.has(key)) return;
    meta.set(key, kvDraft.value);
    setKvDraft({ key: "", value: "" });
    setAdding(false);
  };

  const hasDetails = state.tags.length > 0 || state.extra.length > 0;
  // 畳んだ要約と全体は、いまの幅・開閉に合う方だけを描画する。CSS で片方を隠すだけだと
  // タグが DOM に 2 つずつ並び、支援技術・検索（ページ内の文字列）にも二重に見える。
  // サーバ描画と初回は狭い画面扱い（false）になるので、要約には md:hidden も残して PC で
  // 一瞬出ないようにする。
  const desktop = useMediaQuery(DESKTOP_QUERY);
  const collapsed = !desktop && !expanded;

  return (
    <section aria-label="ノートのプロパティ" className="space-y-3" data-testid="note-meta-panel">
      <div className="flex items-start gap-3">
        {/* アイコン（絵文字 1 文字想定・自由入力）。未設定時は絵文字 placeholder（環境により
            豆腐□になる）を使わず、muted の lucide アイコンを重ねて「追加できる枠」だと示す。 */}
        <div className="relative size-14 shrink-0">
          <Input
            value={state.icon}
            onChange={(e) => setKey("icon", e.target.value)}
            disabled={!editable}
            aria-label="アイコン"
            className={cn(
              "size-14 rounded-lg border-transparent bg-transparent text-center text-3xl shadow-none",
              "transition-colors focus-visible:border-input",
              editable && !state.icon && "hover:bg-accent/50",
            )}
          />
          {!state.icon ? (
            <SmilePlus
              className="pointer-events-none absolute inset-0 m-auto size-6 text-muted-foreground/40"
              aria-hidden
            />
          ) : null}
        </div>
        <div className="min-w-0 flex-1">
          <TitleField
            value={state.title}
            onChange={(v) => setKey("title", v)}
            disabled={!editable}
            placeholder={fallbackTitle?.trim() || "無題のノート"}
          />
        </div>
      </div>

      {/* スマホ: 1 行に畳んだ要約（タグは横に並べて見せ、押すと全体を開く）。 */}
      {collapsed && (editable || hasDetails) ? (
        <div className="flex min-w-0 items-center gap-2 px-2 text-sm md:hidden">
          {state.tags.length > 0 ? (
            <div className="flex min-w-0 flex-1 items-center gap-1.5 overflow-hidden">
              <Tag className="size-4 shrink-0 text-muted-foreground" aria-hidden />
              {state.tags.map((tag) => (
                <span
                  key={tag}
                  className="shrink-0 rounded-full border bg-muted/50 px-2.5 py-0.5 text-xs font-medium"
                >
                  {tag}
                </span>
              ))}
            </div>
          ) : null}
          {/* 閲覧者でもタグがあれば開ける（要約は overflow-hidden で、入り切らないタグは全体を開かないと見えない）。 */}
          {editable || hasDetails ? (
            <button
              type="button"
              onClick={() => setExpanded(true)}
              aria-expanded={false}
              className={cn(
                "inline-flex h-9 shrink-0 items-center gap-1.5 rounded-full px-2.5 text-[13px] text-muted-foreground transition-colors active:bg-accent",
                state.tags.length === 0 && "-ml-2.5",
              )}
            >
              {editable ? <Plus className="size-3.5" aria-hidden /> : null}
              {editable
                ? "タグ・プロパティ"
                : state.extra.length > 0
                  ? `プロパティ ${state.extra.length} 件`
                  : "すべて表示"}
            </button>
          ) : null}
        </div>
      ) : null}

      {collapsed ? null : (
        <div className="space-y-3">
          <div className="flex flex-wrap items-center gap-2 px-2 text-sm">
            <Tag className="size-4 text-muted-foreground" aria-hidden />
            {state.tags.map((tag) => (
              <span
                key={tag}
                className="inline-flex items-center gap-1 rounded-full border bg-muted/50 px-2.5 py-0.5 text-xs font-medium max-md:py-1"
              >
                {tag}
                {editable && (
                  <button
                    type="button"
                    onClick={() => removeTag(tag)}
                    aria-label={`タグ ${tag} を削除`}
                    className="relative -mr-1 flex size-4 items-center justify-center rounded-full text-muted-foreground transition-colors hover:text-foreground after:absolute after:-inset-1.5 after:content-[''] pointer-coarse:size-5 pointer-coarse:after:-inset-3"
                  >
                    <X className="size-3.5" />
                  </button>
                )}
              </span>
            ))}
            {editable && (
              <Input
                value={tagDraft}
                onChange={(e) => setTagDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    addTag();
                  }
                }}
                placeholder="タグを追加…"
                aria-label="タグを追加"
                enterKeyHint="done"
                className="h-7 w-32 border-dashed text-base max-md:h-9 max-md:w-36 md:text-xs"
              />
            )}
          </div>

          {(state.extra.length > 0 || editable) && (
            <dl className="space-y-0.5 px-2">
              {state.extra.map(([key, value]) => (
                <div
                  key={key}
                  className="group/prop flex items-center gap-2 text-sm max-md:flex-wrap max-md:gap-x-1 max-md:gap-y-0 max-md:pb-1.5"
                >
                  {/* スマホは「名前」の行と「値＋削除」の行の 2 段組み（固定幅の名前列で値が潰れない）。 */}
                  <dt className="w-36 shrink-0 truncate text-[13px] text-muted-foreground max-md:w-full max-md:px-2 max-md:text-[12px]">
                    {key}
                  </dt>
                  <dd className="min-w-0 flex-1">
                    <Input
                      value={value}
                      onChange={(e) => setKey(key, e.target.value)}
                      disabled={!editable}
                      aria-label={`プロパティ ${key}`}
                      placeholder="空"
                      className="h-8 border-transparent bg-transparent px-2 text-base shadow-none placeholder:text-muted-foreground/50 hover:bg-accent/50 focus-visible:border-input focus-visible:bg-background max-md:h-10 md:text-sm"
                    />
                  </dd>
                  {editable && (
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon"
                      onClick={() => meta.delete(key)}
                      aria-label={`プロパティ ${key} を削除`}
                      // マウスの端末だけホバーで出す（タッチでは常に見える）。
                      className="size-8 shrink-0 text-muted-foreground transition-opacity focus-visible:opacity-100 max-md:size-10 pointer-fine:opacity-0 pointer-fine:group-hover/prop:opacity-100"
                    >
                      <X className="size-4" />
                    </Button>
                  )}
                </div>
              ))}
              {editable &&
                (adding ? (
                  <div className="flex items-center gap-2 pt-1 text-sm max-md:flex-wrap">
                    <Input
                      value={kvDraft.key}
                      onChange={(e) => setKvDraft((d) => ({ ...d, key: e.target.value }))}
                      placeholder="プロパティ名"
                      aria-label="プロパティ名"
                      autoFocus
                      autoCapitalize="none"
                      autoCorrect="off"
                      spellCheck={false}
                      onKeyDown={(e) => {
                        if (e.key === "Escape") setAdding(false);
                      }}
                      className="h-8 w-36 shrink-0 text-base max-md:h-10 max-md:w-full md:text-sm"
                    />
                    <Input
                      value={kvDraft.value}
                      onChange={(e) => setKvDraft((d) => ({ ...d, value: e.target.value }))}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") {
                          e.preventDefault();
                          addKv();
                        } else if (e.key === "Escape") {
                          setAdding(false);
                        }
                      }}
                      placeholder="値"
                      aria-label="プロパティ値"
                      enterKeyHint="done"
                      className="h-8 flex-1 text-base max-md:h-10 md:text-sm"
                    />
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon"
                      onClick={addKv}
                      aria-label="プロパティを保存"
                      className="size-8 shrink-0 text-muted-foreground max-md:size-10"
                    >
                      <Plus className="size-4" />
                    </Button>
                  </div>
                ) : (
                  <button
                    type="button"
                    onClick={() => setAdding(true)}
                    className="mt-0.5 inline-flex items-center gap-1.5 rounded-md px-2 py-1 text-[13px] text-muted-foreground transition-colors hover:bg-accent hover:text-foreground max-md:h-10"
                  >
                    <Plus className="size-3.5" aria-hidden />
                    プロパティを追加
                  </button>
                ))}
            </dl>
          )}
          {/* スマホ: 開いた全体を畳み直す。 */}
          {desktop ? null : (
            <button
              type="button"
              onClick={() => setExpanded(false)}
              aria-expanded
              className="ml-0.5 inline-flex h-9 items-center gap-1.5 rounded-full px-2.5 text-[13px] text-muted-foreground transition-colors active:bg-accent"
            >
              <ChevronUp className="size-3.5" aria-hidden />
              閉じる
            </button>
          )}
        </div>
      )}
    </section>
  );
}

/// 題名欄。折り返して全文を見せる（1 行の input だと長い題名が切れる）ため textarea を内容の
/// 高さに合わせて伸ばす。改行は入れない（Enter は確定・貼り付けの改行は空白へ）。
function TitleField({
  value,
  onChange,
  disabled,
  placeholder,
}: {
  value: string;
  onChange: (value: string) => void;
  disabled: boolean;
  placeholder: string;
}) {
  const ref = React.useRef<HTMLTextAreaElement>(null);
  const fit = React.useCallback(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, []);
  React.useLayoutEffect(fit, [fit, value, placeholder]);
  // 幅が変わると折り返しが変わるので測り直す（回転・アシスタントの開閉）。
  React.useEffect(() => {
    const el = ref.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(fit);
    ro.observe(el);
    return () => ro.disconnect();
  }, [fit]);

  return (
    <textarea
      ref={ref}
      rows={1}
      value={value}
      onChange={(e) => onChange(e.target.value.replace(/[\r\n]+/g, " "))}
      onKeyDown={(e) => {
        if (e.key === "Enter" && !e.nativeEvent.isComposing) e.preventDefault();
      }}
      disabled={disabled}
      placeholder={placeholder}
      aria-label="タイトル"
      data-testid="note-title-input"
      enterKeyHint="done"
      className={cn(
        "block min-h-14 w-full resize-none overflow-hidden rounded-md border border-transparent bg-transparent px-2 py-2",
        "text-3xl font-bold leading-tight tracking-tight outline-none [overflow-wrap:anywhere] placeholder:text-muted-foreground",
        "transition-colors focus-visible:border-input focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background",
        "disabled:cursor-default disabled:opacity-100 max-md:text-[26px]",
      )}
    />
  );
}
