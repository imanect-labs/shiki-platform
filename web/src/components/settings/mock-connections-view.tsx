"use client";

import { Cloud, GitBranch, MessageSquare } from "lucide-react";

import { AdminPage, Panel } from "@/components/admin/admin-page";
import { MockBanner } from "@/components/admin/mock-banner";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";

/// サンプル（実在の接続ではない）。
const APPS = [
  { key: "drive", name: "Google ドライブ", body: "ドライブの文書をチャットで参照する", icon: Cloud, connected: true, since: "2026/09/12" },
  { key: "slack", name: "Slack", body: "チャンネルの会話を検索・要約する", icon: MessageSquare, connected: false },
  { key: "github", name: "GitHub", body: "Issue や Pull Request を参照する", icon: GitBranch, connected: false },
];

/// 設定 → 接続済みアプリ（モック）。外部サービス連携は未実装のため、操作は保存されない。
export function MockConnectionsView() {
  return (
    <AdminPage title="接続済みアプリ" description="あなたの権限で shiki が使う外部サービスです。">
      <MockBanner what="外部サービス連携は未実装" />
      <Panel title="サービス">
        <ul className="divide-y">
          {APPS.map((a) => {
            const Icon = a.icon;
            return (
              <li key={a.key} className="flex items-center gap-3 px-4 py-3.5">
                <span
                  aria-hidden
                  className="flex size-10 shrink-0 items-center justify-center rounded-lg bg-muted text-foreground/70"
                >
                  <Icon className="size-5" />
                </span>
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-2 text-[13.5px] font-medium">
                    {a.name}
                    {a.connected ? <Badge variant="success">接続中</Badge> : null}
                  </div>
                  <div className="text-xs text-muted-foreground">
                    {a.connected ? `${a.since} に接続 ・ ${a.body}` : a.body}
                  </div>
                </div>
                <Button variant="outline" size="sm" disabled>
                  {a.connected ? "接続を解除" : "接続"}
                </Button>
              </li>
            );
          })}
        </ul>
      </Panel>
    </AdminPage>
  );
}
