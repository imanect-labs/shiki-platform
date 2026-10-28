"use client";

import * as React from "react";
import { AlertCircle } from "lucide-react";

import type { AutonomousMode } from "@/lib/chat-api";
import type { EffortLevel, LlmModelsResponse } from "@/lib/admin-ai-api";
import { useLlmModels } from "@/lib/model-choice";
import {
  reloadPreferences,
  updatePreferences,
  usePreferences,
  type DefaultModelChoice,
  type UserPreferences,
} from "@/lib/preferences";
import { useModKeyLabel } from "@/hooks/use-mod-key";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
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
import { APPROVAL_MODES, approvalModeDef } from "@/components/chat/approval-mode-select";
import { EFFORTS, defaultModelOf, defaultOption } from "@/components/chat/model-picker";
import { SettingsPage, SettingsRow, SettingsSection } from "./settings-ui";

/// 「選ばない」（組織の既定・Auto）を表す Select の値。
const DEFAULT_KEY = "__default";

/// 保存する（楽観的に反映済み）。失敗したら知らせる（値はサーバの値へ戻る）。
function save(patch: Partial<UserPreferences>) {
  updatePreferences(patch).catch((e: unknown) =>
    toast({
      title: "保存できませんでした",
      description: e instanceof Error ? e.message : String(e),
      variant: "destructive",
    }),
  );
}

/// 既定のモデルが思考の深さを選べるか（`model: null` は組織の既定のモデル）。
function supportsEffort(data: LlmModelsResponse, model: string | null): boolean {
  const m = model === null ? defaultModelOf(data) : data.models.find((x) => x.id === model);
  return Boolean(m?.supports_effort);
}

