"use client";

/// インストール済みコードアプリ（B1/B2）セクション（Task 9.11/9.13b）。
///
/// A（宣言的 mini_app）と同じ「アプリ」ページに載せ、同一シェルから一覧・起動できるようにする。
/// インストールはレジストリ（publish 済みエントリ）から**要求スコープを見て同意**する。アプリがテーブルを持つときは、
/// テーブルに「追加だけ」（レコードを追加し、自分が追加したレコードだけ見られる・D-113）を渡す部署・グループを選べる
/// （GAP-53・インストールの `submitter_roles`）。インストールの後は、カードの「使える部署・グループ」から、アプリの
/// テーブルを使える部署・グループ（閲覧・編集・追加だけ）を足す・外す（GAP-68）。
///
/// アプリの持ち主と組織の管理者は、公開を取り下げられる（GAP-69b）。取り下げると新しくインストールできなくなり、
/// 一覧から消える。入れてあるアプリは動き続け、カードに取り下げたことを出す。

import * as React from "react";
import Link from "next/link";
import { AlertTriangle, Download, EyeOff, Info, Loader2, Package, Play, Trash2, Users, X } from "lucide-react";

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
import { ListLoadFailed } from "@/components/artifacts/shared-more";
import { RefPicker } from "@/components/data/ref-picker";
import { AppTableRolesDialog } from "@/components/miniapp/app-table-roles-dialog";
import type { CapabilityScope } from "@/generated/authz-vocab";
import type { FieldDef } from "@/lib/data-api";
import { cachedRoleName, rememberRoleName } from "@/lib/role-names";
import { formatDateTime } from "@/lib/format";
import {
  fetchManifest,
  installApp,
  listInstallations,
  listRegistry,
  uninstallApp,
  yankRegistryEntry,
  type AppFunctionFailure,
  type AppInstallation,
  type RegistryEntry,
} from "@/lib/miniapp-b1-api";

/// 部署・グループを探して選ぶ（ディレクトリの部署・`role_ref` の列と同じ探し方）。
/// 使う人の所属を読む権限（ゲートウェイが所属するロールを返すのは、これを付けたときだけ・D-130）。
const IDENTITY_SCOPE: CapabilityScope = "identity.read";

const ROLE_FIELD: FieldDef = { key: "role", display_name: "部署・グループ", type: "role_ref" } as FieldDef;

/// 自動実行のきっかけの呼び名（サーバーの `trigger`）。
const TRIGGER_LABEL: Record<string, string> = {
  event: "テーブルの変更など",
  cron: "スケジュール",
};

/// 取り下げの失敗を、利用者に分かる言葉にする（ダイアログの中に出す）。
function yankFailure(e: unknown): Error {
  const status = (e as { status?: number } | null)?.status;
  if (status === 403) {
    return new Error("取り下げられるのは、アプリの持ち主と組織の管理者だけです。");
  }
  if (status === 404) {
    return new Error("この公開は見つかりません。一覧を読み込み直してください。");
  }
  return new Error(`取り下げられませんでした（${e instanceof Error ? e.message : String(e)}）。`);
}

/// 自動実行の関数の直近の失敗（アプリの持ち主にだけ届く・GAP-60）。次に成功すると消える。
function FunctionFailures({ failures }: { failures: AppFunctionFailure[] }) {
  return (
    <div
      role="status"
      data-testid="app-function-failures"
      className="flex gap-2 rounded-md border border-destructive/40 bg-destructive/5 p-2.5 text-xs"
    >
      <AlertTriangle className="mt-0.5 size-4 shrink-0 text-destructive" aria-hidden />
      <div className="min-w-0 space-y-1.5">
        <p className="font-medium text-destructive">自動実行が失敗しています</p>
        <ul className="space-y-1.5">
          {failures.map((f) => (
            <li key={f.function} className="min-w-0">
              <p className="break-words font-medium text-foreground">
                「{f.function}」（{TRIGGER_LABEL[f.trigger] ?? "自動実行"}）
              </p>
              <p className="break-words text-muted-foreground">{f.message}</p>
              <p className="text-muted-foreground">
                <span className="whitespace-nowrap">{formatDateTime(f.failed_at)}</span>
                {f.failures > 1 ? <span className="whitespace-nowrap">・{f.failures} 回続けて失敗</span> : null}
              </p>
            </li>
          ))}
        </ul>
        <p className="text-muted-foreground">次に成功すると、この表示は消えます。</p>
      </div>
    </div>
  );
}

