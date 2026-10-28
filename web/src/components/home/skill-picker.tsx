"use client";

/// ホームのチャットモードに置く skill セレクタ（Task 6.11「チャット開始時に skill を選択」）。
/// 選択すると次に開始するチャットへ version 込みでピンされる。skill が 1 つも無いときは `empty` を出す。

import * as React from "react";
import { Sparkles, X } from "lucide-react";

import { listArtifacts, type ArtifactMeta } from "@/lib/artifact-api";
import { cn } from "@/lib/utils";

export function SkillPicker({
  selected,
  onSelect,
  empty = null,
}: {
  selected: ArtifactMeta | null;
  onSelect: (skill: ArtifactMeta | null) => void;
  empty?: React.ReactNode;
}) {
  // null は読み込み中（その間は説明も出さない。読み終えてから説明→チップへ入れ替わる揺れを避ける）。
  const [skills, setSkills] = React.useState<ArtifactMeta[] | null>(null);

  React.useEffect(() => {
    let active = true;
    listArtifacts("skill")
      .then((items) => active && setSkills(items))
      .catch(() => active && setSkills([]));
    return () => {
      active = false;
    };
  }, []);

  if (skills === null) return null;
  if (skills.length === 0) return <>{empty}</>;

  return (
    <div className="flex flex-wrap items-center justify-center gap-1.5" aria-label="スキルを選択">
      <span className="inline-flex items-center gap-1 text-xs text-muted-foreground">
        <Sparkles className="size-3.5" aria-hidden />
        スキル:
      </span>
      {skills.slice(0, 6).map((s) => {
        const active = selected?.id === s.id;
        return (
          <button
            key={s.id}
            type="button"
            aria-pressed={active}
            onClick={() => onSelect(active ? null : s)}
            className={cn(
              "inline-flex items-center gap-1 rounded-full border px-3 py-1 text-xs transition-colors",
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
    </div>
  );
}
