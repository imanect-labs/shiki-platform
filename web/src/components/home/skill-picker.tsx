"use client";

/// ホームのチャットモードに置く skill セレクタ（Task 6.11「チャット開始時に skill を選択」）。
/// 選択すると次に開始するチャットへ version 込みでピンされる。skill が 1 つも無いときは `empty` を出す。
///
/// 新しい順に 6 件をすぐ選べるように並べ、ほかは「スキルを探す」から名前で絞り込んで選ぶ。50 件より古い自分の
/// スキルと、共有された分の続きは、そこで「さらに読み込む」で足す（BUG-186）。読み込みの失敗は、選択肢が無いことに
/// 見せない（BUG-189）。

import * as React from "react";
import { Check, Search, Sparkles, X } from "lucide-react";

import { ListLoadFailed, ListMore } from "@/components/artifacts/shared-more";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { useArtifactList, type ArtifactListState } from "@/hooks/use-artifact-list";
import { useMediaQuery } from "@/hooks/use-media-query";
import type { ArtifactMeta } from "@/lib/artifact-api";
import { cn } from "@/lib/utils";

/// すぐ選べるように並べる数。
const CHIPS = 6;

const CHIP =
  "inline-flex items-center gap-1 rounded-full border px-3 py-1 text-xs transition-colors pointer-coarse:py-1.5";

export function SkillPicker({
  selected,
  onSelect,
  empty = null,
}: {
  selected: ArtifactMeta | null;
  onSelect: (skill: ArtifactMeta | null) => void;
  empty?: React.ReactNode;
}) {
  const list = useArtifactList("skill");
  const skills = list.items ?? [];
  const [open, setOpen] = React.useState(false);

  // 読み込み中は何も出さない（説明も出さない。読み終えてから説明→チップへ入れ替わる揺れを避ける）。
  if (list.items === null) return null;
  if (list.error) {
    return (
      <div className="flex justify-center" aria-label="スキルを選択">
        <ListLoadFailed what="スキルの一覧" compact onRetry={list.reload} />
      </div>
    );
  }
  // 読み込んだ分が 0 件でも続きがあれば「スキルを探す」から足せるので、空の表示は続きも無いときだけ。
  if (skills.length === 0 && !list.hasMore) return <>{empty}</>;

  // 選んだスキルが先頭の 6 件に無ければ、先頭に置いて選んでいることを見せる。
  const top = skills.slice(0, CHIPS);
  const chips = selected && !top.some((s) => s.id === selected.id) ? [selected, ...top.slice(0, CHIPS - 1)] : top;
  const searchable = skills.length > chips.length || list.hasMore;

  return (
    <div className="flex flex-wrap items-center justify-center gap-1.5" aria-label="スキルを選択">
      <span className="inline-flex items-center gap-1 text-xs text-muted-foreground">
        <Sparkles className="size-3.5" aria-hidden />
        スキル:
      </span>
      {chips.map((s) => {
        const active = selected?.id === s.id;
        return (
          <button
            key={s.id}
            type="button"
            aria-pressed={active}
            onClick={() => onSelect(active ? null : s)}
            className={cn(
              CHIP,
              active
                ? "border-primary bg-primary text-primary-foreground"
                : "border-border bg-card text-foreground/70 hover:border-primary/40 hover:text-foreground",
            )}
          >
            {s.name}
            {active ? <X className="size-3" aria-hidden /> : null}
          </button>
        );
      })}
      {searchable ? (
        <button
          type="button"
          onClick={() => setOpen(true)}
          className={cn(CHIP, "border-dashed border-border text-muted-foreground hover:border-primary/40 hover:text-foreground")}
          data-testid="skill-picker-search"
        >
          <Search className="size-3" aria-hidden />
          スキルを探す
        </button>
      ) : null}
      <SkillSearchDialog
        open={open}
        onOpenChange={setOpen}
        list={list}
        selected={selected}
        onSelect={(s) => {
          onSelect(s);
          setOpen(false);
        }}
      />
    </div>
  );
}

/// すべてのスキルから名前で絞り込んで選ぶ（読み込んだ分を絞り込み、続きは「さらに読み込む」で足す）。
function SkillSearchDialog({
  open,
  onOpenChange,
  list,
  selected,
  onSelect,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  list: ArtifactListState;
  selected: ArtifactMeta | null;
  onSelect: (skill: ArtifactMeta) => void;
}) {
  const [query, setQuery] = React.useState("");
  const finePointer = useMediaQuery("(pointer: fine)");
  const inputRef = React.useRef<HTMLInputElement>(null);
  const listRef = React.useRef<HTMLUListElement>(null);
  const skills = list.items ?? [];
  const q = query.trim().toLowerCase();
  const matched = q ? skills.filter((s) => s.name.toLowerCase().includes(q)) : skills;

  return (
    <Dialog
      open={open}
      onOpenChange={(o) => {
        onOpenChange(o);
        if (!o) setQuery("");
      }}
    >
      <DialogContent
        className="flex max-h-[85dvh] max-w-md flex-col"
        data-testid="skill-picker-dialog"
        // スマホでは検索欄へ移さない（ソフトキーボードが一覧を隠す）。
        onOpenAutoFocus={(e) => {
          e.preventDefault();
          if (finePointer) inputRef.current?.focus();
          else listRef.current?.focus({ preventScroll: true });
        }}
      >
        <DialogHeader>
          <DialogTitle>スキルを選ぶ</DialogTitle>
          <DialogDescription>選んだスキルは、ここから始めるチャットで使います。</DialogDescription>
        </DialogHeader>
        <Input
          ref={inputRef}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="名前で絞り込む"
          aria-label="スキルの名前"
        />
        <ul
          ref={listRef}
          tabIndex={-1}
          // 読み込んだ分が無いときは枠を出さない（下の「さらに読み込む」の 1 文が言う）。
          hidden={!q && matched.length === 0}
          className="min-h-0 flex-1 divide-y divide-border overflow-y-auto overscroll-contain rounded-lg border border-border outline-none sm:max-h-72"
          aria-label="スキル"
        >
          {matched.map((s) => {
            const active = selected?.id === s.id;
            return (
              <li key={s.id}>
                <button
                  type="button"
                  onClick={() => onSelect(s)}
                  aria-current={active ? "true" : undefined}
                  className="flex min-h-11 w-full items-center gap-2 px-3 py-2 text-left text-sm transition-colors hover:bg-accent/50 focus-visible:bg-accent/50 focus-visible:outline-none"
                  data-testid="skill-picker-option"
                >
                  <span className="min-w-0 flex-1 truncate">{s.name}</span>
                  {active ? <Check className="size-4 shrink-0 text-primary" aria-label="選択中" /> : null}
                </button>
              </li>
            );
          })}
          {q && matched.length === 0 ? (
            <li className="px-3 py-6 text-center text-sm text-muted-foreground">
              「{query.trim()}」に合うスキルは、読み込んだ中にありません。
            </li>
          ) : null}
        </ul>
        <ListMore noun="スキル" found={skills.length > 0} list={list} compact />
      </DialogContent>
    </Dialog>
  );
}
