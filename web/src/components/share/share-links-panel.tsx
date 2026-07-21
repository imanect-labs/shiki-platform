"use client";

import * as React from "react";
import {
  Building2,
  Check,
  Clock,
  Copy,
  Globe2,
  Link2,
  Loader2,
  Lock,
  type LucideIcon,
  Trash2,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { SegmentedControl } from "@/components/ui/segmented-control";
import { Switch } from "@/components/ui/switch";
import { toast } from "@/components/ui/use-toast";
import {
  createShareLink,
  extendShareLink,
  listShareLinks,
  revokeShareLink,
  type GeneralAccessLevel,
  type ShareLink,
  type ShareRole,
} from "@/lib/storage";
import { cn } from "@/lib/utils";

/// audience（リンクの公開範囲）。OneDrive/Google 風の入れ子。①匿名は #341 で対応（disabled）。
const AUDIENCES: {
  value: GeneralAccessLevel | "anonymous";
  label: string;
  desc: string;
  icon: LucideIcon;
  disabled?: boolean;
  testId: string;
}[] = [
  {
    value: "anonymous",
    label: "リンクを知っている全員",
    desc: "匿名・社外にも公開（近日対応・#341）。",
    icon: Link2,
    disabled: true,
    testId: "link-audience-anonymous",
  },
  {
    value: "anyone",
    label: "社内全員",
    desc: "社内（テナント）の全員がリンクから開けます。",
    icon: Globe2,
    testId: "link-audience-anyone",
  },
  {
    value: "organization",
    label: "組織内",
    desc: "自分の組織のメンバーがリンクから開けます。",
    icon: Building2,
    testId: "link-audience-organization",
  },
  {
    value: "restricted",
    label: "既存のアクセス権を持つ人のみ",
    desc: "新たな権限は付与しない純粋なリンク（既存の権限者に渡す用）。",
    icon: Lock,
    testId: "link-audience-restricted",
  },
];

const ROLE_OPTIONS: { value: ShareRole; label: string }[] = [
  { value: "viewer", label: "閲覧" },
  { value: "editor", label: "編集" },
];

/// ISO 日時 → date input（YYYY-MM-DD・ローカル）。
function isoToDateInput(iso: string | null | undefined): string {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

/// date input（YYYY-MM-DD） → ISO 日時（その日の終わり・ローカル 23:59:59）。
function dateInputToIso(date: string): string {
  return new Date(`${date}T23:59:59`).toISOString();
}

const AUDIENCE_LABEL: Record<GeneralAccessLevel, string> = {
  anyone: "社内全員",
  organization: "組織内",
  restricted: "既存の権限者のみ",
};

/// リンクタブ本体（#342）: 発行フォーム＋発行済み一覧（コピー/延長/失効）。owner のみ表示。
export function ShareLinksPanel({ nodeId, linkPath }: { nodeId: string; linkPath: string }) {
  const [links, setLinks] = React.useState<ShareLink[]>([]);
  const [loading, setLoading] = React.useState(true);
  const [creating, setCreating] = React.useState(false);

  // 発行フォームの状態。
  const [audience, setAudience] = React.useState<GeneralAccessLevel>("anyone");
  const [role, setRole] = React.useState<ShareRole>("viewer");
  const [expiry, setExpiry] = React.useState("");
  const [pwEnabled, setPwEnabled] = React.useState(false);
  const [pwValue, setPwValue] = React.useState("");

  // 操作中のリンク・延長編集中のリンク。
  const [pendingId, setPendingId] = React.useState<string | null>(null);
  const [copiedId, setCopiedId] = React.useState<string | null>(null);
  const [editingId, setEditingId] = React.useState<string | null>(null);
  const [editExpiry, setEditExpiry] = React.useState("");

  React.useEffect(() => {
    let active = true;
    setLoading(true);
    listShareLinks(nodeId)
      .then((ls) => active && setLinks(ls))
      .catch(() => active && setLinks([]))
      .finally(() => active && setLoading(false));
    return () => {
      active = false;
    };
  }, [nodeId]);

  /// リンクの共有 URL を組む。パスワード付きは token を載せて解錠ヒント（?lt=..&unlock=1）を付す。
  /// broad/既存向けリンクは token を要さず、リソースへのポインタ（bare URL）で開ける。
  const buildUrl = React.useCallback(
    (link: ShareLink): string => {
      const origin = typeof window !== "undefined" ? window.location.origin : "";
      if (!link.has_password) return `${origin}${linkPath}`;
      const sep = linkPath.includes("?") ? "&" : "?";
      return `${origin}${linkPath}${sep}lt=${encodeURIComponent(link.token)}&unlock=1`;
    },
    [linkPath],
  );

  const copyUrl = async (link: ShareLink) => {
    try {
      await navigator.clipboard.writeText(buildUrl(link));
      setCopiedId(link.link_id);
      window.setTimeout(() => setCopiedId((id) => (id === link.link_id ? null : id)), 1600);
      toast({ description: "リンクをコピーしました。" });
    } catch {
      toast({ variant: "destructive", description: "リンクをコピーできませんでした。" });
    }
  };

  const pwMissing = pwEnabled && !pwValue;

  const create = async () => {
    if (pwMissing) {
      toast({ variant: "destructive", description: "パスワードを入力してください。" });
      return;
    }
    setCreating(true);
    try {
      // restricted（付与ゼロの純ポインタ）は期限/パスワードを持たない。UI で隠れていても
      // 直前の別 audience の入力状態が残るため、送信前に確実に落とす（Codex P2）。
      const scoped = audience !== "restricted";
      const link = await createShareLink(nodeId, {
        audience,
        role,
        expires_at: scoped && expiry ? dateInputToIso(expiry) : null,
        password: scoped && pwEnabled && pwValue ? pwValue : null,
        label: null,
      });
      setLinks((prev) => [link, ...prev]);
      // 発行と同時にコピーまで済ませる（Google/MS 式）。
      await copyUrl(link);
      // フォームをリセット。
      setExpiry("");
      setPwEnabled(false);
      setPwValue("");
    } catch (e) {
      toast({
        variant: "destructive",
        description: e instanceof Error ? e.message : "リンクの発行に失敗しました。",
      });
    } finally {
      setCreating(false);
    }
  };

  const revoke = async (link: ShareLink) => {
    setPendingId(link.link_id);
    try {
      await revokeShareLink(link.link_id);
      setLinks((prev) => prev.filter((l) => l.link_id !== link.link_id));
      toast({ description: "リンクを失効しました。" });
    } catch (e) {
      toast({
        variant: "destructive",
        description: e instanceof Error ? e.message : "失効に失敗しました。",
      });
    } finally {
      setPendingId(null);
    }
  };

  const applyExtend = async (link: ShareLink) => {
    setPendingId(link.link_id);
    try {
      const next = editExpiry ? dateInputToIso(editExpiry) : null;
      await extendShareLink(link.link_id, next);
      setLinks((prev) =>
        prev.map((l) => (l.link_id === link.link_id ? { ...l, expires_at: next } : l)),
      );
      setEditingId(null);
      toast({ description: "有効期限を更新しました。" });
    } catch (e) {
      toast({
        variant: "destructive",
        description: e instanceof Error ? e.message : "延長に失敗しました。",
      });
    } finally {
      setPendingId(null);
    }
  };

  return (
    <div className="flex flex-col gap-4">
      {/* 発行フォーム */}
      <div className="flex flex-col gap-3 rounded-lg border border-border/60 bg-card/40 p-3">
        <p className="text-sm font-medium">共有リンクを作成</p>
        <div className="flex flex-col" role="radiogroup" aria-label="リンクの公開範囲">
          {AUDIENCES.map((a) => {
            const active = !a.disabled && audience === a.value;
            const Icon = a.icon;
            return (
              <button
                key={a.value}
                type="button"
                role="radio"
                aria-checked={active}
                disabled={a.disabled}
                data-testid={a.testId}
                onClick={() => !a.disabled && setAudience(a.value as GeneralAccessLevel)}
                className={cn(
                  "flex items-center gap-3 rounded-lg border px-3 py-2 text-left transition-colors",
                  active ? "border-border bg-accent" : "border-transparent hover:bg-accent/40",
                  a.disabled && "cursor-not-allowed opacity-50 hover:bg-transparent",
                )}
              >
                <Icon className="size-5 shrink-0 text-muted-foreground" aria-hidden />
                <span className="min-w-0 flex-1">
                  <span className="block text-sm font-medium leading-tight">{a.label}</span>
                  <span className="mt-0.5 block text-xs text-muted-foreground">{a.desc}</span>
                </span>
                <span
                  className={cn(
                    "flex size-4 shrink-0 items-center justify-center rounded-full border",
                    active ? "border-foreground/60" : "border-muted-foreground/40",
                  )}
                  aria-hidden
                >
                  {active ? <span className="size-2 rounded-full bg-foreground" /> : null}
                </span>
              </button>
            );
          })}
        </div>

        {/* その他の設定（restricted 以外＝権限を配るときのみ） */}
        {audience !== "restricted" ? (
          <div className="flex flex-col gap-3">
            <div className="flex items-center justify-between gap-2">
              <span className="text-sm text-muted-foreground">権限</span>
              <SegmentedControl
                aria-label="リンクの権限"
                size="sm"
                options={ROLE_OPTIONS}
                value={role}
                onValueChange={(v) => setRole(v as ShareRole)}
              />
            </div>
            <div className="flex items-center justify-between gap-2">
              <label htmlFor="link-expiry" className="text-sm text-muted-foreground">
                有効期限
              </label>
              <div className="flex items-center gap-1.5">
                <Input
                  id="link-expiry"
                  data-testid="link-expiry"
                  type="date"
                  value={expiry}
                  onChange={(e) => setExpiry(e.target.value)}
                  className="h-8 w-40 text-sm"
                />
                {expiry ? (
                  <Button type="button" variant="ghost" size="sm" onClick={() => setExpiry("")}>
                    なし
                  </Button>
                ) : null}
              </div>
            </div>
            <div className="flex flex-col gap-2">
              <div className="flex items-center justify-between gap-2">
                <label htmlFor="link-password-toggle" className="text-sm text-muted-foreground">
                  パスワード保護
                </label>
                <Switch
                  id="link-password-toggle"
                  data-testid="link-password-toggle"
                  checked={pwEnabled}
                  onCheckedChange={(v) => {
                    setPwEnabled(v);
                    if (!v) setPwValue("");
                  }}
                />
              </div>
              {pwEnabled ? (
                <Input
                  data-testid="link-password"
                  type="password"
                  autoComplete="new-password"
                  value={pwValue}
                  onChange={(e) => setPwValue(e.target.value)}
                  placeholder="パスワードを入力"
                  className="h-8 text-sm"
                />
              ) : null}
            </div>
          </div>
        ) : null}

        <div className="flex justify-end">
          <Button
            type="button"
            size="sm"
            loading={creating}
            disabled={pwMissing}
            onClick={() => void create()}
            data-testid="link-create"
          >
            <Link2 className="size-4" aria-hidden />
            リンクを作成
          </Button>
        </div>
      </div>

      {/* 発行済みリンク一覧 */}
      <div className="flex flex-col gap-2" data-testid="link-list">
        <p className="text-sm font-medium">発行済みリンク</p>
        {loading ? (
          <div className="flex items-center gap-2 py-4 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin" aria-hidden />
            読み込み中…
          </div>
        ) : links.length === 0 ? (
          <p className="py-4 text-center text-sm text-muted-foreground">
            まだ発行済みのリンクはありません。
          </p>
        ) : (
          <ul className="flex flex-col gap-2">
            {links.map((link) => (
              <li
                key={link.link_id}
                data-testid="link-item"
                className="flex flex-col gap-2 rounded-lg border border-border/60 bg-card/40 px-3 py-2.5"
              >
                <div className="flex items-center gap-2">
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-sm font-medium">
                      {AUDIENCE_LABEL[link.audience]}
                      <span className="ml-1.5 text-xs font-normal text-muted-foreground">
                        {link.role === "editor" ? "編集" : "閲覧"}
                      </span>
                    </p>
                    <p className="mt-0.5 flex items-center gap-1 text-xs text-muted-foreground">
                      <Clock className="size-3" aria-hidden />
                      {link.expires_at ? `${isoToDateInput(link.expires_at)} まで` : "無期限"}
                      {link.has_password ? (
                        <span className="ml-1 inline-flex items-center gap-0.5">
                          <Lock className="size-3" aria-hidden />
                          パスワード
                        </span>
                      ) : null}
                    </p>
                  </div>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    data-testid="link-copy"
                    onClick={() => void copyUrl(link)}
                  >
                    {copiedId === link.link_id ? (
                      <Check className="size-4" aria-hidden />
                    ) : (
                      <Copy className="size-4" aria-hidden />
                    )}
                    {copiedId === link.link_id ? "コピー済み" : "コピー"}
                  </Button>
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    data-testid="link-extend"
                    onClick={() => {
                      setEditingId((id) => (id === link.link_id ? null : link.link_id));
                      setEditExpiry(isoToDateInput(link.expires_at));
                    }}
                  >
                    延長
                  </Button>
                  <button
                    type="button"
                    aria-label="リンクを失効"
                    data-testid="link-revoke"
                    disabled={pendingId === link.link_id}
                    onClick={() => void revoke(link)}
                    className="rounded p-1 text-muted-foreground transition-colors hover:bg-destructive/10 hover:text-destructive"
                  >
                    {pendingId === link.link_id ? (
                      <Loader2 className="size-4 animate-spin" aria-hidden />
                    ) : (
                      <Trash2 className="size-4" aria-hidden />
                    )}
                  </button>
                </div>
                {editingId === link.link_id ? (
                  <div className="flex items-center gap-1.5">
                    <Input
                      type="date"
                      value={editExpiry}
                      onChange={(e) => setEditExpiry(e.target.value)}
                      className="h-8 w-40 text-sm"
                    />
                    <Button type="button" variant="ghost" size="sm" onClick={() => setEditExpiry("")}>
                      無期限
                    </Button>
                    <Button
                      type="button"
                      size="sm"
                      loading={pendingId === link.link_id}
                      onClick={() => void applyExtend(link)}
                    >
                      適用
                    </Button>
                  </div>
                ) : null}
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