export function InstalledAppsSection() {
  const [installed, setInstalled] = React.useState<AppInstallation[] | null>(null);
  const [registry, setRegistry] = React.useState<RegistryEntry[]>([]);
  // 読み込みの失敗を「ありません」や欄が無いことに見せない（付録 C の P5・BUG-189）。
  const [installedFailed, setInstalledFailed] = React.useState(false);
  const [registryFailed, setRegistryFailed] = React.useState(false);
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
  /// 公開を取り下げようとしている登録（確かめのダイアログ・GAP-69b）。閉じる動きの間も文言を保つよう、閉じても残す。
  const [yanking, setYanking] = React.useState<RegistryEntry | null>(null);
  const [yankOpen, setYankOpen] = React.useState(false);
  const askYank = (entry: RegistryEntry) => {
    setYanking(entry);
    setYankOpen(true);
  };
  /// テーブルを使える部署・グループを開いているアプリ（GAP-68）。閉じる動きの間も題を保つよう、アプリは閉じても残す。
  const [roles, setRoles] = React.useState<AppInstallation | null>(null);
  const [rolesOpen, setRolesOpen] = React.useState(false);

  const reload = React.useCallback(() => {
    listInstallations()
      .then((items) => {
        setInstalled(items);
        setInstalledFailed(false);
      })
      .catch(() => {
        setInstalled([]);
        setInstalledFailed(true);
      });
    listRegistry()
      .then((items) => {
        setRegistry(items);
        setRegistryFailed(false);
      })
      .catch(() => {
        setRegistry([]);
        setRegistryFailed(true);
      });
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

  /// 公開を取り下げる（確かめのダイアログから呼ぶ。失敗はダイアログ内に出す）。
  const doYank = async (entry: RegistryEntry) => {
    try {
      await yankRegistryEntry(entry.id);
    } catch (e) {
      throw yankFailure(e);
    }
    toast({ title: `「${entry.name}」v${entry.version} の公開を取り下げました` });
    reload();
  };

  /// 入れた版の公開（まだ取り下げていないもの）。カードから取り下げる口を出すのに使う。
  const publishedOf = (app: AppInstallation) =>
    registry.find(
      (r) => r.artifact_id === app.app_id && r.name === app.app_name && r.version === app.installed_version,
    );
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
      ) : installedFailed ? (
        <ListLoadFailed
          what="インストール済みのアプリ"
          className="rounded-lg border border-dashed py-6"
          onRetry={reload}
        />
      ) : installed.length === 0 ? (
        <p className="rounded-lg border border-dashed px-4 py-6 text-center text-sm text-muted-foreground">
          インストール済みのコードアプリはありません。
        </p>
      ) : (
        <ul className="grid gap-3 sm:grid-cols-2" aria-label="インストール済みのアプリ">
          {installed.map((app) => (
            <li key={app.id} className="flex flex-col gap-2 rounded-xl border border-border bg-card p-4">
              <div className="min-w-0">
                <h3 className="truncate text-[15px] font-semibold">{app.app_name}</h3>
                <p className="text-xs text-muted-foreground">
                  v{app.installed_version}・スコープ {app.granted_scopes.length} 件
                </p>
              </div>
              {app.oauth_client_missing ? (
                // 起動時の照合が Keycloak にアプリの登録を作り直せなかった（GAP-70）。入れ直すとテーブルが新しくなる。
                <div
                  role="status"
                  className="flex gap-2 rounded-md border border-destructive/40 bg-destructive/5 p-2.5 text-xs"
                >
                  <AlertTriangle className="mt-0.5 size-4 shrink-0 text-destructive" aria-hidden />
                  <div className="min-w-0">
                    <p className="font-medium text-destructive">このアプリは動かない状態です</p>
                    <p className="mt-0.5 text-muted-foreground">
                      アプリの登録が見つかりません。入れ直すと直りますが、これまでのレコードは使えなくなります。
                    </p>
                  </div>
                </div>
              ) : app.oauth_client_disabled ? (
                // 管理者が Keycloak で B2 を無効にしていて、起動時の照合が有効に戻さなかった（漏れた secret を止めたのかもしれ
                // ないので、同じ secret のまま戻さない・BUG-201）。入れ直すと同じ secret で戻るので、入れ直しは勧めない。
                <div
                  role="status"
                  data-testid="app-client-disabled"
                  className="flex gap-2 rounded-md border border-destructive/40 bg-destructive/5 p-2.5 text-xs"
                >
                  <AlertTriangle className="mt-0.5 size-4 shrink-0 text-destructive" aria-hidden />
                  <div className="min-w-0">
                    <p className="font-medium text-destructive">このアプリのサーバーで動く処理が止まっています</p>
                    <p className="mt-0.5 text-muted-foreground">
                      管理者がアプリの登録を止めています。動かすには、管理者に確かめてください。
                    </p>
                  </div>
                </div>
              ) : null}
              {app.function_failures && app.function_failures.length > 0 ? (
                <FunctionFailures failures={app.function_failures} />
              ) : null}
              {app.yanked ? (
                // 入れた版の公開を取り下げた（GAP-69b）。入れたアプリは動き続けるので、注意の色にしない。
                <div
                  role="status"
                  data-testid="app-yanked"
                  className="flex gap-2 rounded-md border border-border bg-muted/50 p-2.5 text-xs"
                >
                  <Info className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
                  <div className="min-w-0">
                    <p className="font-medium text-foreground">この版は公開を取り下げました</p>
                    <p className="mt-0.5 text-muted-foreground">
                      このまま使えますが、新しくインストールすることはできません。
                    </p>
                  </div>
                </div>
              ) : null}
              <div className="mt-auto flex flex-wrap items-center gap-1 pt-1">
                {app.frontend_bundle ? (
                  <Button size="sm" asChild>
                    <Link href={`/apps/b1/${app.app_id}`}>
                      <Play className="size-4" aria-hidden />
                      開く
                    </Link>
                  </Button>
                ) : (
                  // 画面を持たないアプリ（サーバーで動く処理だけ）は開けない。asChild の Link には disabled が効かない
                  // ので、押せないボタンにして理由を添える（BUG-203）。
                  <>
                    <Button size="sm" disabled aria-describedby={`no-screen-${app.id}`}>
                      <Play className="size-4" aria-hidden />
                      開く
                    </Button>
                    <span id={`no-screen-${app.id}`} className="px-1 text-xs text-muted-foreground">
                      画面のないアプリです
                    </span>
                  </>
                )}
                {(app.table_count ?? 0) > 0 ? (
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => {
                      setRoles(app);
                      setRolesOpen(true);
                    }}
                    aria-label={`「${app.app_name}」のテーブルを使える部署・グループ`}
                  >
                    <Users className="size-4" aria-hidden />
                    使える部署・グループ
                  </Button>
                ) : null}
                {publishedOf(app)?.can_yank ? (
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={() => {
                      const entry = publishedOf(app);
                      if (entry) askYank(entry);
                    }}
                    aria-label={`「${app.app_name}」v${app.installed_version} の公開を取り下げる`}
                  >
                    <EyeOff className="size-4" aria-hidden />
                    公開を取り下げ
                  </Button>
                ) : null}
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

      {registryFailed ? (
        <ListLoadFailed what="インストールできるアプリの一覧" compact className="mt-6" onRetry={reload} />
      ) : null}
      {installable.length > 0 ? (
        <div className="mt-6">
          <h3 className="mb-2 text-sm font-semibold text-muted-foreground">
            <Package className="mr-1 inline size-4" aria-hidden />
            レジストリからインストール
          </h3>
          <ul className="grid gap-2 sm:grid-cols-2" aria-label="インストールできるアプリ">
            {installable.map((entry) => (
              <li
                key={entry.id}
                className="flex items-center justify-between gap-2 rounded-lg border px-3 py-2.5"
              >
                <div className="min-w-0">
                  <p className="truncate text-sm font-medium" title={entry.name}>
                    {entry.name}
                  </p>
                  <p className="text-xs text-muted-foreground">
                    v{entry.version}・{entry.trust_tier === "first_party" ? "署名済み" : "社内"}
                  </p>
                </div>
                <div className="flex shrink-0 flex-wrap items-center justify-end gap-1">
                  {entry.can_yank ? (
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => askYank(entry)}
                      aria-label={`「${entry.name}」v${entry.version} の公開を取り下げる`}
                    >
                      <EyeOff className="size-4" aria-hidden />
                      取り下げ
                    </Button>
                  ) : null}
                  <Button size="sm" variant="outline" onClick={() => void openConsent(entry)}>
                    <Download className="size-4" aria-hidden />
                    インストール
                  </Button>
                </div>
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
          {/* アプリのトークンはアプリのコードに渡る。載るのは使う人（sub）と契約全体（tenant）の ID だけで、所属グループは
              載せない（ゲートウェイは組織をインストールから決める・D-130）。所属するロールは identity.read を付けたときだけ
              ゲートウェイが返す。権限は上の一覧と同じ名前で言う（一覧に表示名が付くまで・FEAT-3 の §9-21）。 */}
          {consent?.runsCode ? (
            <p className="text-[11px] leading-relaxed text-muted-foreground" data-testid="install-identity-note">
              選んだ権限にかかわらず、使う人を見分ける ID と、契約全体を見分ける ID は、このアプリに渡ります。
              {consent.requested.includes(IDENTITY_SCOPE)
                ? `所属する部署・グループは、${IDENTITY_SCOPE} を付けたときだけ渡ります。`
                : "所属する部署・グループは渡りません。"}
            </p>
          ) : null}
          {consent && consent.tables.length > 0 ? (
            <div className="space-y-1.5" data-testid="install-submitters">
              <p className="text-sm font-medium">レコードを追加するだけの部署・グループ</p>
              <p className="text-[11px] leading-relaxed text-muted-foreground">
                選んだ部署・グループの人は、このアプリのテーブル（{consent.tables.map((n) => `「${n}」`).join("、")}）にレコードを追加できます。見られるのは、自分が追加したレコードと、個別に共有されたレコードだけです。
              </p>
              {/* アプリのテーブルはテーブルの共有では渡せない。インストールの後は、カードの「使える部署・グループ」で足す・
                  外す（GAP-68・PATCH /apps/installations/{app_id}/roles）。 */}
              <p className="text-[11px] leading-relaxed text-muted-foreground" data-testid="install-submitters-later">
                インストールの後も、アプリの「使える部署・グループ」で足したり外したりできます。
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
      <AppTableRolesDialog
        open={rolesOpen && roles !== null}
        onOpenChange={setRolesOpen}
        appId={roles?.app_id ?? ""}
        appName={roles?.app_name ?? ""}
      />
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
      <ConfirmDialog
        open={yankOpen && yanking !== null}
        onOpenChange={setYankOpen}
        title="公開を取り下げますか？"
        description={
          yanking
            ? `「${yanking.name}」v${yanking.version} の公開を取り下げます。新しくインストールできなくなり、一覧から消えます。入れてあるアプリは動き続けます。`
            : undefined
        }
        confirmLabel="取り下げる"
        destructive
        onConfirm={async () => {
          if (yanking) await doYank(yanking);
        }}
      />
    </section>
  );
}
