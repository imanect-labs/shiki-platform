"use client";

/// スキルエディタの設定（#919）。本文の上に「プロパティ」（知識スコープ・使うツール・モデル既定）、
/// 本文の下に「お手本の対話」と「script」を置く（ノートのプロパティ欄と同じ並び）。
/// 値は [`SkillDraft`]（保存 API の SkillBody と相互変換する編集状態）を直接読み書きする。

import {
  ChevronDown,
  Cpu,
  FolderTree,
  MessagesSquare,
  Plus,
  ScrollText,
  Wrench,
  X,
  type LucideIcon,
} from "lucide-react";
import * as React from "react";

import type { ScriptKind, ToolName } from "@/generated/gui-spec";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import type { SkillDraft } from "@/lib/skills/skill-draft";
import { cn } from "@/lib/utils";
import { DriveRefChip } from "./drive-ref-chip";
import { DriveRefPicker } from "./drive-ref-picker";

/// 使うツールの選択肢（`ToolName` 型で束縛＝語彙とズレるとコンパイルエラー）。
/// allowed_tools は「使うツールの宣言＝モデルへの誘導」で、承認ゲート（破壊系の明示許可）は
/// スキルでは外れない。
const TOOL_OPTIONS: { value: ToolName; label: string; note?: string }[] = [
  { value: "doc_search", label: "社内文書検索" },
  { value: "doc_read", label: "社内文書を読む" },
  { value: "web_search", label: "Web 検索" },
  { value: "web_fetch", label: "Web ページ取得" },
  { value: "code_interpreter", label: "コード実行" },
  { value: "shell", label: "シェル", note: "実行ごとに承認が必要" },
];

const SCRIPT_KINDS: { value: ScriptKind; label: string; ext: string }[] = [
  { value: "shiki", label: "shiki script", ext: ".shiki" },
  { value: "shell", label: "shell script", ext: ".sh" },
];

type Update = <K extends keyof SkillDraft>(key: K, value: SkillDraft[K]) => void;

/// 1 行のプロパティ（PC は「見出し | 値」の 2 列、スマホは見出しの下に値）。
function PropertyRow({
  icon: Icon,
  label,
  hint,
  children,
}: {
  icon: LucideIcon;
  label: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="grid grid-cols-1 gap-1.5 py-2 sm:grid-cols-[9.5rem_minmax(0,1fr)] sm:gap-3">
      <div className="flex min-h-8 items-center gap-2 text-[13px] text-muted-foreground">
        <Icon className="size-4 shrink-0" aria-hidden />
        <span>{label}</span>
      </div>
      <div className="min-w-0">
        {children}
        {hint ? <p className="mt-1 text-xs text-muted-foreground">{hint}</p> : null}
      </div>
    </div>
  );
}

/// トグルのピル（使うツールの選択）。
function Pill({
  active,
  onClick,
  children,
  testId,
}: {
  active: boolean;
  onClick: () => void;
  children: React.ReactNode;
  testId?: string;
}) {
  return (
    <button
      type="button"
      aria-pressed={active}
      onClick={onClick}
      data-testid={testId}
      className={cn(
        "inline-flex min-h-8 items-center gap-1 rounded-full border px-3 text-[13px] outline-none transition-colors",
        "focus-visible:ring-2 focus-visible:ring-ring pointer-coarse:min-h-10",
        active
          ? "border-primary/40 bg-primary/10 font-medium text-foreground"
          : "border-border text-muted-foreground hover:border-foreground/25 hover:text-foreground",
      )}
    >
      {children}
    </button>
  );
}

