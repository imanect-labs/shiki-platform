"use client";

/// 組織の利用者を選ぶ部品（ディレクトリ検索・同じテナントだけ・自分は出ない）。
/// 招待・チャンネル作成・DM の相手・閲覧の承認者の指名で共有する。

import * as React from "react";
import { Check, Loader2, Search, X } from "lucide-react";

import { cn } from "@/lib/utils";
import { searchUsers, type DirectoryUser } from "@/lib/messaging-api";
import { UserAvatar } from "./primitives";

export type PickedUser = { id: string; name: string };

/// 検索語に合う利用者の一覧（打つたびに少し待ってから引く）。
export function useUserSearch(query: string, enabled = true) {
  const [items, setItems] = React.useState<DirectoryUser[]>([]);
  const [loading, setLoading] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  React.useEffect(() => {
    if (!enabled) return;
    let active = true;
    setLoading(true);
    const t = setTimeout(() => {
      searchUsers(query.trim(), 20)
        .then((r) => {
          if (!active) return;
          setItems(r);
          setError(null);
        })
        .catch((e: unknown) => active && setError(e instanceof Error ? e.message : String(e)))
        .finally(() => active && setLoading(false));
    }, 200);
    return () => {
      active = false;
      clearTimeout(t);
    };
  }, [query, enabled]);
  return { items, loading, error };
}

/// 複数選択（チップ＋検索結果のチェック一覧）。`exclude` は候補に出さない id（参加済みなど）。
export function UserMultiPicker({
  value,
  onChange,
  exclude = [],
  label,
  autoFocus,
}: {
  value: PickedUser[];
  onChange: (next: PickedUser[]) => void;
  exclude?: string[];
  label: string;
  autoFocus?: boolean;
}) {
  const [query, setQuery] = React.useState("");
  const { items, loading, error } = useUserSearch(query);
  const candidates = items.filter((u) => !exclude.includes(u.id));
  const toggle = (u: DirectoryUser) =>
    onChange(
      value.some((v) => v.id === u.id)
        ? value.filter((v) => v.id !== u.id)
        : [...value, { id: u.id, name: u.display_name || u.email }],
    );

  return (
    <div className="flex flex-col gap-2">
      {value.length > 0 ? (
        <div className="flex flex-wrap gap-1.5" aria-label="選んだ利用者">
          {value.map((v) => (
            <span
              key={v.id}
              className="flex items-center gap-1.5 rounded-full border border-border/60 bg-accent/50 py-0.5 pl-1 pr-1 text-[12px] text-foreground"
            >
              <UserAvatar userId={v.id} name={v.name} size="xs" className="rounded-full" />
              {v.name}
              <button
                type="button"
                onClick={() => onChange(value.filter((x) => x.id !== v.id))}
                aria-label={`${v.name} を外す`}
                className="flex size-4 items-center justify-center rounded-full text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
              >
                <X className="size-3" aria-hidden />
              </button>
            </span>
          ))}
        </div>
      ) : null}
      <div className="flex h-9 items-center gap-2 rounded-lg border border-input bg-transparent px-2.5 focus-within:border-ring/60">
        <Search className="size-4 shrink-0 text-muted-foreground" aria-hidden />
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="名前・メールで探す"
          aria-label={label}
          autoFocus={autoFocus}
          className="min-w-0 flex-1 bg-transparent text-[13px] outline-none placeholder:text-muted-foreground/70"
        />
        {loading ? <Loader2 className="size-3.5 animate-spin text-muted-foreground" aria-hidden /> : null}
      </div>
      <div className="rule-soft max-h-[220px] overflow-y-auto rounded-xl border border-border/60" role="listbox" aria-label={label} aria-multiselectable>
        {error ? (
          <p className="px-3 py-3 text-[12px] text-destructive" role="alert">
            {error}
          </p>
        ) : candidates.length === 0 && !loading ? (
          <p className="px-3 py-3 text-[12px] text-muted-foreground">該当する利用者がいません</p>
        ) : (
          candidates.map((u, i) => {
            const on = value.some((v) => v.id === u.id);
            return (
              <button
                key={u.id}
                type="button"
                role="option"
                aria-selected={on}
                onClick={() => toggle(u)}
                className={cn(
                  "flex w-full items-center gap-2.5 px-3 py-2 text-left outline-none transition-colors hover:bg-accent/50 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring",
                  i > 0 && "shiki-dash-top",
                  on && "bg-accent/60",
                )}
              >
                <UserAvatar userId={u.id} name={u.display_name || u.email} size="sm" />
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-[13px] font-medium text-foreground">{u.display_name || u.email}</span>
                  <span className="block truncate text-[11px] text-muted-foreground">{u.email}</span>
                </span>
                <span
                  className={cn(
                    "flex size-5 items-center justify-center rounded-[5px] border",
                    on ? "border-transparent bg-primary text-primary-foreground" : "border-border",
                  )}
                  aria-hidden
                >
                  {on ? <Check className="size-3.5" /> : null}
                </span>
              </button>
            );
          })
        )}
      </div>
    </div>
  );
}
