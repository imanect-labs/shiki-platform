"use client";

import * as React from "react";

import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { PreviewNote, SettingsPage, SettingsRow, SettingsSection } from "./settings-ui";

/// 設定 → パーソナライズ（モック）。カスタム指示・記憶は未実装のため、操作は保存されない。
export function MockAiView() {
  const [about, setAbout] = React.useState("");
  const [style, setStyle] = React.useState("");
  const [tone, setTone] = React.useState("default");
  const [memory, setMemory] = React.useState(true);
  const [web, setWeb] = React.useState(true);

  return (
    <SettingsPage title="パーソナライズ" description="AI があなたに合わせて答えるための設定です。">
      <PreviewNote />
      <SettingsSection title="カスタム指示" description="すべての新しいチャットの最初に、AI へ伝えます。">
        <SettingsRow label="あなたについて" htmlFor="ai-about" description="職種・担当・よく扱う資料など。" stacked>
          <Textarea
            id="ai-about"
            value={about}
            onChange={(e) => setAbout(e.target.value)}
            placeholder="例: 法人営業。提案書と見積の作成が多い。"
            rows={3}
            className="w-full"
          />
        </SettingsRow>
        <SettingsRow label="答え方の希望" htmlFor="ai-style" stacked>
          <Textarea
            id="ai-style"
            value={style}
            onChange={(e) => setStyle(e.target.value)}
            placeholder="例: 結論を先に。社内文書を引用するときは出典を必ず付ける。"
            rows={3}
            className="w-full"
          />
        </SettingsRow>
        <SettingsRow label="口調">
          <Select value={tone} onValueChange={setTone}>
            <SelectTrigger className="h-9 w-full sm:w-48" aria-label="口調">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="default">標準</SelectItem>
              <SelectItem value="formal">丁寧・かしこまった</SelectItem>
              <SelectItem value="casual">くだけた</SelectItem>
              <SelectItem value="concise">簡潔</SelectItem>
            </SelectContent>
          </Select>
        </SettingsRow>
      </SettingsSection>
      <SettingsSection title="記憶と検索">
        <SettingsRow label="会話を記憶する" description="過去のチャットから好みや前提を覚え、次の回答に活かします。">
          <Switch checked={memory} onCheckedChange={setMemory} aria-label="会話を記憶する" />
        </SettingsRow>
        <SettingsRow label="記憶を消去" description="これまでに覚えた内容をすべて忘れます。">
          <Button variant="outline" size="sm" disabled>
            記憶を消去
          </Button>
        </SettingsRow>
        <SettingsRow label="Web 検索を既定で使う" description="新しいチャットで Web 検索を最初から有効にします。">
          <Switch checked={web} onCheckedChange={setWeb} aria-label="Web 検索を既定で使う" />
        </SettingsRow>
      </SettingsSection>
    </SettingsPage>
  );
}
