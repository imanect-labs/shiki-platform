"use client";

/// スキルの「モデル既定」欄（#1039）。使うモデルは組織が公開しているモデル（`GET /llm/models`）
/// から選ぶ（自由入力だと綴りの誤りが実行時まで分からない）。温度・最大トークン数は
/// 「詳細」に畳み、値があれば開いておく。
///
/// スキルのモデル指定が効くのは、チャットのモデルが Auto のときだけ（利用者が選んだモデルが
/// 優先される・組織が選択を禁じていれば使われない）。サーバ側の判定は
/// `llm_gateway::tenant::resolve_at_run` が正。

import { ChevronDown } from "lucide-react";
import * as React from "react";

import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectSeparator,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Input } from "@/components/ui/input";
import type { LlmModelOption, LlmModelsResponse } from "@/lib/admin-ai-api";
import type { SkillDraft } from "@/lib/skills/skill-draft";
import { useLlmModels } from "@/lib/model-choice";
import { cn } from "@/lib/utils";

/// 「指定しない」の値（Radix Select は空文字を値にできない）。
const DEFAULT_VALUE = "__default";

type Update = <K extends keyof SkillDraft>(key: K, value: SkillDraft[K]) => void;

type ProviderGroup = { provider: string; models: LlmModelOption[] };

/// プロバイダごとにまとめる（カタログの並び順を保つ）。
function groupByProvider(models: LlmModelOption[]): ProviderGroup[] {
  const groups: ProviderGroup[] = [];
  for (const m of models) {
    const g = groups.find((x) => x.provider === m.provider_label);
    if (g) g.models.push(m);
    else groups.push({ provider: m.provider_label, models: [m] });
  }
  return groups;
}

/// 選んだモデルの下に出す説明（効く条件・効かない理由）。
///
/// 一覧（`GET /llm/models`）は**編集している本人**から見たもので、実行時の判定は**使う人**の
/// 設定で行う（本人専用・ユーザー単位で許可されたモデルがある）。なので「必ず効く／効かない」
/// とは言い切らず、使う人によって変わることを添える。
function modelHint(data: LlmModelsResponse | null, model: string, known: boolean): string {
  if (!data) return "選べるモデルの一覧を取得できていません。";
  if (!model) return "チャットで選ばれているモデル（Auto なら組織の振り先）で動きます。";
  if (!known) {
    return "このモデルはあなたが選べるモデルにありません。使えない人が使うときは、この指定は効きません。";
  }
  if (!data.allow_user_choice && model !== data.default_model) {
    return "組織の設定でモデルの選択が許可されていないため、この指定は使われません。";
  }
  return "チャットのモデルが Auto のときにこのモデルで動きます（利用者が選んだモデルが優先され、このモデルを使えない人には効きません）。";
}

export function SkillModelField({ draft, update }: { draft: SkillDraft; update: Update }) {
  const data = useLlmModels();
  const model = draft.model.trim();
  const groups = React.useMemo(() => groupByProvider(data?.models ?? []), [data]);
  const known = data?.models.some((m) => m.id === model) ?? false;
  const paramsSet = Boolean(draft.temperature.trim() || draft.maxTokens.trim());
  // 値があれば開く（AI の下書きで後から入った値も見せる）。一度開いたら閉じない
  // （値を消し切った途端に入力欄が消えると、打ち直そうとした文字が行き場を失う）。
  const [paramsOpen, setParamsOpen] = React.useState(paramsSet);
  React.useEffect(() => {
    if (paramsSet) setParamsOpen(true);
  }, [paramsSet]);
  const showParams = paramsOpen || paramsSet;

  return (
    <div className="space-y-2">
      <Select
        value={model || DEFAULT_VALUE}
        onValueChange={(v) => update("model", v === DEFAULT_VALUE ? "" : v)}
      >
        <SelectTrigger
          className="h-9 w-full max-w-sm text-[13px] pointer-coarse:h-11"
          aria-label="使うモデル"
          data-testid="skill-model-select"
        >
          <SelectValue />
        </SelectTrigger>
        <SelectContent className="max-h-80">
          <SelectItem value={DEFAULT_VALUE}>指定しない（システム既定）</SelectItem>
          {model && !known ? (
            // 公開から外れた・まだ一覧を取れていない値も、選択中として見せる（黙って消さない）。
            <SelectItem value={model}>
              <span className="font-mono text-[12px]">{model}</span>
              {data ? <span className="text-muted-foreground">（選べません）</span> : null}
            </SelectItem>
          ) : null}
          {groups.map((g) => (
            <React.Fragment key={g.provider}>
              <SelectSeparator />
              <SelectGroup>
                <SelectLabel>{g.provider}</SelectLabel>
                {g.models.map((m) => (
                  <SelectItem key={m.id} value={m.id}>
                    {m.label}
                    {m.processed_abroad ? (
                      <span className="text-[11px] text-muted-foreground">（国外処理）</span>
                    ) : null}
                  </SelectItem>
                ))}
              </SelectGroup>
            </React.Fragment>
          ))}
        </SelectContent>
      </Select>
      <p className="text-xs text-muted-foreground" data-testid="skill-model-hint">
        {modelHint(data, model, known)}
      </p>

      {showParams ? (
        <div className="grid max-w-sm grid-cols-2 gap-2">
          <SubField label="temperature（0〜2）" htmlFor="skill-temperature">
            <Input
              id="skill-temperature"
              value={draft.temperature}
              onChange={(e) => update("temperature", e.target.value)}
              placeholder="例: 0.3"
              inputMode="decimal"
            />
          </SubField>
          <SubField label="最大トークン数" htmlFor="skill-max-tokens">
            <Input
              id="skill-max-tokens"
              value={draft.maxTokens}
              onChange={(e) => update("maxTokens", e.target.value)}
              placeholder="例: 2000"
              inputMode="numeric"
            />
          </SubField>
        </div>
      ) : (
        <button
          type="button"
          onClick={() => setParamsOpen(true)}
          className={cn(
            "-ml-2 inline-flex min-h-8 items-center gap-1 rounded-md px-2 text-[13px] text-muted-foreground outline-none transition-colors",
            "hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring pointer-coarse:min-h-10",
          )}
          data-testid="skill-model-params-toggle"
        >
          temperature・最大トークン数
          <ChevronDown className="size-3.5" aria-hidden />
        </button>
      )}
    </div>
  );
}

/// 複数の入力を並べる欄の、入力ごとの小さな見出し（placeholder をラベル代わりにしない）。
function SubField({
  label,
  htmlFor,
  children,
}: {
  label: string;
  htmlFor: string;
  children: React.ReactNode;
}) {
  return (
    <div className="min-w-0 space-y-1">
      <label htmlFor={htmlFor} className="block truncate text-[11px] text-muted-foreground">
        {label}
      </label>
      {children}
    </div>
  );
}
