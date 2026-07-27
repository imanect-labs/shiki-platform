"use client";

import * as React from "react";
import {
  Building2,
  Check,
  Clock,
  Copy,
  Link2,
  Loader2,
  Lock,
  type LucideIcon,
  ShieldOff,
  Trash2,
  UserRound,
  Users,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { SegmentedControl } from "@/components/ui/segmented-control";
import { Switch } from "@/components/ui/switch";
import { toast } from "@/components/ui/use-toast";
import {
  createShareLink,
  extendShareLink,
  listShareLinkGrants,
  listShareLinks,
  revokeShareLink,
  type GeneralAccessLevel,
  type ShareLink,
  type ShareLinkGrant,
  type ShareRole,
} from "@/lib/storage";
import { cn } from "@/lib/utils";

/// audience（リンクの公開範囲）。#342 レビュー A-2 で「社内全員」「組織内」を **broad 1 つ**へ統合した
/// （到達集合が organization#member で同一になるため・区別は誤解を生む）。①匿名は #341 で対応（disabled）。
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
    value: "organization",
    label: "社内の全員",
    desc: "社内（テナント）の全員がリンクから開けます。",
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

/// 今日（ローカル）の YYYY-MM-DD。date input の min に使い、過去日を選ばせない（B-5・UI 側）。
function todayInput(): string {
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

/// 一覧表示用ラベル。`anyone`（legacy）は `organization` と同義に縮退（A-2）。
const AUDIENCE_LABEL: Record<GeneralAccessLevel, string> = {
  anyone: "社内の全員",
  organization: "社内の全員",
  restricted: "既存の権限者のみ",
};

/// リンクタブ本体（#342）: 発行フォーム＋発行済み一覧。owner のみ操作できる。
///
/// `passwordSupported`: パスワード付きリンクは解錠画面のあるページ（ノート/Office/スライド/CSV）でしか
/// 開けない。フォルダ・非プレビューファイル（drive 解決）では発行させない（#342 レビュー C-1）。
export function ShareLinksPanel({
  nodeId,
  linkPath,
  passwordSupported,
}: {
  nodeId: string;
  linkPath: string;
  passwordSupported: boolean;
}) {
  const [links, setLinks] = React.useState<ShareLink[]>([]);
  const [loading, setLoading] = React.useState(true);
  const [forbidden, setForbidden] = React.useState(false);
  const [creating, setCreating] = React.useState(false);

  // 発行フォームの状態。broad は organization に統合（A-2）。
  const [audience, setAudience] = React.useState<GeneralAccessLevel>("organization");
  const [role, setRole] = React.useState<ShareRole>("viewer");
  const [expiry, setExpiry] = React.useState("");
  const [pwEnabled, setPwEnabled] = React.useState(false);
  const [pwValue, setPwValue] = React.useState("");

  // 操作中のリンク・延長編集中のリンク。
  const [pendingId, setPendingId] = React.useState<string | null>(null);
  const [copiedId, setCopiedId] = React.useState<string | null>(null);
  const [editingId, setEditingId] = React.useState<string | null>(null);
  const [editExpiry, setEditExpiry] = React.useState("");

  // C-3（#369）: 解錠済み user の可視化・個別取り消し。開いているリンク・取得済み一覧・取消中 user。
  const [grantsOpenId, setGrantsOpenId] = React.useState<string | null>(null);
  const [grantsMap, setGrantsMap] = React.useState<Record<string, ShareLinkGrant[]>>({});
  const [grantsLoadingId, setGrantsLoadingId] = React.useState<string | null>(null);

  React.useEffect(() => {
    let active = true;
    setLoading(true);
    setForbidden(false);
    listShareLinks(nodeId)
      .then((ls) => active && setLinks(ls))
      .catch((e: unknown) => {
        if (!active) return;
        setLinks([]);
        // 非 owner は 403。空フォームを見せず「権限なし」状態にする（#342 レビュー C-2）。
        const status = (e as { status?: number } | null)?.status;
        const msg = e instanceof Error ? e.message : String(e);
        if (status === 403 || /403|forbidden|権限/i.test(msg)) setForbidden(true);
      })
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

  const pwOn = passwordSupported && pwEnabled;
  const pwMissing = pwOn && !pwValue;

  const create = async () => {
    if (pwMissing) {
      toast({ variant: "destructive", description: "パスワードを入力してください。" });
      return;
    }
    // A-1: 非パスワードの broad/restricted リンクは URL が同一で区別できない。同一 (audience, role) が
    // 既にあれば発行させない（サーバも 409 で弾くが、UI 側で先に止めて既存へ誘導する）。
    if (!pwOn) {
      const dup = links.some(
        (l) => !l.has_password && l.audience === audience && l.role === role,
      );
      if (dup) {
        toast({
          variant: "destructive",
          description:
            "同じ公開範囲のリンクが既にあります。既存のリンクをコピーして共有してください。",
        });
        return;
      }
    }
    setCreating(true);
    try {
      // restricted（付与ゼロの純ポインタ）は期限/パスワードを持たない。UI で隠れていても直前の別
      // audience の入力状態が残るため、送信前に確実に落とす。
      const scoped = audience !== "restricted";
      const link = await createShareLink(nodeId, {
        audience,
        role,
        expires_at: scoped && expiry ? dateInputToIso(expiry) : null,
        password: scoped && pwOn && pwValue ? pwValue : null,
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
      const status = (e as { status?: number } | null)?.status;
      const conflict = status === 409 || /409|conflict/i.test(e instanceof Error ? e.message : "");
      toast({
        variant: "destructive",
        description: conflict
          ? "同じ公開範囲のリンクが既にあります。"
          : e instanceof Error
            ? e.message
            : "リンクの発行に失敗しました。",
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
      // broad リンクの失効は「公開範囲の解除」（誰か 1 人ではなく全員のアクセスが切れる）。
      // パスワードリンクだけが per-user capability として真に個別失効できる（A-1）。
      toast({
        description: link.has_password
          ? "パスワードリンクを失効しました。"
          : `${AUDIENCE_LABEL[link.audience]}への公開を解除しました。`,
      });
    } catch (e) {
      toast({
        variant: "destructive",
        description: e instanceof Error ? e.message : "解除に失敗しました。",
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

  // C-3: 解錠済み user 一覧を開閉する（開くとき未取得なら遅延ロード）。
  const toggleGrants = async (link: ShareLink) => {
    if (grantsOpenId === link.link_id) {
      setGrantsOpenId(null);
      return;
    }
    setGrantsOpenId(link.link_id);
    if (grantsMap[link.link_id]) return;
    setGrantsLoadingId(link.link_id);
    try {
      const grants = await listShareLinkGrants(link.link_id);
      setGrantsMap((prev) => ({ ...prev, [link.link_id]: grants }));
    } catch (e) {
      toast({
        variant: "destructive",
        description: e instanceof Error ? e.message : "解錠済みユーザーの取得に失敗しました。",
      });
      setGrantsOpenId((id) => (id === link.link_id ? null : id));
    } finally {
      setGrantsLoadingId(null);
    }
  };

  // 非 owner にはフォームを見せない（C-2）。
  if (forbidden) {
    return (
      <div className="flex flex-col items-center gap-2 py-8 text-center text-sm text-muted-foreground">
        <ShieldOff className="size-6" aria-hidden />
        <p>共有リンクを管理する権限がありません。</p>
        <p className="text-xs">オーナーに発行を依頼してください。</p>
      </div>
    );
  }

  const minDate = todayInput();

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
                  min={minDate}
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
                {passwordSupported ? (
                  <Switch
                    id="link-password-toggle"
                    data-testid="link-password-toggle"
                    checked={pwEnabled}
                    onCheckedChange={(v) => {
                      setPwEnabled(v);
                      if (!v) setPwValue("");
                    }}
                  />
                ) : (
                  <span className="text-xs text-muted-foreground">この種別では未対応</span>
                )}
              </div>
              {pwOn ? (
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
                    aria-label={link.has_password ? "リンクを失効" : "この公開範囲を解除"}
                    title={link.has_password ? "リンクを失効" : "この公開範囲を解除"}
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
                      min={minDate}
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
                {/* C-3（#369）: パスワードリンクを解錠した user の可視化・個別取り消し。 */}
                {link.has_password && link.redeem_count > 0 ? (
                  <div className="flex flex-col gap-1.5 border-t border-border/40 pt-2">
                    <button
                      type="button"
                      data-testid="link-grants-toggle"
                      onClick={() => void toggleGrants(link)}
                      className="flex items-center gap-1.5 self-start text-xs text-muted-foreground transition-colors hover:text-foreground"
                    >
                      <Users className="size-3.5" aria-hidden />
                      {link.redeem_count} 人が解錠済み
                    </button>
                    {grantsOpenId === link.link_id ? (
                      grantsLoadingId === link.link_id ? (
                        <p className="flex items-center gap-1 pl-1 text-xs text-muted-foreground">
                          <Loader2 className="size-3 animate-spin" aria-hidden />
                          読み込み中…
                        </p>
                      ) : (
                        <ul className="flex flex-col gap-1" data-testid="link-grant-list">
                          {(grantsMap[link.link_id] ?? []).map((g) => (
                            <li
                              key={g.user_id}
                              data-testid="link-grant-item"
                              className="flex items-center gap-2 rounded-md bg-muted/40 px-2 py-1"
                            >
                              <UserRound
                                className="size-3.5 shrink-0 text-muted-foreground"
                                aria-hidden
                              />
                              <span className="min-w-0 flex-1 truncate text-xs">
                                {g.display_name ?? g.user_id}
                              </span>
                              <span className="shrink-0 text-[11px] text-muted-foreground">
                                {isoToDateInput(g.granted_at)}
                              </span>
                            </li>
                          ))}
                        </ul>
                      )
                    ) : null}
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
