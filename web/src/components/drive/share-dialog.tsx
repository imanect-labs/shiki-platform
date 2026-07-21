"use client";

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
import { SegmentedControl } from "@/components/ui/segmented-control";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { toast } from "@/components/ui/use-toast";
import { CopyLinkButton } from "@/components/share/copy-link-button";
import { ShareLinksPanel } from "@/components/share/share-links-panel";
import {
  listShares,
  searchDirectory,
  searchRoles,
  shareNode,
  unshareNode,
  type NodeResponse,
  type ShareEntry,
  type ShareRole,
  type ShareTarget,
} from "@/lib/storage";
import { resourcePath } from "@/lib/resource-link";

const ROLE_OPTIONS: { value: ShareRole; label: string; testId: string }[] = [
  { value: "viewer", label: "閲覧", testId: "share-role-viewer" },
  { value: "editor", label: "編集", testId: "share-role-editor" },
];

/// 共有先の種別（個人 / 部署・ロール）。
type TargetKind = ShareTarget["type"];
const KINDS: { value: TargetKind; label: string; placeholder: string }[] = [
  { value: "user", label: "メンバー", placeholder: "名前・メールで検索" },
  { value: "role", label: "部署・ロール", placeholder: "部署・ロール名で検索" },
];
const KIND_OPTIONS = [
  { value: "user", label: "メンバー", icon: UserPlus, testId: "share-kind-user" },
  { value: "role", label: "部署・ロール", icon: Users, testId: "share-kind-role" },
];

/// 検索結果を種別非依存に正規化した 1 候補。
type Candidate = { id: string; primary: string; secondary: string };

/// 共有対象の最小形。NodeResponse は構造的に適合するため既存のドライブ呼び出しは無変更で、
/// ノート/Office エディタは `{ id, name }` を直接渡せる（kind/parent_id はリンク解決の任意情報）。
export type ShareTargetNode = Pick<NodeResponse, "id" | "name"> &
  Partial<Pick<NodeResponse, "kind" | "parent_id">>;

