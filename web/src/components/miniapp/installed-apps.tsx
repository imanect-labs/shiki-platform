"use client";

/// インストール済みコードアプリ（B1/B2）セクション（Task 9.11/9.13b）。
///
/// A（宣言的 mini_app）と同じ「アプリ」ページに載せ、同一シェルから一覧・起動できるようにする。
/// インストールはレジストリ（publish 済みエントリ）から**要求スコープを見て同意**する。アプリがテーブルを持つときは、
/// テーブルに「追加だけ」（レコードを追加し、自分が追加したレコードだけ見られる・D-113）を渡す部署・グループを選べる
/// （GAP-53・インストールの `submitter_roles`）。

import * as React from "react";
import Link from "next/link";
import { Download, Loader2, Package, Play, Trash2, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { toast } from "@/components/ui/use-toast";
import { ConfirmDialog } from "@/components/drive/dialogs";
import { RefPicker } from "@/components/data/ref-picker";
import type { FieldDef } from "@/lib/data-api";
import { cachedRoleName, rememberRoleName } from "@/lib/role-names";
import {
  fetchManifest,
  installApp,
  listInstallations,
  listRegistry,
  uninstallApp,
  type AppInstallation,
  type RegistryEntry,
} from "@/lib/miniapp-b1-api";

/// 部署・グループを探して選ぶ（ディレクトリの部署・`role_ref` の列と同じ探し方）。
const ROLE_FIELD: FieldDef = { key: "role", display_name: "部署・グループ", type: "role_ref" } as FieldDef;

export function InstalledAppsSection() {
  const [installed, setInstalled] = React.useState<AppInstallation[] | null>(null);
  const [registry, setRegistry] = React.useState<RegistryEntry[]>([]);
  const [consent, setConsent] = React.useState<{
    entry: RegistryEntry;
    requested: string[];
    granted: Set<string>;
    /// アプリが持つテーブルの名前（インストールで作られる）。
    tables: string[];
    /// テーブルに「追加だけ」を渡す部署・グループ（id）。
    submitters: string[];
    /// アプリのコード（画面かサーバーの処理）がある（ゲートウェイを呼ぶトークンを受け取る）。
    runsCode: boolean;
  } | null>(null);
  const [pending, setPending] = React.useState<string | null>(null);
  const [removing, setRemoving] = React.useState<AppInstallation | null>(null);

  const reload = React.useCallback(() => {
    listInstallations()
      .then(setInstalled)
      .catch(() => setInstalled([]));
    listRegistry()
      .then(setRegistry)
      .catch(() => setRegistry([]));
  }, []);
  React.useEffect(reload, [reload]);

  const openConsent = async (entry: RegistryEntry) => {
    try {
      const manifest = await fetchManifest(entry.artifact_id, entry.artifact_version);
      const requested = manifest.requested_scopes ?? [];
      setConsent({
        entry,
        requested,
        granted: new Set(requested),
        tables: (manifest.tables ?? []).map((t) => t.name),
        submitters: [],
        runsCode: Boolean(manifest.frontend || manifest.server),
      });
    } catch (e) {
      toast({
        variant: "destructive",
        title: "マニフェストの取得に失敗しました",
        description: e instanceof Error ? e.message : String(e),
      });
    }
  };

  const doInstall = async () => {
    if (!consent) return;
    setPending(consent.entry.id);
    try {
      await installApp({
        name: consent.entry.name,
        version: consent.entry.version,
        grantedScopes: [...consent.granted],
        submitterRoles: consent.submitters,
      });
      toast({ title: `${consent.entry.name} をインストールしました` });
      setConsent(null);
      reload();
    } catch (e) {
      toast({
        variant: "destructive",
        title: "インストールに失敗しました",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setPending(null);
    }
  };

  /// アンインストール（確認ダイアログから呼ぶ。失敗はダイアログ内に出す）。
  const doUninstall = async (app: AppInstallation) => {
    setPending(app.app_id);
    try {
      await uninstallApp(app.app_id);
      toast({ title: "アンインストールしました" });
      reload();
    } finally {
      setPending(null);
    }
  };

  const installedIds = new Set((installed ?? []).map((i) => `${i.app_name}@${i.installed_version}`));
  const installable = registry.filter(
    (r) => !r.yanked && !installedIds.has(`${r.name}@${r.version}`),
  );

  return (
    <section className="mt-8">
      <div className="mb-3">
        <div>
          <h2 className="text-base font-semibold">インストール済みアプリ（コード）</h2>
          <p className="mt-0.5 text-xs text-muted-foreground">
            レジストリから同意インストールした B1/B2 アプリ。宣言的アプリと同じくここから起動できます。
          </p>
        </div>
      </div>

      {installed === null ? (
        <div className="flex items-center justify-center gap-2 py-8 text-sm text-muted-foreground">
          <Loader2 className="size-4 animate-spin" aria-hidden />
          読み込み中…
        </div>
      ) : installed.length === 0 ? (
        <p className="rounded-lg border border-dashed px-4 py-6 text-center text-sm text-muted-foreground">
          インストール済みのコードアプリはありません。
        </p>
      ) : (
        <ul className="grid gap-3 sm:grid-cols-2">
          {installed.map((app) => (
            <li key={app.id} className="flex flex-col gap-2 rounded-xl border border-border bg-card p-4">
              <div className="min-w-0">
                <h3 className="truncate text-[15px] font-semibold">{app.app_name}</h3>
                <p className="text-xs text-muted-foreground">
                  v{app.installed_version}・スコープ {app.granted_scopes.length} 件
                </p>
              </div>
              <div className="mt-auto flex flex-wrap items-center gap-1 pt-1">
                <Button size="sm" asChild disabled={!app.frontend_bundle}>
                  <Link href={`/apps/b1/${app.app_id}`}>
                    <Play className="size-4" aria-hidden />
                    開く
                  </Link>
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  aria-label={`${app.app_name} をアンインストール`}
                  onClick={() => setRemoving(app)}
                  className="text-muted-foreground hover:text-destructive"
                >
                  {pending === app.app_id ? (
                    <Loader2 className="size-4 animate-spin" aria-hidden />
                  ) : (
                    <Trash2 className="size-4" aria-hidden />
                  )}
                </Button>
              </div>
            </li>
          ))}
        </ul>
      )}

      {installable.length > 0 ? (
        <div className="mt-6">
          <h3 className="mb-2 text-sm font-semibold text-muted-foreground">
            <Package className="mr-1 inline size-4" aria-hidden />
            レジストリからインストール
          </h3>
          <ul className="grid gap-2 sm:grid-cols-2">
            {installable.map((entry) => (
              <li
                key={entry.id}
                className="flex items-center justify-between gap-2 rounded-lg border px-3 py-2.5"
              >
                <div className="min-w-0">
                  <p className="truncate text-sm font-medium">{entry.name}</p>
                  <p className="text-xs text-muted-foreground">
                    v{entry.version}・{entry.trust_tier === "first_party" ? "署名済み" : "社内"}
                  </p>
                </div>
                <Button size="sm" variant="outline" onClick={() => void openConsent(entry)}>
                  <Download className="size-4" aria-hidden />
                  インストール
                </Button>
              </li>
            ))}
          </ul>
        </div>
      ) : null}

      <Dialog open={consent !== null} onOpenChange={(open) => !open && setConsent(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              {consent ? `${consent.entry.name} v${consent.entry.version} をインストール` : ""}
            </DialogTitle>
            <DialogDescription>
              このアプリが要求する権限を確認してください。チェックした権限だけが付与されます
              （付与しない権限に依存する機能は動きません）。
            </DialogDescription>
          </DialogHeader>
          {/* スコープが多くても同意ボタンが画面外に出ないよう、一覧だけを内側でスクロールさせる。
              行全体を label にして、チェックボックスの外をタップしても切り替わるようにする。 */}
          <ul className="max-h-[min(18rem,45dvh)] divide-y divide-border overflow-y-auto overscroll-y-contain rounded-lg border">
            {consent?.requested.map((scope) => (
              <li key={scope}>
                <label className="flex min-h-11 cursor-pointer items-center gap-3 px-3 py-2 transition-colors hover:bg-accent/50">
                  <input
                    type="checkbox"
                    className="size-4 shrink-0 accent-primary"
                    checked={consent.granted.has(scope)}
                    onChange={(e) => {
                      const next = new Set(consent.granted);
                      if (e.target.checked) next.add(scope);
                      else next.delete(scope);
                      setConsent({ ...consent, granted: next });
                    }}
                  />
                  <span className="min-w-0 break-all font-mono text-sm">{scope}</span>
                </label>
              </li>
            ))}
          </ul>
          {/* アプリのトークンには、ゲートウェイが組織を決めるために所属グループの完全なパスが載る（Keycloak の groups
              のマッパー）。トークンはアプリのコードに渡るので、身元を読む権限を選ばなくても所属はアプリに見える。 */}
          {consent?.runsCode ? (
            <p className="text-[11px] leading-relaxed text-muted-foreground" data-testid="install-identity-note">
              使う人の所属する組織と部署・グループの名前は、選んだ権限にかかわらず、このアプリに渡ります。
            </p>
          ) : null}
          {consent && consent.tables.length > 0 ? (
            <div className="space-y-1.5" data-testid="install-submitters">
              <p className="text-sm font-medium">レコードを追加するだけの部署・グループ</p>
              <p className="text-[11px] leading-relaxed text-muted-foreground">
                選んだ部署・グループの人は、このアプリのテーブル（{consent.tables.map((n) => `「${n}」`).join("、")}）にレコードを追加できます。見られるのは、自分が追加したレコードと、個別に共有されたレコードだけです。
              </p>
              {/* アプリのテーブルはテーブルの共有で渡せない（付与・解除・一覧とも断る）。渡す口はインストールだけで、
                  アンインストールはアプリのテーブルをしまうので、入れ直すとたまったレコードを使えなくなる。 */}
              <p className="text-[11px] leading-relaxed text-[var(--season-autumn)]" data-testid="install-submitters-later">
                あとから足すには、アプリを入れ直す必要があります。入れ直すと、これまでのレコードは使えなくなります。
              </p>
              {consent.submitters.length > 0 ? (
                <ul className="flex flex-wrap gap-1" aria-label="レコードを追加するだけの部署・グループ">
                  {consent.submitters.map((id) => (
                    <li key={id} className="inline-flex max-w-full items-center gap-1 rounded-full border bg-card py-0.5 pl-2 pr-0.5 text-xs">
                      <span className="truncate">{cachedRoleName(id) || id}</span>
                      <button
                        type="button"
                        aria-label={`「${cachedRoleName(id) || id}」を外す`}
                        onClick={() => setConsent({ ...consent, submitters: consent.submitters.filter((x) => x !== id) })}
                        className="rounded-full p-0.5 text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring pointer-coarse:p-2"
                      >
                        <X className="size-3" aria-hidden />
                      </button>
                    </li>
                  ))}
                </ul>
              ) : null}
              <RefPicker
                field={ROLE_FIELD}
                value={[]}
                ariaLabel="レコードを追加するだけの部署・グループを探す"
                onChange={(ids, labels) => {
                  const id = ids[0];
                  if (!id || consent.submitters.includes(id)) return;
                  if (labels[id]) rememberRoleName(id, labels[id]);
                  setConsent({ ...consent, submitters: [...consent.submitters, id] });
                }}
              />
            </div>
          ) : null}
          <DialogFooter>
            <Button variant="outline" onClick={() => setConsent(null)}>
              キャンセル
            </Button>
            <Button onClick={() => void doInstall()} disabled={pending !== null}>
              {pending !== null ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
              同意してインストール
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(open) => !open && setRemoving(null)}
        title="アプリをアンインストールしますか？"
        description={
          removing ? `「${removing.app_name}」をアンインストールします。所有テーブルはアーカイブされます。` : undefined
        }
        confirmLabel="アンインストール"
        destructive
        onConfirm={async () => {
          if (removing) await doUninstall(removing);
        }}
      />
    </section>
  );
}
