"use client";

import * as React from "react";

import { SegmentedControl } from "@/components/ui/segmented-control";
import { Switch } from "@/components/ui/switch";
import { PreviewNote, SettingsPage, SettingsRow, SettingsSection } from "./settings-ui";

const EVENTS = [
  { key: "share", label: "ファイルやノートが共有されたとき", email: true, inApp: true },
  { key: "mention", label: "メンション・コメントされたとき", email: true, inApp: true },
  { key: "approval", label: "エージェントが承認を求めたとき", email: false, inApp: true },
  { key: "workflow", label: "ワークフローの実行が失敗したとき", email: true, inApp: true },
  { key: "report", label: "長時間のリサーチが完了したとき", email: false, inApp: true },
];

/// 設定 → 通知（モック）。通知基盤は未実装のため、操作は保存されない。
export function MockNotificationsView() {
  const [state, setState] = React.useState(() =>
    Object.fromEntries(EVENTS.map((e) => [e.key, { email: e.email, inApp: e.inApp }])),
  );
  const [digest, setDigest] = React.useState("daily");
  const toggle = (key: string, channel: "email" | "inApp", value: boolean) =>
    setState((s) => ({ ...s, [key]: { ...s[key], [channel]: value } }));

  return (
    <SettingsPage title="通知" description="どの出来事を、どこで知らせるかを選びます。">
      <PreviewNote />
      <SettingsSection title="通知する出来事">
        <div className="hidden grid-cols-[minmax(0,1fr)_5rem_5rem] px-5 pb-2 pt-3.5 text-xs text-muted-foreground md:grid">
          <span>出来事</span>
          <span className="text-center">アプリ内</span>
          <span className="text-center">メール</span>
        </div>
        <ul>
          {EVENTS.map((e) => (
            <li
              key={e.key}
              className="shiki-dash-top grid grid-cols-[minmax(0,1fr)_auto] items-center gap-3 px-5 py-3.5 max-md:first:bg-none md:grid-cols-[minmax(0,1fr)_5rem_5rem]"
            >
              <span className="text-[13.5px]">{e.label}</span>
              <span className="flex items-center justify-center gap-2 max-md:justify-end">
                <span className="text-xs text-muted-foreground md:hidden">アプリ内</span>
                <Switch
                  checked={state[e.key].inApp}
                  onCheckedChange={(v) => toggle(e.key, "inApp", v)}
                  aria-label={`${e.label}（アプリ内）`}
                />
              </span>
              <span className="flex items-center justify-center gap-2 max-md:col-start-2 max-md:justify-end">
                <span className="text-xs text-muted-foreground md:hidden">メール</span>
                <Switch
                  checked={state[e.key].email}
                  onCheckedChange={(v) => toggle(e.key, "email", v)}
                  aria-label={`${e.label}（メール）`}
                />
              </span>
            </li>
          ))}
        </ul>
      </SettingsSection>
      <SettingsSection title="メールのまとめ">
        <SettingsRow label="送る頻度" description="すぐに送らない通知を、まとめて 1 通にします。">
          <SegmentedControl
            aria-label="まとめの頻度"
            size="sm"
            value={digest}
            onValueChange={setDigest}
            options={[
              { value: "none", label: "まとめない" },
              { value: "daily", label: "毎日" },
              { value: "weekly", label: "毎週" },
            ]}
          />
        </SettingsRow>
      </SettingsSection>
    </SettingsPage>
  );
}
