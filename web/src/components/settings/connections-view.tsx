"use client";

import { Cloud, GitBranch, MessageSquare } from "lucide-react";

import { Button } from "@/components/ui/button";
import { ChatGptConnectionSection } from "./chatgpt-connection";
import { IconTile, PreviewNote, SettingsItem, SettingsPage, SettingsSection, StatusDot } from "./settings-ui";

/// サンプル（実在の接続ではない）。
const APPS = [
  { key: "drive", name: "Google ドライブ", body: "ドライブの文書をチャットで参照する", icon: Cloud, connected: true, since: "2026/09/12" },
  { key: "slack", name: "Slack", body: "チャンネルの会話を検索・要約する", icon: MessageSquare, connected: false },
  { key: "github", name: "GitHub", body: "Issue や Pull Request を参照する", icon: GitBranch, connected: false },
];

/// 設定 → 接続済みアプリ。
///
/// ChatGPT プラン（#854）は実際に接続できる（その環境で有効なときだけ出る）。外部サービス連携は
/// 未実装のため「サービス」はモックで、操作は保存されない。
export function ConnectionsView() {
  return (
    <SettingsPage title="接続済みアプリ" description="あなたの権限で Shiki が使う外部サービスです。">
      <ChatGptConnectionSection />
      <div className="flex flex-col gap-3">
        <PreviewNote />
        <SettingsSection title="サービス">
          <ul>
            {APPS.map((a) => (
              <SettingsItem
                key={a.key}
                icon={<IconTile icon={a.icon} />}
                title={
                  <>
                    {a.name}
                    {a.connected ? <StatusDot>接続中</StatusDot> : null}
                  </>
                }
                meta={a.connected ? `${a.since} に接続 ・ ${a.body}` : a.body}
                action={
                  <Button variant="outline" size="sm" disabled>
                    {a.connected ? "接続を解除" : "接続"}
                  </Button>
                }
              />
            ))}
          </ul>
        </SettingsSection>
      </div>
    </SettingsPage>
  );
}
