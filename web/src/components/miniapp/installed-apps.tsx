"use client";

/// インストール済みコードアプリ（B1/B2）セクション（Task 9.11/9.13b）。
///
/// A（宣言的 mini_app）と同じ「アプリ」ページに載せ、同一シェルから一覧・起動できるようにする。
/// インストールはレジストリ（publish 済みエントリ）から**要求スコープを見て同意**する。

import * as React from "react";
import Link from "next/link";
import { Download, Loader2, Package, Play, Trash2 } from "lucide-react";

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
import {
  fetchManifest,
  installApp,
  listInstallations,
  listRegistry,
  uninstallApp,
  type AppInstallation,
  type RegistryEntry,
} from "@/lib/miniapp-b1-api";

export function InstalledAppsSection() {
  const [installed, setInstalled] = React.useState<AppInstallation[] | null>(null);
  const [registry, setRegistry] = React.useState<RegistryEntry[]>([]);
  const [consent, setConsent] = React.useState<{
    entry: RegistryEntry;
    requested: string[];
    granted: Set<string>;
  } | null>(null);
  const [pending, setPending] = React.useState<string | null>(null);

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
      setConsent({
        entry,
        requested: manifest.requested_scopes,
        granted: new Set(manifest.requested_scopes),
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

  const doUninstall = async (app: AppInstallation) => {
    if (!window.confirm(`「${app.app_name}」をアンインストールしますか？（所有テーブルはアーカイブされます）`))
      return;
    setPending(app.app_id);
    try {
      await uninstallApp(app.app_id);
      toast({ title: "アンインストールしました" });
      reload();
    } catch (e) {
      toast({
        variant: "destructive",
        title: "アンインストールに失敗しました",
        description: e instanceof Error ? e.message : String(e),
      });
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
      <div className="mb-3 flex items-center justify-between">
        <div>
          <h2 className="text-base font-semibold">インストール済みアプリ（コード）</h2>
          <p className="text-xs text-muted-foreground">
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
                  onClick={() => void doUninstall(app)}
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
                className="flex items-center justify-between gap-2 rounded-lg border px-3 py-2"
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
          <ul className="space-y-2 py-2">
            {consent?.requested.map((scope) => (
              <li key={scope} className="flex items-center gap-2">
                <input
                  id={`scope-${scope}`}
                  type="checkbox"
                  className="size-4 accent-primary"
                  checked={consent.granted.has(scope)}
                  onChange={(e) => {
                    const next = new Set(consent.granted);
                    if (e.target.checked) next.add(scope);
                    else next.delete(scope);
                    setConsent({ ...consent, granted: next });
                  }}
                />
                <label htmlFor={`scope-${scope}`} className="font-mono text-sm">
                  {scope}
                </label>
              </li>
            ))}
          </ul>
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
    </section>
  );
}
