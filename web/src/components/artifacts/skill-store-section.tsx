"use client";

/// スキルストア（テナントのレジストリ一覧＋本人の同意インストール・#344 Task 10.11）。
///
/// インストール＝「自分のカタログへ載せる」明示行為。インストール済みスキルは
/// チャットの skill ツール一覧（カタログ）に載り、ワークフローの skill ノードから参照できる。

import * as React from "react";
import { Download, Loader2, PackageCheck, ShieldCheck, Store } from "lucide-react";

import { Button } from "@/components/ui/button";
import { toast } from "@/components/ui/use-toast";
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

  const toggle = async (entry: SkillRegistryEntry) => {
    setPending(entry.name);
    try {
      const current = installedByName.get(entry.name);
      if (current && current.registryVersion === entry.version) {
        await uninstallSkill(entry.name);
        toast({ title: `「${entry.name}」を外しました` });
      } else if (current) {
        // 旧バージョンをインストール済み → 最新へ更新（アンインストールさせない・レビュー指摘）。
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

  if (entries === null || latest.length === 0) return null;

  return (
    <section className="mt-8">
      <div className="mb-3 flex items-center gap-2">
        <Store className="size-4 text-primary" aria-hidden />
        <h2 className="text-[15px] font-semibold">スキルストア</h2>
        <p className="text-xs text-muted-foreground">
          インストールすると自分のチャット/ワークフローから使えます。
        </p>
      </div>
      <ul className="grid gap-2 sm:grid-cols-2">
        {latest.map((entry) => {
          const current = installedByName.get(entry.name);
          const isInstalled = current !== undefined;
          const isOutdated = current !== undefined && current.registryVersion !== entry.version;
          return (
            <li
              key={entry.name}
              className="flex items-center justify-between gap-2 rounded-xl border border-border bg-card px-4 py-3"
            >
              <div className="min-w-0">
                <div className="flex items-center gap-1.5">
                  <span className="truncate text-sm font-medium">{entry.name}</span>
                  {entry.trustTier === "first_party" ? (
                    <span
                      className="inline-flex items-center gap-0.5 rounded-full bg-primary/10 px-1.5 py-0.5 text-[11px] text-primary"
                      title="署名検証済みの公式スキル"
                    >
                      <ShieldCheck className="size-3" aria-hidden />
                      公式
                    </span>
                  ) : null}
                </div>
                <p className="text-xs text-muted-foreground">v{entry.version}</p>
              </div>
              <Button
                size="sm"
                variant={isInstalled && !isOutdated ? "ghost" : "secondary"}
                onClick={() => void toggle(entry)}
                disabled={pending === entry.name}
              >
                {pending === entry.name ? (
                  <Loader2 className="size-4 animate-spin" aria-hidden />
                ) : isOutdated ? (
                  <Download className="size-4" aria-hidden />
                ) : isInstalled ? (
                  <PackageCheck className="size-4" aria-hidden />
                ) : (
                  <Download className="size-4" aria-hidden />
                )}
                {isOutdated
                  ? `v${entry.version} へ更新`
                  : isInstalled
                    ? "インストール済み"
                    : "インストール"}
              </Button>
            </li>
          );
        })}
      </ul>
    </section>
  );
}
