"use client";

/// 設定 → メッセージのエージェント（パーソナルエージェントの名前とアイコン・design §4.14）。
///
/// 名前とアイコンは**見た目だけ**で、権限・承認・使える道具には影響しない。紛らわしい名前（実在の利用者名・
/// 「シキ」「管理者」に似た名前）はサーバが断る。エージェントの発言には必ず「〇〇 のエージェント」と
/// 持ち主が並ぶ。

import * as React from "react";
import { Bot, ImagePlus, ShieldCheck, Trash2 } from "lucide-react";

import { useMe } from "@/hooks/use-me";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { toast } from "@/components/ui/use-toast";
import {
  agentIconUrl,
  DEFAULT_AGENT_NAME,
  deleteAgentIcon,
  getAgentProfile,
  MessagingError,
  setAgentIcon,
  setAgentName,
  type AgentProfileView,
} from "@/lib/messaging-api";
import { SettingsPage, SettingsRow, SettingsSection } from "./settings-ui";

/// アイコンの上限（サーバの `MAX_ICON_BYTES`・正はサーバの検査）。
const MAX_ICON_BYTES = 256 * 1024;
const ICON_TYPES = ["image/png", "image/jpeg", "image/webp"];

function AgentIcon({ userId, hasIcon, version }: { userId: string; hasIcon: boolean; version: number }) {
  const [broken, setBroken] = React.useState(false);
  React.useEffect(() => setBroken(false), [version]);
  if (hasIcon && userId && !broken) {
    return (
      // eslint-disable-next-line @next/next/no-img-element -- 認証付きの API 画像（next/image の最適化を通さない）。
      <img
        src={agentIconUrl(userId, version)}
        alt="エージェントのアイコン"
        onError={() => setBroken(true)}
        className="size-14 rounded-[14px] border object-cover"
      />
    );
  }
  return (
    <span
      className="flex size-14 items-center justify-center rounded-[14px] text-primary-foreground"
      style={{ backgroundImage: "linear-gradient(140deg, var(--primary), color-mix(in oklab, var(--primary) 70%, var(--season-autumn)))" }}
      aria-label="既定のアイコン"
      role="img"
    >
      <Bot className="size-7" strokeWidth={2.25} aria-hidden />
    </span>
  );
}

