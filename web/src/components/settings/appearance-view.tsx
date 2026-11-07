"use client";

import * as React from "react";
import { useTheme } from "next-themes";
import { Monitor, Moon, Sun } from "lucide-react";

import { cn } from "@/lib/utils";
import { formatDateTime, relativeTime } from "@/lib/format";
import { updatePreferences, usePreferences, type TimeFormat } from "@/lib/preferences";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { SegmentedControl } from "@/components/ui/segmented-control";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { toast } from "@/components/ui/use-toast";
import { SettingsPage, SettingsRow, SettingsSection, SoonBadge } from "./settings-ui";

const THEMES = [
  { value: "light", label: "ライト", icon: Sun },
  { value: "dark", label: "ダーク", icon: Moon },
  { value: "system", label: "システムに合わせる", icon: Monitor },
] as const;

/// テーマの選択カード（見本付き）。next-themes に保存される（この端末のブラウザ単位）。
function ThemePicker() {
  const { theme, setTheme } = useTheme();
  const [mounted, setMounted] = React.useState(false);
  React.useEffect(() => setMounted(true), []);
  return (
    <div role="radiogroup" aria-label="テーマ" className="grid w-full grid-cols-3 gap-2">
      {THEMES.map(({ value, label, icon: Icon }) => {
        const active = mounted && theme === value;
        return (
          <button
            key={value}
            type="button"
            role="radio"
            aria-checked={active}
            onClick={() => setTheme(value)}
            className={cn(
              "flex flex-col items-start gap-2.5 rounded-xl border bg-background p-2 text-left text-[12.5px] outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring",
              active ? "border-primary ring-1 ring-primary" : "hover:border-foreground/20",
            )}
          >
            <span
              aria-hidden
              className={cn(
                "flex h-14 w-full overflow-hidden rounded-lg border",
                value === "dark" && "bg-[oklch(0.16_0.004_285.8)]",
                value === "light" && "bg-[oklch(0.99_0_0)]",
                value === "system" &&
                  "bg-[linear-gradient(90deg,oklch(0.99_0_0)_50%,oklch(0.16_0.004_285.8)_50%)]",
              )}
            />
            <span className="flex items-center gap-1.5 px-0.5 pb-0.5 font-medium">
              <Icon className="size-3.5" aria-hidden />
              {label}
            </span>
          </button>
        );
      })}
    </div>
  );
}

/// 日時の表示形式（アカウントに保存・#952）。見本は 2 時間前の時刻で見せる。
function TimeFormatRow() {
  const { prefs, loaded, error } = usePreferences();
  const [sample] = React.useState(() => {
    const now = Date.now();
    return { now, iso: new Date(now - 2 * 3600_000).toISOString() };
  });
  const example = (f: TimeFormat) =>
    f === "relative" ? relativeTime(sample.iso, sample.now) : formatDateTime(sample.iso);
  return (
    <SettingsRow
      label="日時の表示"
      description={
        loaded
          ? `ドライブやワークフローの一覧で「${example(prefs.time_format)}」のように表示します。`
          : (error ?? "読み込み中…")
      }
    >
      {loaded ? (
        <SegmentedControl
          aria-label="日時の表示"
          size="sm"
          value={prefs.time_format}
          onValueChange={(v) =>
            updatePreferences({ time_format: v as TimeFormat }).catch((e: unknown) =>
              toast({
                title: "保存できませんでした",
                description: e instanceof Error ? e.message : String(e),
                variant: "destructive",
              }),
            )
          }
          options={[
            { value: "absolute", label: "日時" },
            { value: "relative", label: "経過時間" },
          ]}
        />
      ) : error ? null : (
        <Skeleton className="h-8 w-40" />
      )}
    </SettingsRow>
  );
}

/// 設定 → 表示。テーマは実際に切り替わる（ブラウザに保存）。日時の表示はアカウントに保存する。
/// 言語・密度などはモック。
export function AppearanceView() {
  const [density, setDensity] = React.useState("comfortable");
  const [lang, setLang] = React.useState("ja");
  const [motion, setMotion] = React.useState(false);

  return (
    <SettingsPage title="表示" description="画面の見た目と言語を設定します。">
      <SettingsSection title="テーマ" description="この端末のブラウザに保存されます。">
        <div className="p-5">
          <ThemePicker />
        </div>
      </SettingsSection>

      <SettingsSection title="日時" description="アカウントに保存され、ほかの端末でも同じ表示になります。">
        <TimeFormatRow />
      </SettingsSection>

      <SettingsSection
        title={
          <>
            表示の詳細
            <SoonBadge />
          </>
        }
        description="操作しても保存されません。"
      >
        <SettingsRow label="言語" description="メニューやメッセージの言語。">
          <Select value={lang} onValueChange={setLang}>
            <SelectTrigger className="h-9 w-full sm:w-48" aria-label="言語">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="ja">日本語</SelectItem>
              <SelectItem value="en">English</SelectItem>
            </SelectContent>
          </Select>
        </SettingsRow>
        <SettingsRow label="表示密度" description="一覧の行の高さ。">
          <SegmentedControl
            aria-label="表示密度"
            size="sm"
            value={density}
            onValueChange={setDensity}
            options={[
              { value: "comfortable", label: "標準" },
              { value: "compact", label: "コンパクト" },
            ]}
          />
        </SettingsRow>
        <SettingsRow label="動きを減らす" description="アニメーションを最小限にします。">
          <Switch checked={motion} onCheckedChange={setMotion} aria-label="動きを減らす" />
        </SettingsRow>
      </SettingsSection>
    </SettingsPage>
  );
}
