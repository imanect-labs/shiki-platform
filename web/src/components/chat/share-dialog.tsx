"use client";

import * as React from "react";
import { Loader2, Search, Users, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { toast } from "@/components/ui/use-toast";
import { searchDirectory, searchRoles } from "@/lib/storage";
import {
  listThreadShares,
  shareThread,
  unshareThread,
  type ShareTarget,
  type ThreadRole,
  type ThreadShareEntry,
} from "@/lib/chat-api";
import { cn } from "@/lib/utils";

const ROLES: { value: ThreadRole; label: string }[] = [
  { value: "viewer", label: "閲覧" },
  { value: "commenter", label: "コメント" },
  { value: "editor", label: "編集" },
];

type TargetKind = ShareTarget["type"];
const KINDS: { value: TargetKind; label: string; placeholder: string }[] = [
  { value: "user", label: "メンバー", placeholder: "名前・メールで検索" },
  { value: "role", label: "部署・ロール", placeholder: "部署・ロール名で検索" },
];

type Candidate = { id: string; primary: string; secondary: string };

/// スレッド共有ダイアログ。同テナントのメンバー / 部署・ロールへ viewer/commenter/editor で共有する。
/// 共有された相手がスレッドを開くと、引用は**その閲覧者の権限で再評価**される（他人の引用は見せない）。
export function ThreadShareDialog({
  open,
  onOpenChange,
  threadId,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  threadId: string | null;
}) {
  const [kind, setKind] = React.useState<TargetKind>("user");
  const [query, setQuery] = React.useState("");
  const [results, setResults] = React.useState<Candidate[]>([]);
  const [searching, setSearching] = React.useState(false);
  const [role, setRole] = React.useState<ThreadRole>("viewer");
  const [shares, setShares] = React.useState<ThreadShareEntry[]>([]);
  const [busy, setBusy] = React.useState(false);

  const reloadShares = React.useCallback(() => {
    if (!threadId) return;
    listThreadShares(threadId)
      .then(setShares)
      .catch(() => setShares([]));
  }, [threadId]);

  React.useEffect(() => {
    if (open) reloadShares();
    else {
      setQuery("");
      setResults([]);
    }
  }, [open, reloadShares]);

  // オートコンプリート（debounce）。
  React.useEffect(() => {
    if (!open) return;
    const q = query.trim();
    if (!q) {
      setResults([]);
      return;
    }
    setSearching(true);
    const t = setTimeout(async () => {
      try {
        if (kind === "user") {
          const r = await searchDirectory({ q, limit: 8 });
          setResults(
            r.items.map((u) => ({
              id: u.id,
              primary: u.display_name || u.id,
              secondary: u.email ?? "",
            })),
          );
        } else {
          const r = await searchRoles({ q, limit: 8 });
          setResults(
            r.items.map((role) => ({
              id: role.id,
              primary: role.display_name || role.id,
              secondary: "部署・ロール",
            })),
          );
        }
      } catch {
        setResults([]);
      } finally {
        setSearching(false);
      }
    }, 200);
    return () => clearTimeout(t);
  }, [query, kind, open]);

  const doShare = async (targetId: string) => {
    if (!threadId) return;
    setBusy(true);
    try {
      const target: ShareTarget = { type: kind, id: targetId };
      await shareThread(threadId, target, role);
      toast({ description: "共有しました" });
      setQuery("");
      setResults([]);
      reloadShares();
    } catch (e) {
      toast({ description: e instanceof Error ? e.message : "共有に失敗しました" });
    } finally {
      setBusy(false);
    }
  };

  const doUnshare = async (entry: ThreadShareEntry) => {
    if (!threadId) return;
    setBusy(true);
    try {
      await unshareThread(threadId, entry.target, entry.role);
      reloadShares();
    } catch (e) {
      toast({ description: e instanceof Error ? e.message : "共有解除に失敗しました" });
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>会話を共有</DialogTitle>
          <DialogDescription>
            同じテナントのメンバー・部署へ共有します。共有相手には、その人自身の権限で引用が再評価されます。
          </DialogDescription>
        </DialogHeader>

        {/* 種別＋権限 */}
        <div className="flex flex-wrap items-center gap-2">
          <div className="inline-flex rounded-lg border border-border p-0.5">
            {KINDS.map((k) => (
              <button
                key={k.value}
                type="button"
                onClick={() => setKind(k.value)}
                className={cn(
                  "rounded-md px-3 py-1.5 text-[13px] font-medium transition-colors",
                  kind === k.value ? "bg-secondary text-foreground" : "text-muted-foreground hover:text-foreground",
                )}
              >
                {k.label}
              </button>
            ))}
          </div>
          <div className="inline-flex rounded-lg border border-border p-0.5">
            {ROLES.map((r) => (
              <button
                key={r.value}
                type="button"
                onClick={() => setRole(r.value)}
                className={cn(
                  "rounded-md px-3 py-1.5 text-[13px] font-medium transition-colors",
                  role === r.value ? "bg-secondary text-foreground" : "text-muted-foreground hover:text-foreground",
                )}
              >
                {r.label}
              </button>
            ))}
          </div>
        </div>

        {/* 検索 */}
        <div className="relative">
          <Search className="absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={KINDS.find((k) => k.value === kind)?.placeholder}
            className="pl-9"
          />
          {searching ? (
            <Loader2 className="absolute right-3 top-1/2 size-4 -translate-y-1/2 animate-spin text-muted-foreground" />
          ) : null}
        </div>

        {results.length > 0 ? (
          <ul className="max-h-52 overflow-y-auto rounded-lg border border-border">
            {results.map((c) => (
              <li key={c.id}>
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => doShare(c.id)}
                  className="flex w-full items-center gap-2 px-3 py-2 text-left text-sm hover:bg-secondary disabled:opacity-50"
                >
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-foreground">{c.primary}</span>
                    {c.secondary ? (
                      <span className="block truncate text-xs text-muted-foreground">{c.secondary}</span>
                    ) : null}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        ) : null}

        {/* 現在の共有相手 */}
        <div>
          <p className="mb-1.5 flex items-center gap-1.5 text-xs font-medium uppercase tracking-wide text-muted-foreground">
            <Users className="size-3.5" /> 共有中
          </p>
          {shares.length === 0 ? (
            <p className="text-sm text-muted-foreground">まだ誰にも共有していません。</p>
          ) : (
            <ul className="flex flex-col gap-1">
              {shares.map((s, i) => (
                <li
                  key={`${s.target.type}:${s.target.id}:${s.role}:${i}`}
                  className="flex items-center justify-between rounded-lg border border-border px-3 py-2 text-sm"
                >
                  <span className="min-w-0 truncate">
                    {s.target.type === "role" ? "部署/ロール " : ""}
                    {s.target.id}
                    <span className="ml-2 text-xs text-muted-foreground">
                      {ROLES.find((r) => r.value === s.role)?.label}
                    </span>
                  </span>
                  <Button
                    variant="ghost"
                    size="sm"
                    disabled={busy}
                    onClick={() => doUnshare(s)}
                    aria-label="共有を解除"
                  >
                    <X className="size-4" />
                  </Button>
                </li>
              ))}
            </ul>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
