"use client";

/// メタデータ（プロパティ）パネル（Task 11P.3）。
///
/// Yjs Map "meta"（title/icon/tags/任意 key-value）を直接編集する。frontmatter への
/// 反映は保存時のシリアライズ（Task 11P.2）が行うため、ここは Yjs だけを触る。
/// thread_id は 11P.5 が管理する（このパネルには出さない）。

import { Plus, Tag, X } from "lucide-react";
import * as React from "react";
import type * as Y from "yjs";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

/// パネルが扱う予約キー（title/icon は専用 UI・thread_id は非表示）。
const RESERVED_KEYS = new Set(["title", "icon", "tags", "thread_id"]);

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
}: {
  meta: Y.Map<unknown>;
  editable: boolean;
}) {
  const [state, setState] = React.useState<MetaState>(() => readMeta(meta));
  const [tagDraft, setTagDraft] = React.useState("");
  // プロパティ追加行は既定で畳む（Notion 風・常時ダッシュ枠を出さない）。
  const [adding, setAdding] = React.useState(false);
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

  return (
    <section aria-label="ノートのプロパティ" className="space-y-3" data-testid="note-meta-panel">
      <div className="flex items-start gap-3">
        {/* アイコン（絵文字 1 文字想定・自由入力） */}
        <Input
          value={state.icon}
          onChange={(e) => setKey("icon", e.target.value)}
          disabled={!editable}
          placeholder="📝"
          aria-label="アイコン"
          className="size-14 shrink-0 border-transparent bg-transparent text-center text-3xl shadow-none focus-visible:border-input"
        />
        <div className="min-w-0 flex-1">
          <Input
            value={state.title}
            onChange={(e) => setKey("title", e.target.value)}
            disabled={!editable}
            placeholder="無題のノート"
            aria-label="タイトル"
            data-testid="note-title-input"
            className="h-14 border-transparent bg-transparent px-2 text-3xl font-bold shadow-none focus-visible:border-input"
          />
        </div>
      </div>

      <div className="flex flex-wrap items-center gap-2 px-2 text-sm">
        <Tag className="size-4 text-muted-foreground" aria-hidden />
        {state.tags.map((tag) => (
          <span
            key={tag}
            className="inline-flex items-center gap-1 rounded-full border bg-muted/50 px-2.5 py-0.5 text-xs font-medium"
          >
            {tag}
            {editable && (
              <button
                type="button"
                onClick={() => removeTag(tag)}
                aria-label={`タグ ${tag} を削除`}
                className="text-muted-foreground transition-colors hover:text-foreground"
              >
                <X className="size-3" />
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
            className="h-7 w-32 border-dashed text-xs"
          />
        )}
      </div>

      {(state.extra.length > 0 || editable) && (
        <dl className="space-y-0.5 px-2">
          {state.extra.map(([key, value]) => (
            <div key={key} className="group/prop flex items-center gap-2 text-sm">
              <dt className="w-36 shrink-0 truncate text-[13px] text-muted-foreground">{key}</dt>
              <dd className="min-w-0 flex-1">
                <Input
                  value={value}
                  onChange={(e) => setKey(key, e.target.value)}
                  disabled={!editable}
                  aria-label={`プロパティ ${key}`}
                  placeholder="空"
                  className="h-8 border-transparent bg-transparent px-2 text-sm shadow-none placeholder:text-muted-foreground/50 hover:bg-accent/50 focus-visible:border-input focus-visible:bg-background"
                />
              </dd>
              {editable && (
                <Button
                  type="button"
                  variant="ghost"
                  size="icon"
                  onClick={() => meta.delete(key)}
                  aria-label={`プロパティ ${key} を削除`}
                  className="size-8 shrink-0 text-muted-foreground opacity-0 transition-opacity group-hover/prop:opacity-100 focus-visible:opacity-100"
                >
                  <X className="size-4" />
                </Button>
              )}
            </div>
          ))}
          {editable &&
            (adding ? (
              <div className="flex items-center gap-2 pt-1 text-sm">
                <Input
                  value={kvDraft.key}
                  onChange={(e) => setKvDraft((d) => ({ ...d, key: e.target.value }))}
                  placeholder="プロパティ名"
                  aria-label="プロパティ名"
                  autoFocus
                  onKeyDown={(e) => {
                    if (e.key === "Escape") setAdding(false);
                  }}
                  className="h-8 w-36 shrink-0 text-sm"
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
                  className="h-8 flex-1 text-sm"
                />
                <Button
                  type="button"
                  variant="ghost"
                  size="icon"
                  onClick={addKv}
                  aria-label="プロパティを保存"
                  className="size-8 shrink-0 text-muted-foreground"
                >
                  <Plus className="size-4" />
                </Button>
              </div>
            ) : (
              <button
                type="button"
                onClick={() => setAdding(true)}
                className="mt-0.5 inline-flex items-center gap-1.5 rounded-md px-2 py-1 text-[13px] text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
              >
                <Plus className="size-3.5" aria-hidden />
                プロパティを追加
              </button>
            ))}
        </dl>
      )}
    </section>
  );
}
