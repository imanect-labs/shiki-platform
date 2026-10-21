"use client";

/// skill の作成/編集ダイアログ（Task 6.11）。
/// SKILL.md 本文・知識スコープ（フォルダ）・許可ツール・モデル既定・few-shot・script を
/// フォームで編集し、保存はサーバの保存時検証（全件エラー）に委ねる。

import * as React from "react";
import { FolderOpen, Loader2, Plus, X } from "lucide-react";

import type { ScriptKind, SkillBody, ToolName } from "@/generated/gui-spec";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { useMediaQuery } from "@/hooks/use-media-query";
import { cn } from "@/lib/utils";
import { createSkill, updateSkill, type SkillVersion } from "@/lib/artifact-api";
import { FolderPicker } from "./folder-picker";

/// 許可ツールの選択肢（`ToolName` 型で束縛＝語彙とズレるとコンパイルエラー）。
/// 破壊系（shell/fs_*）も選べるが、明示許可（承認）は skill では無効化されない。
const TOOL_OPTIONS: { value: ToolName; label: string; note?: string }[] = [
  { value: "doc_search", label: "社内文書検索" },
  { value: "doc_read", label: "社内文書の本文を読む" },
  { value: "web_search", label: "Web 検索" },
  { value: "web_fetch", label: "Web ページ取得" },
  { value: "code_interpreter", label: "コード実行" },
  { value: "shell", label: "シェル（自律・要承認）", note: "実行ごとに承認が必要" },
];

const SCRIPT_KINDS: { value: ScriptKind; label: string; ext: string }[] = [
  { value: "shiki", label: "shiki script", ext: ".shiki" },
  { value: "shell", label: "shell script", ext: ".sh" },
];

type ScopeFolder = { id: string; name: string };

type Draft = {
  name: string;
  description: string;
  instructions: string;
  scopeFolders: ScopeFolder[];
  /// ファイル単位スコープ（エディタ UI では編集しないが、既存値を保存で失わない）。
  scopeFiles: string[];
  allowedTools: ToolName[] | null; // null = 全ツール
  model: string;
  temperature: string;
  maxTokens: string;
  fewShot: { user: string; assistant: string }[];
  scripts: { path: string; kind: ScriptKind; source: string }[];
  /// スラッシュコマンド宣言（#387）。エディタ UI では編集しないが、既存値を保存で失わない
  /// （first-party バンドルの宣言をこの画面の再保存で消さないため）。
  command: SkillBody["command"];
};

const EMPTY_DRAFT: Draft = {
  name: "",
  description: "",
  instructions: "",
  scopeFolders: [],
  scopeFiles: [],
  allowedTools: null,
  model: "",
  temperature: "",
  maxTokens: "",
  fewShot: [],
  scripts: [],
  command: null,
};

function draftFrom(skill: SkillVersion | null, name: string): Draft {
  if (!skill) return { ...EMPTY_DRAFT };
  const b = skill.body;
  return {
    name,
    description: b.description ?? "",
    instructions: b.instructions ?? "",
    // 既存フォルダの表示名は保存していないため id を示す（選び直しで名前が付く）。
    scopeFolders: (b.knowledge_scope?.folders ?? []).map((id) => ({ id, name: id.slice(0, 8) })),
    scopeFiles: b.knowledge_scope?.files ?? [],
    allowedTools: (b.allowed_tools as ToolName[] | null) ?? null,
    model: b.model?.model ?? "",
    temperature: b.model?.temperature != null ? String(b.model.temperature) : "",
    maxTokens: b.model?.max_tokens != null ? String(b.model.max_tokens) : "",
    fewShot: (b.few_shot ?? []).map((f) => ({ user: f.user, assistant: f.assistant })),
    scripts: (b.scripts ?? []).map((s) => ({ path: s.path, kind: s.kind, source: s.source })),
    command: b.command ?? null,
  };
}