export function SkillProperties({ draft, update }: { draft: SkillDraft; update: Update }) {
  const [scopePickerOpen, setScopePickerOpen] = React.useState(false);
  const modelSet = Boolean(draft.model || draft.temperature || draft.maxTokens);
  // 値があれば常に開く（AI の下書きで後から入った値も見せる）。
  const [modelOpen, setModelOpen] = React.useState(false);
  const showModel = modelOpen || modelSet;

  const toggleTool = (tool: ToolName) => {
    const current = draft.allowedTools ?? [];
    const next = current.includes(tool) ? current.filter((t) => t !== tool) : [...current, tool];
    // 空の宣言は保存できない（全ツールは null で表す）ので、最後の 1 つを外したら「すべて」に戻す。
    update("allowedTools", next.length > 0 ? next : null);
  };

  return (
    <section aria-label="スキルの設定" className="divide-y divide-border/60 border-y border-border/60">
      <PropertyRow
        icon={FolderTree}
        label="知識スコープ"
        hint={
          draft.scopeFolders.length + draft.scopeFiles.length === 0
            ? "未指定なら、使う人が読めるすべての文書から探します。"
            : "社内文書検索をこの範囲に絞ります（読めるかどうかは使う人の権限で確かめます）。"
        }
      >
        <div className="flex flex-wrap items-center gap-1.5">
          {draft.scopeFolders.map((id) => (
            <DriveRefChip
              key={id}
              kind="folder"
              id={id}
              onRemove={() => update("scopeFolders", draft.scopeFolders.filter((x) => x !== id))}
              className="text-[13px]"
            />
          ))}
          {draft.scopeFiles.map((id) => (
            <DriveRefChip
              key={id}
              kind="file"
              id={id}
              onRemove={() => update("scopeFiles", draft.scopeFiles.filter((x) => x !== id))}
              className="text-[13px]"
            />
          ))}
          <Button
            type="button"
            size="sm"
            variant="ghost"
            onClick={() => setScopePickerOpen(true)}
            className="h-8 text-muted-foreground"
            data-testid="skill-scope-add"
          >
            <Plus className="size-4" aria-hidden />
            フォルダを追加
          </Button>
        </div>
      </PropertyRow>

      <PropertyRow
        icon={Wrench}
        label="使うツール"
        hint={
          draft.allowedTools === null
            ? "AI が必要に応じてどのツールでも使えます。"
            : "このスキルを使うときに AI が使うツールです（承認が必要な操作は従来どおり確認します）。"
        }
      >
        <div className="flex flex-wrap gap-1.5" role="group" aria-label="使うツール">
          <Pill
            active={draft.allowedTools === null}
            onClick={() => update("allowedTools", null)}
            testId="skill-tools-all"
          >
            すべて
          </Pill>
          {TOOL_OPTIONS.map((t) => (
            <Pill
              key={t.value}
              active={draft.allowedTools?.includes(t.value) ?? false}
              onClick={() => toggleTool(t.value)}
            >
              {t.label}
              {t.note ? <span className="text-[11px] text-muted-foreground">（{t.note}）</span> : null}
            </Pill>
          ))}
          {/* 上の選択肢に無いツール（AI の下書きや既存の定義が宣言したもの）も見せ、外せるようにする。 */}
          {(draft.allowedTools ?? [])
            .filter((t) => !TOOL_OPTIONS.some((o) => o.value === t))
            .map((t) => (
              <Pill key={t} active onClick={() => toggleTool(t)}>
                <span className="font-mono text-[12px]">{t}</span>
              </Pill>
            ))}
        </div>
      </PropertyRow>

      <PropertyRow icon={Cpu} label="モデル既定">
        {showModel ? (
          <div className="grid grid-cols-1 gap-2 sm:grid-cols-3">
            <SubField label="モデル名" htmlFor="skill-model">
              <Input
                id="skill-model"
                value={draft.model}
                onChange={(e) => update("model", e.target.value)}
                placeholder="例: claude-sonnet"
                autoCapitalize="none"
                autoCorrect="off"
                spellCheck={false}
              />
            </SubField>
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
            onClick={() => setModelOpen(true)}
            className="inline-flex min-h-8 items-center gap-1 rounded-md px-2 text-[13px] text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring -ml-2"
          >
            システム既定
            <ChevronDown className="size-3.5" aria-hidden />
          </button>
        )}
      </PropertyRow>

      <DriveRefPicker
        open={scopePickerOpen}
        onOpenChange={setScopePickerOpen}
        accept="folder"
        onSelect={(ref) => {
          if (!draft.scopeFolders.includes(ref.id)) {
            update("scopeFolders", [...draft.scopeFolders, ref.id]);
          }
        }}
      />
    </section>
  );
}

/// 本文の下の折りたたみ節（見出し＋件数＋追加ボタン）。
function Section({
  icon: Icon,
  title,
  description,
  count,
  onAdd,
  addLabel,
  children,
}: {
  icon: LucideIcon;
  title: string;
  description: string;
  count: number;
  onAdd: () => void;
  addLabel: string;
  children: React.ReactNode;
}) {
  return (
    <section className="border-t border-border/60 pt-4" aria-label={title}>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <h2 className="flex items-center gap-2 text-sm font-semibold">
          <Icon className="size-4 text-muted-foreground" aria-hidden />
          {title}
          {count > 0 ? (
            <span className="rounded-full bg-muted px-1.5 text-[11px] font-normal text-muted-foreground">
              {count}
            </span>
          ) : null}
        </h2>
        <span className="min-w-0 flex-1 text-xs text-muted-foreground max-sm:basis-full max-sm:order-last">
          {description}
        </span>
        <Button type="button" size="sm" variant="ghost" onClick={onAdd} className="h-8">
          <Plus className="size-4" aria-hidden />
          {addLabel}
        </Button>
      </div>
      {count > 0 ? <div className="mt-3 space-y-3">{children}</div> : null}
    </section>
  );
}

