"use client";

import * as React from "react";
import { Globe2, Loader2, Lock, type LucideIcon, Users } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { SegmentedControl } from "@/components/ui/segmented-control";
import { Switch } from "@/components/ui/switch";
import { toast } from "@/components/ui/use-toast";
import {
  clearGeneralAccess,
  getGeneralAccess,
  setGeneralAccess,
  type GeneralAccess,
  type GeneralAccessLevel,
  type ShareRole,
} from "@/lib/storage";
import { cn } from "@/lib/utils";

/// 「アクセスできる範囲」の選択肢。ユーザーの語彙に合わせる（既存アクセス者のみ/組織内/全員）。
const LEVELS: { value: GeneralAccessLevel; label: string; icon: LucideIcon; testId: string }[] = [
  { value: "restricted", label: "既存のアクセス権のある人のみ", icon: Lock, testId: "ga-level-restricted" },
  { value: "organization", label: "組織内のユーザー", icon: Users, testId: "ga-level-organization" },
  { value: "anyone", label: "すべてのユーザー", icon: Globe2, testId: "ga-level-anyone" },
];

const ROLES: { value: ShareRole; label: string; testId: string }[] = [
  { value: "viewer", label: "閲覧", testId: "ga-role-viewer" },
  { value: "editor", label: "編集", testId: "ga-role-editor" },
];

/// レベルの説明文（現在の役割を織り込む）。
function levelDesc(level: GeneralAccessLevel, role: ShareRole): string {
  const verb = role === "editor" ? "編集" : "閲覧";
  switch (level) {
    case "restricted":
      return "下で追加した特定のユーザー・部署だけが開けます。";
    case "organization":
      return `組織内のユーザーはリンクから${verb}できます。`;
    case "anyone":
      return `リンクを知っている認証済みユーザー全員が${verb}できます。`;
  }
}

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