function toBody(d: Draft): SkillBody {
  const model =
    d.model.trim() || d.temperature.trim() || d.maxTokens.trim()
      ? {
          model: d.model.trim() || null,
          temperature: d.temperature.trim() ? Number(d.temperature) : null,
          max_tokens: d.maxTokens.trim() ? Number(d.maxTokens) : null,
        }
      : null;
  return {
    description: d.description.trim(),
    instructions: d.instructions,
    knowledge_scope:
      d.scopeFolders.length > 0 || d.scopeFiles.length > 0
        ? { folders: d.scopeFolders.map((f) => f.id), files: d.scopeFiles }
        : null,
    allowed_tools: d.allowedTools,
    model,
    few_shot: d.fewShot.filter((f) => f.user.trim() && f.assistant.trim()),
    scripts: d.scripts.filter((s) => s.path.trim()),
    references: [],
    command: d.command,
  };
}

export function SkillEditorDialog({
  open,
  onOpenChange,
  editing,
  editingName,
  onSaved,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /// 編集対象（null は新規作成）。
  editing: SkillVersion | null;
  editingName: string;
  onSaved: () => void;
}) {
  const [draft, setDraft] = React.useState<Draft>(EMPTY_DRAFT);
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const [pickerOpen, setPickerOpen] = React.useState(false);
  // タッチで開いた瞬間にキーボードを出さない（マウス操作のときだけ名前欄へ自動フォーカス）。
  const finePointer = useMediaQuery("(pointer: fine)");

  React.useEffect(() => {
    if (open) {
      setDraft(draftFrom(editing, editingName));
      setError(null);
    }
  }, [open, editing, editingName]);

  const set = <K extends keyof Draft>(key: K, value: Draft[K]) =>
    setDraft((d) => ({ ...d, [key]: value }));

  const toggleTool = (tool: ToolName) => {
    setDraft((d) => {
      const current = d.allowedTools ?? [];
      const next = current.includes(tool)
        ? current.filter((t) => t !== tool)
        : [...current, tool];
      return { ...d, allowedTools: next };
    });
  };

  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      if (editing) {
        await updateSkill(editing.id, toBody(draft), editing.version);
      } else {
        await createSkill(draft.name.trim(), toBody(draft));
      }
      onSaved();
      onOpenChange(false);
    } catch (err) {
      setError(err instanceof Error ? err.message : "保存に失敗しました");
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>{editing ? `スキルを編集（v${editing.version} → 新版）` : "スキルを作成"}</DialogTitle>
          <DialogDescription>
            指示文・知識スコープ・許可ツール・モデル既定をまとめた「呼び出せる業務知識」を定義します。
          </DialogDescription>
        </DialogHeader>

        <form onSubmit={save} className="space-y-4">
          {!editing ? (
            <Field label="名前" htmlFor="skill-name" required>
              <Input
                id="skill-name"
                value={draft.name}
                onChange={(e) => set("name", e.target.value)}
                placeholder="expense-assistant"
                aria-label="名前"
                required
                autoFocus={finePointer}
                autoCapitalize="none"
                autoCorrect="off"
                spellCheck={false}
              />
            </Field>
          ) : null}

          <Field label="説明" htmlFor="skill-description" required>
            <Input
              id="skill-description"
              value={draft.description}
              onChange={(e) => set("description", e.target.value)}
              placeholder="経費精算の質問に規程を根拠に答える"
              aria-label="説明"
              required
            />
          </Field>

          <Field label="指示文（SKILL.md 本文）" htmlFor="skill-instructions" required>
            <Textarea
              id="skill-instructions"
              value={draft.instructions}
              onChange={(e) => set("instructions", e.target.value)}
              placeholder={"あなたは経費規程に詳しいアシスタントです。\n規程に基づいて回答してください。"}
              aria-label="指示文（SKILL.md 本文）"
              rows={6}
              required
            />
          </Field>

          <Field label="知識スコープ（参照を許すフォルダ・未指定は全可読範囲)">
            <div className="flex flex-wrap items-center gap-1.5">
              {draft.scopeFolders.map((f) => (
                <span
                  key={f.id}
                  className="inline-flex items-center gap-1 rounded-full border border-border bg-secondary/60 px-2.5 py-1 text-xs"
                >
                  <FolderOpen className="size-3.5 text-primary" aria-hidden />
                  {f.name}
                  <button
                    type="button"
                    aria-label={`${f.name} をスコープから外す`}
                    onClick={() =>
                      set(
                        "scopeFolders",
                        draft.scopeFolders.filter((x) => x.id !== f.id),
                      )
                    }
                    className={REMOVE_CHIP}
                  >
                    <X className="size-3" aria-hidden />
                  </button>
                </span>
              ))}
              <Button type="button" size="sm" variant="outline" onClick={() => setPickerOpen(true)}>
                <Plus className="size-4" aria-hidden />
                フォルダを追加
              </Button>
            </div>
          </Field>

          <Field label="許可ツール">
            <label className="mb-1.5 flex min-h-9 cursor-pointer items-center gap-2 text-sm pointer-coarse:min-h-11">
              <input
                type="checkbox"
                checked={draft.allowedTools === null}
                onChange={(e) => set("allowedTools", e.target.checked ? null : [])}
                className="size-4 accent-[var(--primary)]"
              />
              すべてのツールを許可（既定）
            </label>
            {draft.allowedTools !== null ? (
              <div className="grid grid-cols-1 gap-1 rounded-lg border border-border p-2 sm:grid-cols-2 sm:p-3">
                {TOOL_OPTIONS.map((t) => (
                  <label
                    key={t.value}
                    className="flex min-h-9 cursor-pointer items-center gap-2 rounded-md px-1.5 text-sm pointer-coarse:min-h-11"
                  >
                    <input
                      type="checkbox"
                      checked={draft.allowedTools?.includes(t.value) ?? false}
                      onChange={() => toggleTool(t.value)}
                      className="size-4 accent-[var(--primary)]"
                    />
                    <span>
                      {t.label}
                      {t.note ? (
                        <span className="ml-1 text-xs text-muted-foreground">（{t.note}）</span>
                      ) : null}
                    </span>
                  </label>
                ))}
              </div>
            ) : null}
          </Field>

          <Field label="モデル既定（未指定はシステム既定）">
            <div className="grid grid-cols-1 gap-2 sm:grid-cols-3">
              <SubField label="モデル名" htmlFor="skill-model">
                <Input
                  id="skill-model"
                  value={draft.model}
                  onChange={(e) => set("model", e.target.value)}
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
                  onChange={(e) => set("temperature", e.target.value)}
                  placeholder="例: 0.3"
                  aria-label="temperature"
                  inputMode="decimal"
                />
              </SubField>
              <SubField label="最大トークン数" htmlFor="skill-max-tokens">
                <Input
                  id="skill-max-tokens"
                  value={draft.maxTokens}
                  onChange={(e) => set("maxTokens", e.target.value)}
                  placeholder="例: 2000"
                  aria-label="max tokens"
                  inputMode="numeric"
                />
              </SubField>
            </div>
          </Field>

          <Field label="few-shot（お手本の対話・任意）">
            <div className="space-y-2">
              {draft.fewShot.map((ex, i) => (
                <div key={i} className="space-y-1.5 rounded-lg border border-border p-3">
                  <div className="flex items-center justify-between">
                    <span className="text-xs font-medium text-muted-foreground">例 {i + 1}</span>
                    <button
                      type="button"
                      aria-label={`例 ${i + 1} を削除`}
                      onClick={() => set("fewShot", draft.fewShot.filter((_, j) => j !== i))}
                      className={REMOVE_ROW}
                    >
                      <X className="size-4" aria-hidden />
                    </button>
                  </div>
                  <Input
                    value={ex.user}
                    onChange={(e) =>
                      set(
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
                      set(
                        "fewShot",
                        draft.fewShot.map((x, j) =>
                          j === i ? { ...x, assistant: e.target.value } : x,
                        ),
                      )
                    }
                    placeholder="期待する応答"
                    aria-label={`例 ${i + 1} の期待応答`}
                    rows={2}
                  />
                </div>
              ))}
              <Button
                type="button"
                size="sm"
                variant="outline"
                onClick={() => set("fewShot", [...draft.fewShot, { user: "", assistant: "" }])}
              >
                <Plus className="size-4" aria-hidden />
                例を追加
              </Button>
            </div>
          </Field>

          <Field label="script（任意・実行は今後のフェーズ）">
            <div className="space-y-2">
              {draft.scripts.map((sc, i) => (
                <div key={i} className="space-y-1.5 rounded-lg border border-border p-3">
                  <div className="flex items-center gap-2">
                    <Input
                      value={sc.path}
                      onChange={(e) =>
                        set(
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
                        set(
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
                      onClick={() => set("scripts", draft.scripts.filter((_, j) => j !== i))}
                      className={REMOVE_ROW}
                    >
                      <X className="size-4" aria-hidden />
                    </button>
                  </div>
                  <Textarea
                    value={sc.source}
                    onChange={(e) =>
                      set(
                        "scripts",
                        draft.scripts.map((x, j) => (j === i ? { ...x, source: e.target.value } : x)),
                      )
                    }
                    placeholder="スクリプト本文"
                    aria-label={`script ${i + 1} の本文`}
                    rows={3}
                    className="font-mono text-xs max-md:text-base"
                    autoCapitalize="none"
                    autoCorrect="off"
                    spellCheck={false}
                  />
                </div>
              ))}
              <Button
                type="button"
                size="sm"
                variant="outline"
                onClick={() =>
                  set("scripts", [...draft.scripts, { path: "", kind: "shiki", source: "" }])
                }
              >
                <Plus className="size-4" aria-hidden />
                script を追加
              </Button>
            </div>
          </Field>

          {error ? (
            <p className="rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive" role="alert">
              {error}
            </p>
          ) : null}

          {/* 長いフォームでも保存ボタンが常に見えるよう、フッタはスクロール領域の下端に貼り付ける。 */}
          <DialogFooter className="sticky -bottom-6 z-10 -mx-6 -mb-6 border-t bg-card px-6 py-3 max-sm:-bottom-[max(1.25rem,env(safe-area-inset-bottom))] max-sm:-mx-5 max-sm:-mb-[max(1.25rem,env(safe-area-inset-bottom))] max-sm:px-5 max-sm:pb-[max(0.75rem,env(safe-area-inset-bottom))]">
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)} disabled={busy}>
              キャンセル
            </Button>
            <Button type="submit" disabled={busy}>
              {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
              {editing ? "新しいバージョンとして保存" : "作成"}
            </Button>
          </DialogFooter>
        </form>

        <FolderPicker
          open={pickerOpen}
          onOpenChange={setPickerOpen}
          onSelect={(f) => {
            if (!draft.scopeFolders.some((x) => x.id === f.id)) {
              set("scopeFolders", [...draft.scopeFolders, { id: f.id, name: f.name }]);
            }
          }}
        />
      </DialogContent>
    </Dialog>
  );
}

/// 削除ボタンの当たり判定（見た目は小さいまま、指では 44px 四方に広げる）。
const REMOVE_CHIP = cn(
  "relative -mr-1 flex size-5 items-center justify-center rounded-full text-muted-foreground outline-none",
  "hover:text-destructive focus-visible:ring-2 focus-visible:ring-ring",
  "pointer-coarse:after:absolute pointer-coarse:after:-inset-3 pointer-coarse:after:content-['']",
);
const REMOVE_ROW = cn(
  "flex size-8 shrink-0 items-center justify-center rounded-md text-muted-foreground outline-none",
  "hover:bg-accent hover:text-destructive focus-visible:ring-2 focus-visible:ring-ring pointer-coarse:size-11",
);

function Field({
  label,
  htmlFor,
  required,
  children,
}: {
  label: string;
  htmlFor?: string;
  required?: boolean;
  children: React.ReactNode;
}) {
  const Tag = htmlFor ? "label" : "span";
  return (
    <div className="space-y-1.5">
      <Tag htmlFor={htmlFor} className="block text-xs font-medium text-foreground/70">
        {label}
        {required ? <span className="ml-0.5 text-destructive">*</span> : null}
      </Tag>
      {children}
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
    <div className="space-y-1">
      <label htmlFor={htmlFor} className="block text-[11px] text-muted-foreground">
        {label}
      </label>
      {children}
    </div>
  );
}