export function SkillExtras({ draft, update }: { draft: SkillDraft; update: Update }) {
  return (
    <div className="mt-10 space-y-6">
      <Section
        icon={MessagesSquare}
        title="お手本の対話"
        description="こう聞かれたらこう答える、という例（few-shot・任意・最大 8 件）。"
        count={draft.fewShot.length}
        onAdd={() => update("fewShot", [...draft.fewShot, { user: "", assistant: "" }])}
        addLabel="例を追加"
      >
        {draft.fewShot.map((ex, i) => (
          <div key={i} className="space-y-1.5 rounded-lg border border-border p-3">
            <div className="flex items-center justify-between">
              <span className="text-xs font-medium text-muted-foreground">例 {i + 1}</span>
              <button
                type="button"
                aria-label={`例 ${i + 1} を削除`}
                onClick={() => update("fewShot", draft.fewShot.filter((_, j) => j !== i))}
                className={REMOVE_ROW}
              >
                <X className="size-4" aria-hidden />
              </button>
            </div>
            <Input
              value={ex.user}
              onChange={(e) =>
                update(
                  "fewShot",
                  draft.fewShot.map((x, j) => (j === i ? { ...x, user: e.target.value } : x)),
                )
              }
              placeholder="ユーザーの発話（例: 交通費は何日以内に申請？）"
              aria-label={`例 ${i + 1} のユーザー発話`}
            />
            <Textarea
              value={ex.assistant}
              onChange={(e) =>
                update(
                  "fewShot",
                  draft.fewShot.map((x, j) => (j === i ? { ...x, assistant: e.target.value } : x)),
                )
              }
              placeholder="期待する応答"
              aria-label={`例 ${i + 1} の期待応答`}
              rows={2}
            />
          </div>
        ))}
      </Section>

      <Section
        icon={ScrollText}
        title="script"
        description="スキルに同梱するスクリプト（任意・実行は今後のフェーズ）。"
        count={draft.scripts.length}
        onAdd={() => update("scripts", [...draft.scripts, { path: "", kind: "shiki", source: "" }])}
        addLabel="script を追加"
      >
        {draft.scripts.map((sc, i) => (
          <div key={i} className="space-y-1.5 rounded-lg border border-border p-3">
            <div className="flex items-center gap-2">
              <Input
                value={sc.path}
                onChange={(e) =>
                  update(
                    "scripts",
                    draft.scripts.map((x, j) => (j === i ? { ...x, path: e.target.value } : x)),
                  )
                }
                placeholder={`scripts/example${SCRIPT_KINDS.find((k) => k.value === sc.kind)?.ext}`}
                aria-label={`script ${i + 1} のパス`}
                className="min-w-0 flex-1"
                autoCapitalize="none"
                autoCorrect="off"
                spellCheck={false}
              />
              <select
                value={sc.kind}
                onChange={(e) =>
                  update(
                    "scripts",
                    draft.scripts.map((x, j) =>
                      j === i ? { ...x, kind: e.target.value as ScriptKind } : x,
                    ),
                  )
                }
                aria-label={`script ${i + 1} の種別`}
                className="h-9 rounded-lg border border-input bg-background px-2 text-base md:text-sm pointer-coarse:text-base"
              >
                {SCRIPT_KINDS.map((k) => (
                  <option key={k.value} value={k.value}>
                    {k.label}
                  </option>
                ))}
              </select>
              <button
                type="button"
                aria-label={`script ${i + 1} を削除`}
                onClick={() => update("scripts", draft.scripts.filter((_, j) => j !== i))}
                className={REMOVE_ROW}
              >
                <X className="size-4" aria-hidden />
              </button>
            </div>
            <Textarea
              value={sc.source}
              onChange={(e) =>
                update(
                  "scripts",
                  draft.scripts.map((x, j) => (j === i ? { ...x, source: e.target.value } : x)),
                )
              }
              placeholder="スクリプト本文"
              aria-label={`script ${i + 1} の本文`}
              rows={4}
              className="font-mono text-xs max-md:text-base"
              autoCapitalize="none"
              autoCorrect="off"
              spellCheck={false}
            />
          </div>
        ))}
      </Section>
    </div>
  );
}

const REMOVE_ROW = cn(
  "flex size-8 shrink-0 items-center justify-center rounded-md text-muted-foreground outline-none",
  "hover:bg-accent hover:text-destructive focus-visible:ring-2 focus-visible:ring-ring pointer-coarse:size-11",
);

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
    <div className="space-y-1">
      <label htmlFor={htmlFor} className="block text-[11px] text-muted-foreground">
        {label}
      </label>
      {children}
    </div>
  );
}