/// 共有ダイアログの「アクセスできる範囲」セクション（#338）。owner だけが変更でき、
/// 公開範囲（既存アクセス者のみ/組織内/全員）・役割・有効期限・パスワードを扱う。
export function GeneralAccessSection({
  nodeId,
  onServerChange,
}: {
  nodeId: string;
  /// 現在の設定（未取得/権限なしは null）を親へ通知する（リンクの unlock ヒント用）。
  onServerChange?: (ga: GeneralAccess | null) => void;
}) {
  const [server, setServer] = React.useState<GeneralAccess | null>(null);
  const [loading, setLoading] = React.useState(true);
  const [saving, setSaving] = React.useState(false);

  // 編集中の状態。
  const [level, setLevel] = React.useState<GeneralAccessLevel>("restricted");
  const [role, setRole] = React.useState<ShareRole>("viewer");
  const [expiry, setExpiry] = React.useState("");
  const [pwEnabled, setPwEnabled] = React.useState(false);
  const [pwValue, setPwValue] = React.useState("");
  const [pwTouched, setPwTouched] = React.useState(false);

  const hydrate = React.useCallback(
    (ga: GeneralAccess) => {
      setServer(ga);
      setLevel(ga.level);
      setRole(ga.role);
      setExpiry(isoToDateInput(ga.expires_at));
      setPwEnabled(ga.has_password);
      setPwValue("");
      setPwTouched(false);
      onServerChange?.(ga);
    },
    [onServerChange],
  );

  React.useEffect(() => {
    let active = true;
    setLoading(true);
    getGeneralAccess(nodeId)
      .then((ga) => active && hydrate(ga))
      .catch(() => {
        if (active) {
          setServer(null);
          onServerChange?.(null);
        }
      })
      .finally(() => active && setLoading(false));
    return () => {
      active = false;
    };
  }, [nodeId, hydrate, onServerChange]);

  const dirty =
    !!server &&
    (level !== server.level ||
      (level !== "restricted" &&
        (role !== server.role ||
          expiry !== isoToDateInput(server.expires_at) ||
          pwEnabled !== server.has_password ||
          pwTouched)));

  // パスワード保護 ON なのに（新規で）パスワード未入力は保存不可。
  const pwMissing = level !== "restricted" && pwEnabled && !pwValue && !server?.has_password;

  const save = async () => {
    if (pwMissing) {
      toast({ variant: "destructive", description: "パスワードを入力してください。" });
      return;
    }
    setSaving(true);
    try {
      if (level === "restricted") {
        await clearGeneralAccess(nodeId);
      } else {
        const body = {
          level,
          role,
          expires_at: expiry ? dateInputToIso(expiry) : null,
        } as Parameters<typeof setGeneralAccess>[1];
        if (!pwEnabled) {
          body.password = null;
          body.keep_password = false;
        } else if (pwValue) {
          body.password = pwValue;
          body.keep_password = false;
        } else {
          body.keep_password = true; // 既存パスワードを引き継ぐ（level/期限だけ変更）。
        }
        await setGeneralAccess(nodeId, body);
      }
      const next = await getGeneralAccess(nodeId);
      hydrate(next);
      toast({ description: "共有設定を更新しました。" });
    } catch (e) {
      toast({
        variant: "destructive",
        description: e instanceof Error ? e.message : "更新に失敗しました。",
      });
    } finally {
      setSaving(false);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center gap-2 text-sm text-muted-foreground">
        <Loader2 className="size-4 animate-spin" aria-hidden />
        読み込み中…
      </div>
    );
  }
  // 権限が無い（owner でない）等で取得できなければセクション非表示。
  if (!server) return null;

  return (
    <div className="flex flex-col gap-2.5">
      <p className="text-sm font-medium">アクセスできる範囲</p>

      {/* 範囲の選択（ラジオ・選択は塗り bg-accent／黒枠は使わない） */}
      <div className="flex flex-col gap-1.5" role="radiogroup" aria-label="アクセスできる範囲">
        {LEVELS.map((l) => {
          const active = level === l.value;
          const Icon = l.icon;
          return (
            <button
              key={l.value}
              type="button"
              role="radio"
              aria-checked={active}
              data-testid={l.testId}
              onClick={() => setLevel(l.value)}
              className={cn(
                "flex items-start gap-3 rounded-lg border px-3 py-2.5 text-left transition-colors",
                active
                  ? "border-border bg-accent"
                  : "border-border/60 hover:bg-accent/40",
              )}
            >
              <span
                className={cn(
                  "mt-0.5 flex size-4 shrink-0 items-center justify-center rounded-full border",
                  active ? "border-foreground/50" : "border-muted-foreground/40",
                )}
                aria-hidden
              >
                {active ? <span className="size-2 rounded-full bg-foreground" /> : null}
              </span>
              <Icon className="mt-px size-4 shrink-0 text-muted-foreground" aria-hidden />
              <span className="min-w-0">
                <span className="block text-sm font-medium leading-tight">{l.label}</span>
                <span className="mt-0.5 block text-xs text-muted-foreground">
                  {levelDesc(l.value, role)}
                </span>
              </span>
            </button>
          );
        })}
      </div>

      {/* 範囲が restricted 以外なら 役割・有効期限・パスワードを出す（枠は border-border/60 + bg-card/40） */}
      {level !== "restricted" ? (
        <div className="space-y-3 rounded-lg border border-border/60 bg-card/40 p-3">
          <div className="flex items-center justify-between gap-2">
            <span className="text-sm text-muted-foreground">権限</span>
            <SegmentedControl
              aria-label="一般アクセスの権限"
              size="sm"
              options={ROLES}
              value={role}
              onValueChange={(v) => setRole(v as ShareRole)}
            />
          </div>

          <div className="flex items-center justify-between gap-2">
            <label htmlFor="ga-expiry" className="text-sm text-muted-foreground">
              有効期限
            </label>
            <div className="flex items-center gap-1.5">
              <Input
                id="ga-expiry"
                data-testid="ga-expiry"
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

          <div className="space-y-2">
            <div className="flex items-center justify-between gap-2">
              <label htmlFor="ga-password-toggle" className="text-sm text-muted-foreground">
                パスワード保護
              </label>
              <Switch
                id="ga-password-toggle"
                data-testid="ga-password-toggle"
                checked={pwEnabled}
                onCheckedChange={(v) => {
                  setPwEnabled(v);
                  setPwTouched(true);
                  if (!v) setPwValue("");
                }}
              />
            </div>
            {pwEnabled ? (
              <Input
                data-testid="ga-password"
                type="password"
                autoComplete="new-password"
                value={pwValue}
                onChange={(e) => {
                  setPwValue(e.target.value);
                  setPwTouched(true);
                }}
                placeholder={
                  server.has_password ? "設定済み（変更する場合のみ入力）" : "パスワードを入力"
                }
                className="h-8 text-sm"
              />
            ) : null}
          </div>
        </div>
      ) : null}

      {dirty ? (
        <div className="flex justify-end">
          <Button
            type="button"
            size="sm"
            loading={saving}
            disabled={pwMissing}
            onClick={() => void save()}
            data-testid="ga-save"
          >
            変更を保存
          </Button>
        </div>
      ) : null}
    </div>
  );
}