/// 新しいチャットの既定のモデルと思考の深さ（入力欄のモデル選択と同じ選択肢）。
function DefaultModelRows({ value }: { value: DefaultModelChoice | null }) {
  const data = useLlmModels();
  // 読み込み中と取得失敗は区別できない（どちらも null）ので、どちらでも正しい説明にする。
  if (!data) {
    return (
      <SettingsRow label="モデル" description="選べるモデルを取得できるまでは、組織の既定で始まります。">
        <Skeleton className="h-9 w-full sm:w-56" />
      </SettingsRow>
    );
  }
  if (!data.allow_user_choice) {
    return (
      <SettingsRow
        label="モデル"
        description="組織の設定でモデルを選べないため、組織の既定のモデルで始まります。"
      />
    );
  }
  const model = value?.model ?? null;
  const missing = model !== null && !data.models.some((m) => m.id === model);
  const effort: EffortLevel = value?.effort ?? data.default_effort;
  const showEffort = !missing && supportsEffort(data, model);
  const dflt = defaultOption(data);

  /// 組織の既定のまま思考の深さも既定なら「指定なし」（null）で保存する。
  const choose = (nextModel: string | null, nextEffort: EffortLevel) => {
    const next: DefaultModelChoice | null =
      nextModel === null && (nextEffort === data.default_effort || !supportsEffort(data, null))
        ? null
        : { model: nextModel, effort: nextEffort };
    save({ default_model: next });
  };

  return (
    <>
      <SettingsRow
        label="モデル"
        htmlFor="pref-model"
        description={
          missing ? (
            <span className="text-destructive">
              選んでいたモデルは公開されていないため、組織の既定で始まります。
            </span>
          ) : model === null && dflt.target ? (
            `いまは ${dflt.target} で答えます。`
          ) : (
            "入力欄のモデル選択の初期値です。チャットごとに変えられます。"
          )
        }
      >
        <Select
          value={missing ? undefined : (model ?? DEFAULT_KEY)}
          onValueChange={(v) => {
            const next = v === DEFAULT_KEY ? null : v;
            choose(next, supportsEffort(data, next) ? effort : data.default_effort);
          }}
        >
          <SelectTrigger id="pref-model" className="h-9 w-full sm:w-56" aria-label="既定のモデル">
            <SelectValue placeholder="モデルを選ぶ" />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={DEFAULT_KEY}>{dflt.label}</SelectItem>
            {data.models.map((m) => (
              <SelectItem key={m.id} value={m.id}>
                {m.label}
                <span className="ml-1.5 text-xs text-muted-foreground">{m.provider_label}</span>
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </SettingsRow>
      {showEffort ? (
        <SettingsRow label="思考の深さ" description={EFFORTS.find((e) => e.value === effort)?.hint}>
          <SegmentedControl
            aria-label="既定の思考の深さ"
            size="sm"
            value={effort}
            onValueChange={(v) => choose(model, v as EffortLevel)}
            options={EFFORTS.map((e) => ({ value: e.value, label: e.label }))}
          />
        </SettingsRow>
      ) : null}
    </>
  );
}

/// 設定 → チャット。アカウントに保存され、ほかの端末でも同じ設定になる（#952）。
export function ChatSettingsView() {
  const { prefs, loaded, bypassAllowed, error } = usePreferences();
  const mod = useModKeyLabel();
  const [retrying, setRetrying] = React.useState(false);

  if (!loaded) {
    return (
      <SettingsPage title="チャット" description="入力のしかたと、新しいチャットの始まり方を設定します。">
        {error ? (
          <EmptyState
            icon={AlertCircle}
            title="設定を読み込めませんでした"
            description={error}
            action={
              <Button
                variant="outline"
                size="sm"
                loading={retrying}
                onClick={() => {
                  setRetrying(true);
                  // 失敗は error に出る（再試行ボタンはそのまま押せる）。
                  void reloadPreferences().finally(() => setRetrying(false));
                }}
              >
                再読み込み
              </Button>
            }
            className="py-10"
          />
        ) : (
          <div className="flex flex-col gap-4" aria-busy="true" aria-label="読み込み中">
            <Skeleton className="h-28 w-full rounded-2xl" />
            <Skeleton className="h-48 w-full rounded-2xl" />
          </div>
        )}
      </SettingsPage>
    );
  }

  const mode = prefs.default_autonomous_mode;
  const clamped = mode === "bypass" && !bypassAllowed;

  return (
    <SettingsPage title="チャット" description="入力のしかたと、新しいチャットの始まり方を設定します。">
      <SettingsSection title="入力" description="アカウントに保存され、ほかの端末でも同じ設定になります。">
        <SettingsRow
          label="送信キー"
          description={
            prefs.submit_key === "mod_enter"
              ? `${mod}+Enter で送信し、Enter で改行します。`
              : "Enter で送信し、Shift+Enter で改行します。"
          }
        >
          <SegmentedControl
            aria-label="送信キー"
            size="sm"
            value={prefs.submit_key}
            onValueChange={(v) => save({ submit_key: v as UserPreferences["submit_key"] })}
            options={[
              { value: "enter", label: "Enter", testId: "submit-key-enter" },
              { value: "mod_enter", label: `${mod}+Enter`, testId: "submit-key-mod-enter" },
            ]}
          />
        </SettingsRow>
      </SettingsSection>

      <SettingsSection
        title="新しいチャット"
        description="ホームから始めるチャットの初期状態です。始めた後もチャットごとに変えられます。"
      >
        <DefaultModelRows value={prefs.default_model} />
        <SettingsRow
          label="Web 検索を使う"
          htmlFor="pref-web-search"
          description="最初の発話で Web 検索を選んだ状態にします。"
        >
          <Switch
            id="pref-web-search"
            checked={prefs.web_search_by_default}
            onCheckedChange={(v) => save({ web_search_by_default: v })}
          />
        </SettingsRow>
        <SettingsRow
          label="承認モード"
          htmlFor="pref-approval"
          description={
            clamped ? (
              <span className="text-destructive">
                全自動は組織ポリシで禁止されているため、承認必須で始まります。
              </span>
            ) : (
              <>
                ホームの「エージェント」で始めるときの初期値です。ホームの承認の選択に表示され、チャットごとに変えられます。
                {approvalModeDef(mode).description}
              </>
            )
          }
        >
          <Select value={mode} onValueChange={(v) => save({ default_autonomous_mode: v as AutonomousMode })}>
            <SelectTrigger id="pref-approval" className="h-9 w-full sm:w-56" aria-label="既定の承認モード">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {APPROVAL_MODES.map((m) => {
                const Icon = m.icon;
                const disabled = m.mode === "bypass" && !bypassAllowed;
                return (
                  <SelectItem key={m.mode} value={m.mode} disabled={disabled}>
                    <span className="flex items-center gap-2">
                      <Icon className={m.danger ? "size-3.5 text-destructive" : "size-3.5"} />
                      {m.label}
                      {disabled ? <span className="text-xs text-muted-foreground">（組織で禁止）</span> : null}
                    </span>
                  </SelectItem>
                );
              })}
            </SelectContent>
          </Select>
        </SettingsRow>
      </SettingsSection>

      <SettingsSection title="回答の表示">
        <SettingsRow
          label="思考過程を開いて表示"
          htmlFor="pref-reasoning"
          description="AI が考えた過程を、折りたたまずに最初から表示します。"
        >
          <Switch
            id="pref-reasoning"
            checked={prefs.expand_reasoning}
            onCheckedChange={(v) => save({ expand_reasoning: v })}
          />
        </SettingsRow>
      </SettingsSection>
    </SettingsPage>
  );
}