export function AgentProfileSettingsView() {
  const { data: me } = useMe();
  const [profile, setProfile] = React.useState<AgentProfileView | null>(null);
  const [loadError, setLoadError] = React.useState<string | null>(null);
  const [name, setName] = React.useState("");
  const [nameError, setNameError] = React.useState<string | null>(null);
  const [saving, setSaving] = React.useState<"name" | "icon" | "remove" | null>(null);
  const [version, setVersion] = React.useState(() => Date.now());
  const fileInput = React.useRef<HTMLInputElement>(null);

  const apply = (p: AgentProfileView) => {
    setProfile(p);
    setName(p.is_default ? "" : p.name);
  };

  React.useEffect(() => {
    getAgentProfile()
      .then(apply)
      .catch((e: unknown) => setLoadError(e instanceof Error ? e.message : String(e)));
  }, []);

  const saveName = async () => {
    setSaving("name");
    setNameError(null);
    try {
      apply(await setAgentName(name.trim()));
      toast({ title: "エージェントの名前を保存しました" });
    } catch (e) {
      // サーバの理由（紛らわしい名前など）は本文に載らないことがあるので、400 は案内を出す。
      setNameError(
        e instanceof MessagingError && e.status === 400 && e.message === "入力を受け付けられませんでした"
          ? "この名前は使えません。実在の利用者名や「シキ」「管理者」と紛らわしい名前は付けられません。"
          : e instanceof Error
            ? e.message
            : String(e),
      );
    } finally {
      setSaving(null);
    }
  };

  const upload = async (file: File) => {
    if (!ICON_TYPES.includes(file.type)) {
      toast({ title: "アイコンは PNG・JPEG・WebP の画像だけです", variant: "destructive" });
      return;
    }
    if (file.size > MAX_ICON_BYTES) {
      toast({ title: "アイコンは 256 KB までです", variant: "destructive" });
      return;
    }
    setSaving("icon");
    try {
      apply(await setAgentIcon(file));
      setVersion(Date.now());
      toast({ title: "アイコンを変更しました" });
    } catch (e) {
      toast({
        title: "アイコンを変更できませんでした",
        description: e instanceof Error ? e.message : String(e),
        variant: "destructive",
      });
    } finally {
      setSaving(null);
    }
  };

  const removeIcon = async () => {
    setSaving("remove");
    try {
      apply(await deleteAgentIcon());
      setVersion(Date.now());
    } catch (e) {
      toast({ title: "外せませんでした", description: e instanceof Error ? e.message : String(e), variant: "destructive" });
    } finally {
      setSaving(null);
    }
  };

  const current = profile?.is_default ? "" : (profile?.name ?? "");
  const dirty = name.trim() !== current;

  return (
    <SettingsPage
      title="メッセージのエージェント"
      description="メッセージで @ を付けて呼ぶ、あなた専用のエージェントの名前とアイコンです。呼べるのはあなただけで、あなたの権限で答えます。"
    >
      <SettingsSection
        title="名前とアイコン"
        description="名前とアイコンは見た目だけで、権限には影響しません。"
      >
        {loadError ? (
          <p className="px-5 py-6 text-[13px] text-destructive" role="alert">
            {loadError}
          </p>
        ) : !profile ? (
          <div className="flex flex-col gap-3 px-5 py-5" aria-busy="true">
            <Skeleton className="h-14 w-14 rounded-[14px]" />
            <Skeleton className="h-9 w-72" />
          </div>
        ) : (
          <>
            <SettingsRow label="アイコン" description="PNG・JPEG・WebP、256 KB まで。正方形の画像を推奨します。">
              <div className="flex items-center gap-3">
                <AgentIcon userId={me?.id ?? ""} hasIcon={profile.has_icon} version={version} />
                <input
                  ref={fileInput}
                  type="file"
                  accept={ICON_TYPES.join(",")}
                  className="sr-only"
                  aria-label="アイコンの画像を選ぶ"
                  onChange={(e) => {
                    const f = e.target.files?.[0];
                    e.target.value = "";
                    if (f) void upload(f);
                  }}
                />
                <Button variant="outline" size="sm" loading={saving === "icon"} onClick={() => fileInput.current?.click()}>
                  <ImagePlus className="size-3.5" aria-hidden />
                  画像を選ぶ
                </Button>
                {profile.has_icon ? (
                  <Button variant="ghost" size="sm" loading={saving === "remove"} onClick={() => void removeIcon()}>
                    <Trash2 className="size-3.5" aria-hidden />
                    外す
                  </Button>
                ) : null}
              </div>
            </SettingsRow>
            <SettingsRow
              label="名前"
              htmlFor="agent-name"
              description={`空欄にすると既定の「${DEFAULT_AGENT_NAME}」に戻ります。`}
              stacked
            >
              <form
                className="flex w-full flex-col gap-2"
                onSubmit={(e) => {
                  e.preventDefault();
                  if (dirty) void saveName();
                }}
              >
                <div className="flex w-full flex-wrap items-center gap-2">
                  <Input
                    id="agent-name"
                    value={name}
                    onChange={(e) => {
                      setName(e.target.value);
                      setNameError(null);
                    }}
                    placeholder={DEFAULT_AGENT_NAME}
                    maxLength={40}
                    aria-invalid={nameError ? true : undefined}
                    aria-describedby={nameError ? "agent-name-error" : undefined}
                    className="w-full sm:w-72"
                  />
                  <Button type="submit" size="sm" disabled={!dirty} loading={saving === "name"}>
                    保存
                  </Button>
                </div>
                {nameError ? (
                  <p id="agent-name-error" className="text-[12.5px] text-destructive" role="alert">
                    {nameError}
                  </p>
                ) : null}
              </form>
            </SettingsRow>
          </>
        )}
      </SettingsSection>

      <SettingsSection title="ほかの人からの見え方">
        <div className="flex items-start gap-3 px-5 py-4">
          <ShieldCheck className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
          <p className="text-[12.5px] leading-relaxed text-muted-foreground">
            エージェントの発言には、名前とアイコンに並べて「（あなたの表示名） のエージェント」と持ち主が必ず表示されます。
            ほかの人があなたのエージェントを呼ぶことはできません。「自分だけに表示」で呼んだ回答は、あなたが投稿するまでほかの参加者には見えません。
          </p>
        </div>
      </SettingsSection>
    </SettingsPage>
  );
}
