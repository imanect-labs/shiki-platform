"use client";

/// アーティファクト（skill / ミニアプリ）の共有ダイアログ（Task 6.11）。
/// UX は Drive の ShareDialog と同一（メンバー/部署検索 → viewer/editor 付与・解除）。
/// 対象が artifact（/artifacts/{id}/shares）である点だけが異なる。

import * as React from "react";
import { Loader2, Search, UserPlus, Users, X } from "lucide-react";

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
import {
  listArtifactShares,
  shareArtifact,
  unshareArtifact,
  type ArtifactRole,
  type ArtifactShareEntry,
  type ShareTarget,
} from "@/lib/artifact-api";
import { CopyLinkButton } from "@/components/share/copy-link-button";
import { searchDirectory, searchRoles } from "@/lib/storage";
import { cn } from "@/lib/utils";

const ROLES: { value: ArtifactRole; label: string }[] = [
  { value: "viewer", label: "閲覧" },
  { value: "editor", label: "編集" },
];

type TargetKind = ShareTarget["type"];
const KINDS: { value: TargetKind; label: string; placeholder: string }[] = [
  { value: "user", label: "メンバー", placeholder: "名前・メールで検索" },
  { value: "role", label: "部署・ロール", placeholder: "部署・ロール名で検索" },
];

type Candidate = { id: string; primary: string; secondary: string };

