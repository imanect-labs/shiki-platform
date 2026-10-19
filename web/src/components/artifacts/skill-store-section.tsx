"use client";

/// スキルストア（テナントのレジストリ一覧＋本人の同意インストール・#344 Task 10.11）。
///
/// インストール＝「自分のカタログへ載せる」明示行為。インストール済みスキルは
/// チャットの skill ツール一覧（カタログ）に載り、ワークフローの skill ノードから参照できる。
///
/// 「インストール済み」は状態表示（押しても何も起きない）で、外す操作は別ボタン＋確認に分ける
/// （状態ラベルを押しただけでアンインストールされる事故を防ぐ）。

import * as React from "react";
import { Download, Loader2, PackageCheck, ShieldCheck, Store } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { toast } from "@/components/ui/use-toast";
import { ConfirmDialog } from "@/components/drive/dialogs";
import {
  installSkill,
  listSkillInstallations,
  listSkillRegistry,
  uninstallSkill,
  type SkillInstallation,
  type SkillRegistryEntry,
} from "@/lib/skill-registry-api";

export function SkillStoreSection() {
  const [entries, setEntries] = React.useState<SkillRegistryEntry[] | null>(null);
  const [installed, setInstalled] = React.useState<SkillInstallation[]>([]);
  const [pending, setPending] = React.useState<string | null>(null);
  const [removing, setRemoving] = React.useState<string | null>(null);

  const reload = React.useCallback(() => {
    Promise.all([listSkillRegistry(), listSkillInstallations()])
      .then(([e, i]) => {
        setEntries(e);
        setInstalled(i);
      })
      .catch(() => setEntries([]));
  }, []);
  React.useEffect(reload, [reload]);

  // 同名は最新公開のみ表示（レジストリは不変 publish の履歴を持つ）。
  const latest = React.useMemo(() => {
    const seen = new Set<string>();
    return (entries ?? []).filter((e) => {
      if (e.yanked || seen.has(e.name)) return false;
      seen.add(e.name);
      return true;
    });
  }, [entries]);

  const installedByName = React.useMemo(
    () => new Map(installed.map((i) => [i.name, i])),
    [installed],
  );

  /// インストール／最新版への更新（旧版を入れているときは外さずに更新する）。
  const install = async (entry: SkillRegistryEntry) => {
    setPending(entry.name);
    try {
      const current = installedByName.get(entry.name);
      if (current) {
        await installSkill(entry.name, entry.version);
        toast({ title: `「${entry.name}」を v${entry.version} へ更新しました` });
      } else {
        await installSkill(entry.name);
        toast({ title: `「${entry.name}」をインストールしました`, description: "チャットのスキル一覧に載ります。" });
      }
      reload();
    } catch (e) {
      toast({
        variant: "destructive",
        title: "操作に失敗しました",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setPending(null);
    }
  };

  /// 外す（確認ダイアログから呼ぶ。失敗はダイアログ内に出す）。
  const uninstall = async (name: string) => {
    setPending(name);
    try {
      await uninstallSkill(name);
      toast({ title: `「${name}」を外しました` });
      reload();
    } finally {
      setPending(null);
    }
  };

  if (entries === null || latest.length === 0) return null;

  return (
    <section id="skill-store" className="mt-8 scroll-mt-20">
      <div className="mb-3">
        <h2 className="flex items-center gap-2 text-[15px] font-semibold">
          <Store className="size-4 text-primary" aria-hidden />
          スキルストア
        </h2>
        <p className="mt-0.5 text-xs text-muted-foreground">
          インストールすると自分のチャット/ワークフローから使えます。
        </p>
      </div>
      <ul className="grid gap-2 sm:grid-cols-2">
        {latest.map((entry) => {
          const current = installedByName.get(entry.name);
          const isInstalled = current !== undefined;
          const isOutdated = current !== undefined && current.registryVersion !== entry.version;
          const busy = pending === entry.name;
          return (
            <li
              key={entry.name}
              className="flex flex-wrap items-center justify-between gap-x-2 gap-y-2 rounded-xl border border-border bg-card px-4 py-3"
            >
              <div className="min-w-0">
                <div className="flex flex-wrap items-center gap-1.5">
                  <span className="truncate text-sm font-medium">{entry.name}</span>
                  {entry.trustTier === "first_party" ? (
                    <span className="inline-flex items-center gap-0.5 rounded-full bg-primary/10 px-1.5 py-0.5 text-[11px] text-primary">
                      <ShieldCheck className="size-3" aria-hidden />
                      公式（署名検証済み）
                    </span>
                  ) : null}
                </div>
                <p className="mt-0.5 flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
                  v{entry.version}
                  {isInstalled ? (
                    <Badge variant="success" className="px-1.5 py-0 text-[11px]">
                      <PackageCheck className="size-3" aria-hidden />
                      {isOutdated ? `v${current?.registryVersion} をインストール済み` : "インストール済み"}
                    </Badge>
                  ) : null}
                </p>
              </div>
              <div className="ml-auto flex shrink-0 items-center gap-1">
                {busy ? <Loader2 className="size-4 animate-spin text-muted-foreground" aria-hidden /> : null}
                {isInstalled ? (
                  <Button
                    size="sm"
                    variant="ghost"
                    className="text-muted-foreground hover:text-destructive"
                    onClick={() => setRemoving(entry.name)}
                    disabled={busy}
                  >
                    外す
                  </Button>
                ) : null}
                {!isInstalled || isOutdated ? (
                  <Button size="sm" variant="secondary" onClick={() => void install(entry)} disabled={busy}>
                    <Download className="size-4" aria-hidden />
                    {isOutdated ? `v${entry.version} へ更新` : "インストール"}
                  </Button>
                ) : null}
              </div>
            </li>
          );
        })}
      </ul>
      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(open) => !open && setRemoving(null)}
        title="スキルを外しますか？"
        description={
          removing
            ? `「${removing}」をアンインストールします。チャットのスキル一覧とワークフローから使えなくなります（ストアからいつでも入れ直せます）。`
            : undefined
        }
        confirmLabel="外す"
        destructive
        onConfirm={async () => {
          if (removing) await uninstall(removing);
        }}
      />
    </section>
  );
}