/// 共有ダイアログ（#342）。2 タブ構成:
/// - **権限**: 特定のメンバー/部署・ロールへ直接共有（ReBAC 付与）＋既存の権限者へ渡すリンクコピー。
/// - **リンク**: 共有リンクの発行（範囲/権限/期限/パスワード）＋発行済み一覧（コピー/延長/失効）。
/// 別テナントの相手は検索結果に出ない（サーバ側 tenant_id スコープ）。部署・ロールはそのメンバー
/// （配下ロール込み）へ一括共有される（#76）。
export function ShareDialog({
  open,
  onOpenChange,
  node,
  shareUrl,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  node: ShareTargetNode | null;
  /// 共有リンクのパス（例 `/notes/{id}`）。エディタから開くとき name から拡張子が落ちて種別を
  /// 判定できないケースのために明示指定する。省略時は node から解決する。
  shareUrl?: string;
}) {
  const [kind, setKind] = React.useState<TargetKind>("user");
  const [query, setQuery] = React.useState("");
  const [results, setResults] = React.useState<Candidate[]>([]);
  const [searching, setSearching] = React.useState(false);
  const [role, setRole] = React.useState<ShareRole>("viewer");
  const [shares, setShares] = React.useState<ShareEntry[]>([]);
  const [loadingShares, setLoadingShares] = React.useState(false);
  const [pendingKey, setPendingKey] = React.useState<string | null>(null);

  // 開いたら状態リセット＋現在の共有相手を読む。
  React.useEffect(() => {
    if (!open || !node) return;
    setKind("user");
    setQuery("");
    setResults([]);
    setRole("viewer");
    setLoadingShares(true);
    listShares(node.id)
      .then(setShares)
      .catch(() => setShares([]))
      .finally(() => setLoadingShares(false));
  }, [open, node]);

  // 種別を切り替えたら検索状態をリセットする（user↔role で結果の意味が変わるため）。
  React.useEffect(() => {
    setQuery("");
    setResults([]);
  }, [kind]);

  // インクリメンタル検索（デバウンス。全件取得はせず先頭ページのみ）。active フラグで世代を守り、
  // 古いクエリ/種別の遅延レスポンスが新しい結果を上書きしないようにする。
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

  // (type, id, role) で既存共有を判定する（同じ役割のみ「付与済み」。役割の昇格は許可）。
  const sharedKeys = React.useMemo(
    () => new Set(shares.map((s) => `${s.target.type}:${s.target.id}:${s.role}`)),
    [shares],
  );

  if (!node) return null;

  const linkPath =
    shareUrl ??
    resourcePath({ id: node.id, name: node.name, kind: node.kind, parent_id: node.parent_id });
  const pointerUrl = () => {
    const origin = typeof window !== "undefined" ? window.location.origin : "";
    return `${origin}${linkPath}`;
  };

  const grant = async (candidate: Candidate) => {
    const target = { type: kind, id: candidate.id } as ShareTarget;
    const key = `${kind}:${candidate.id}:${role}`;
    setPendingKey(key);
    try {
      await shareNode(node.id, target, role);
      const next = await listShares(node.id);
      setShares(next);
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

  const revoke = async (entry: ShareEntry) => {
    const key = `${entry.target.type}:${entry.target.id}:${entry.role}`;
    setPendingKey(key);
    try {
      await unshareNode(node.id, entry.target, entry.role);
      // 解除した (type, id, role) の行だけを消す。同相手の別ロール共有はサーバ側に残るため、
      // id だけで filter すると画面から消えて「完全に外せた」と誤認する（実際は残存）。
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
      <DialogContent className="max-h-[85vh] max-w-lg overflow-y-auto">
        <DialogHeader>
          <DialogTitle>「{node.name}」を共有</DialogTitle>
          <DialogDescription>
            特定のユーザー・部署に権限を付与するか、共有リンクを発行します。
          </DialogDescription>
        </DialogHeader>

        <Tabs defaultValue="permissions">
          <TabsList className="grid w-full grid-cols-2">
            <TabsTrigger value="permissions" data-testid="share-tab-permissions">
              権限
            </TabsTrigger>
            <TabsTrigger value="links" data-testid="share-tab-links">
              リンク
            </TabsTrigger>
          </TabsList>

          {/* ===== 権限タブ（直接共有＋既存権限者向けリンクコピー） ===== */}
          <TabsContent value="permissions" className="flex flex-col gap-4">
            {/* 相手の種別＋権限 */}
            <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
              <SegmentedControl
                aria-label="共有先の種別"
                size="sm"
                options={KIND_OPTIONS}
                value={kind}
                onValueChange={(v) => setKind(v as TargetKind)}
              />
              <div className="flex items-center gap-2">
                <span className="text-sm text-muted-foreground">権限</span>
                <SegmentedControl
                  aria-label="付与する権限"
                  size="sm"
                  options={ROLE_OPTIONS}
                  value={role}
                  onValueChange={(v) => setRole(v as ShareRole)}
                />
              </div>
            </div>

            {/* 検索 */}
            <div className="relative">
              <Search
                className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground"
                aria-hidden
              />
              <Input
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder={placeholder}
                className="pl-9"
              />
            </div>

            {/* 検索結果 */}
            <div className="max-h-40 overflow-y-auto rounded-lg border border-border">
              {searching ? (
                <div className="flex items-center justify-center gap-2 py-5 text-sm text-muted-foreground">
                  <Loader2 className="size-4 animate-spin" aria-hidden />
                  検索中…
                </div>
              ) : results.length === 0 ? (
                <p className="px-3 py-5 text-center text-sm text-muted-foreground">
                  {kind === "user"
                    ? "該当するメンバーがいません"
                    : "該当する部署・ロールがありません"}
                </p>
              ) : (
                <ul className="divide-y divide-border">
                  {results.map((c) => {
                    const key = `${kind}:${c.id}:${role}`;
                    const already = sharedKeys.has(key);
                    return (
                      <li key={c.id} className="flex items-center gap-3 px-3 py-2">
                        {kind === "role" ? (
                          <span className="flex size-8 shrink-0 items-center justify-center rounded-full bg-secondary text-secondary-foreground">
                            <Users className="size-4" aria-hidden />
                          </span>
                        ) : null}
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

            {/* 共有中の相手 */}
            {loadingShares ? null : shares.length === 0 ? null : (
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
                      {s.target.type === "role" ? (
                        <span className="rounded bg-secondary px-2 py-0.5 text-xs text-secondary-foreground">
                          部署・ロール
                        </span>
                      ) : null}
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

            <div className="shiki-dash-x" />

            {/* 既存の権限者に渡すリンク（付与ゼロの純ポインタ）。 */}
            <div className="flex items-center justify-between gap-2">
              <span className="text-xs text-muted-foreground">
                アクセス権のある人へリンクを渡す
              </span>
              <CopyLinkButton url={pointerUrl} />
            </div>
          </TabsContent>

          {/* ===== リンクタブ（発行物・個別失効/延長） ===== */}
          <TabsContent value="links">
            <ShareLinksPanel nodeId={node.id} linkPath={linkPath} />
          </TabsContent>
        </Tabs>
      </DialogContent>
    </Dialog>
  );
}