export function ArtifactShareDialog({
  open,
  onOpenChange,
  artifactId,
  name,
  shareUrl,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  artifactId: string | null;
  name: string;
  /// コピーするリンク（そのアーティファクトのページ URL）。省略時はリンクコピーを出さない
  /// （一覧ページから開いた場合に誤ったリンクを渡さないため）。
  shareUrl?: string;
}) {
  const [kind, setKind] = React.useState<TargetKind>("user");
  const [query, setQuery] = React.useState("");
  const [results, setResults] = React.useState<Candidate[]>([]);
  const [searching, setSearching] = React.useState(false);
  const [role, setRole] = React.useState<ArtifactRole>("viewer");
  const [shares, setShares] = React.useState<ArtifactShareEntry[]>([]);
  const [loadingShares, setLoadingShares] = React.useState(false);
  const [pendingKey, setPendingKey] = React.useState<string | null>(null);

  React.useEffect(() => {
    if (!open || !artifactId) return;
    setKind("user");
    setQuery("");
    setResults([]);
    setRole("viewer");
    setLoadingShares(true);
    listArtifactShares(artifactId)
      .then(setShares)
      .catch(() => setShares([]))
      .finally(() => setLoadingShares(false));
  }, [open, artifactId]);

  React.useEffect(() => {
    setQuery("");
    setResults([]);
  }, [kind]);

  // インクリメンタル検索（Drive の ShareDialog と同じ世代ガード付き）。
  React.useEffect(() => {
    if (!open) return;
    let active = true;
    const handle = setTimeout(() => {
      setSearching(true);
      const req =
        kind === "user"
          ? searchDirectory({ q: query, limit: 8 }).then((res) =>
              res.items.map((u) => ({ id: u.id, primary: u.display_name, secondary: u.email })),
            )
          : searchRoles({ q: query, limit: 8 }).then((res) =>
              res.items.map((r) => ({ id: r.id, primary: r.display_name, secondary: "部署・ロール" })),
            );
      req
        .then((items) => active && setResults(items))
        .catch(() => active && setResults([]))
        .finally(() => active && setSearching(false));
    }, 200);
    return () => {
      active = false;
      clearTimeout(handle);
    };
  }, [open, query, kind]);

  const sharedKeys = React.useMemo(
    () => new Set(shares.map((s) => `${s.target.type}:${s.target.id}:${s.role}`)),
    [shares],
  );

  if (!artifactId) return null;

  const grant = async (candidate: Candidate) => {
    const target = { type: kind, id: candidate.id } as ShareTarget;
    const key = `${kind}:${candidate.id}:${role}`;
    setPendingKey(key);
    try {
      await shareArtifact(artifactId, target, role);
      setShares(await listArtifactShares(artifactId));
      toast({
        title: "共有しました",
        description: `${candidate.primary} に${role === "editor" ? "編集" : "閲覧"}権限を付与`,
      });
    } catch (e) {
      toast({
        variant: "destructive",
        title: "共有に失敗しました",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setPendingKey(null);
    }
  };

  const revoke = async (entry: ArtifactShareEntry) => {
    const key = `${entry.target.type}:${entry.target.id}:${entry.role}`;
    setPendingKey(key);
    try {
      await unshareArtifact(artifactId, entry.target, entry.role);
      setShares((prev) =>
        prev.filter(
          (s) =>
            !(
              s.target.type === entry.target.type &&
              s.target.id === entry.target.id &&
              s.role === entry.role
            ),
        ),
      );
    } catch (e) {
      toast({
        variant: "destructive",
        title: "解除に失敗しました",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setPendingKey(null);
    }
  };

  const placeholder = KINDS.find((k) => k.value === kind)!.placeholder;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>「{name}」を共有</DialogTitle>
          <DialogDescription>
            同じ組織のメンバー・部署に権限を付与します。部署に共有するとそのメンバー全員に反映されます。
          </DialogDescription>
        </DialogHeader>

        <div className="inline-flex rounded-md border border-border p-0.5">
          {KINDS.map((k) => (
            <button
              key={k.value}
              type="button"
              aria-pressed={kind === k.value}
              onClick={() => setKind(k.value)}
              className={cn(
                "inline-flex items-center gap-1.5 rounded px-3 py-1 text-sm transition-colors",
                kind === k.value
                  ? "bg-primary text-primary-foreground"
                  : "text-muted-foreground hover:text-foreground",
              )}
            >
              {k.value === "role" ? (
                <Users className="size-3.5" aria-hidden />
              ) : (
                <UserPlus className="size-3.5" aria-hidden />
              )}
              {k.label}
            </button>
          ))}
        </div>

        <div className="flex items-center gap-2">
          <span className="text-sm text-muted-foreground">付与する権限</span>
          <div className="inline-flex rounded-md border border-border p-0.5">
            {ROLES.map((r) => (
              <button
                key={r.value}
                type="button"
                aria-pressed={role === r.value}
                onClick={() => setRole(r.value)}
                className={cn(
                  "rounded px-3 py-1 text-sm transition-colors",
                  role === r.value
                    ? "bg-primary text-primary-foreground"
                    : "text-muted-foreground hover:text-foreground",
                )}
              >
                {r.label}
              </button>
            ))}
          </div>
        </div>

        <div className="relative">
          <Search
            className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground"
            aria-hidden
          />
          <Input
            autoFocus
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={placeholder}
            className="pl-9"
          />
        </div>

        <div className="max-h-48 overflow-y-auto rounded-lg border border-border">
          {searching ? (
            <div className="flex items-center justify-center gap-2 py-5 text-sm text-muted-foreground">
              <Loader2 className="size-4 animate-spin" aria-hidden />
              検索中…
            </div>
          ) : results.length === 0 ? (
            <p className="px-3 py-5 text-center text-sm text-muted-foreground">
              {kind === "user" ? "該当するメンバーがいません" : "該当する部署・ロールがありません"}
            </p>
          ) : (
            <ul className="divide-y divide-border">
              {results.map((c) => {
                const key = `${kind}:${c.id}:${role}`;
                const already = sharedKeys.has(key);
                return (
                  <li key={c.id} className="flex items-center gap-3 px-3 py-2">
                    <div className="min-w-0 flex-1">
                      <p className="truncate text-sm font-medium">{c.primary}</p>
                      <p className="truncate text-xs text-muted-foreground">{c.secondary}</p>
                    </div>
                    <Button
                      type="button"
                      size="sm"
                      variant={already ? "ghost" : "outline"}
                      disabled={already || pendingKey === key}
                      onClick={() => void grant(c)}
                    >
                      {pendingKey === key ? (
                        <Loader2 className="size-4 animate-spin" aria-hidden />
                      ) : (
                        <UserPlus className="size-4" aria-hidden />
                      )}
                      {already ? "共有済み" : "共有"}
                    </Button>
                  </li>
                );
              })}
            </ul>
          )}
        </div>

        <div>
          <p className="mb-2 text-sm font-medium">共有中の相手</p>
          {loadingShares ? (
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <Loader2 className="size-4 animate-spin" aria-hidden />
              読み込み中…
            </div>
          ) : shares.length === 0 ? (
            <p className="text-sm text-muted-foreground">まだ誰にも共有していません。</p>
          ) : (
            <ul className="flex flex-col gap-1">
              {shares.map((s) => {
                const key = `${s.target.type}:${s.target.id}:${s.role}`;
                return (
                  <li
                    key={key}
                    className="flex items-center gap-2 rounded-md border border-border px-3 py-2"
                  >
                    {s.target.type === "role" ? (
                      <Users className="size-4 shrink-0 text-muted-foreground" aria-hidden />
                    ) : null}
                    <span className="min-w-0 flex-1 truncate text-sm">{s.target.id}</span>
                    <span className="rounded bg-secondary px-2 py-0.5 text-xs text-secondary-foreground">
                      {s.role === "editor" ? "編集" : "閲覧"}
                    </span>
                    <button
                      type="button"
                      aria-label="共有を解除"
                      disabled={pendingKey === key}
                      onClick={() => void revoke(s)}
                      className="rounded p-1 text-muted-foreground transition-colors hover:bg-destructive/10 hover:text-destructive"
                    >
                      {pendingKey === key ? (
                        <Loader2 className="size-4 animate-spin" aria-hidden />
                      ) : (
                        <X className="size-4" aria-hidden />
                      )}
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </div>

        {shareUrl ? (
          <>
            <div className="shiki-dash-x" />
            <div className="flex justify-start">
              <CopyLinkButton url={shareUrl} />
            </div>
          </>
        ) : null}
      </DialogContent>
    </Dialog>
  );
}
