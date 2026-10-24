"use client";

import * as React from "react";
import { AlertCircle } from "lucide-react";

import {
  CUSTOM_INSTRUCTION_MAX_CHARS,
  fetchCustomInstructions,
  instructionChars,
  saveCustomInstructions,
  type CustomInstructionTone,
  type CustomInstructionsDraft,
} from "@/lib/custom-instructions-api";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { toast } from "@/components/ui/use-toast";
import { SettingsPage, SettingsRow, SettingsSection, SoonBadge } from "./settings-ui";

const TONES: { value: CustomInstructionTone; label: string }[] = [
  { value: "default", label: "標準" },
  { value: "formal", label: "丁寧・かしこまった" },
  { value: "casual", label: "くだけた" },
  { value: "concise", label: "簡潔" },
];

/// 画面上の検証（サーバも同じ上限で検証する）。
export function validateCustomInstructions(d: CustomInstructionsDraft): string | null {
  if (instructionChars(d.about) > CUSTOM_INSTRUCTION_MAX_CHARS)
    return `「あなたについて」は ${CUSTOM_INSTRUCTION_MAX_CHARS.toLocaleString()} 文字以内で入力してください`;
  if (instructionChars(d.style) > CUSTOM_INSTRUCTION_MAX_CHARS)
    return `「答え方の希望」は ${CUSTOM_INSTRUCTION_MAX_CHARS.toLocaleString()} 文字以内で入力してください`;
  return null;
}

/// 保存すると変わるか（前後の空白だけの違いは変更としない。サーバが同じように整える）。
function changed(base: CustomInstructionsDraft, next: CustomInstructionsDraft): boolean {
  return (
    base.about.trim() !== next.about.trim() ||
    base.style.trim() !== next.style.trim() ||
    base.tone !== next.tone
  );
}

/// テキストエリアの下に出す文字数（上限を超えたら赤く）。
function CharCount({ id, text }: { id: string; text: string }) {
  const n = instructionChars(text);
  const over = n > CUSTOM_INSTRUCTION_MAX_CHARS;
  return (
    <p
      id={id}
      className={cn(
        "mt-1.5 text-right text-xs tabular-nums text-muted-foreground",
        over && "font-medium text-destructive",
      )}
      aria-live={over ? "polite" : undefined}
    >
      {n.toLocaleString()} / {CUSTOM_INSTRUCTION_MAX_CHARS.toLocaleString()}
    </p>
  );
}

/// 設定 → パーソナライズ。カスタム指示は保存され、以後の発話で AI の system プロンプトへ渡る（#900）。
/// 記憶・Web 検索の既定はまだ API が無いため「準備中」（操作できない）。
export function AiView() {
  const [base, setBase] = React.useState<CustomInstructionsDraft | null>(null);
  const [draft, setDraft] = React.useState<CustomInstructionsDraft | null>(null);
  const [loadError, setLoadError] = React.useState<string | null>(null);
  const [saving, setSaving] = React.useState(false);

  const load = React.useCallback(() => {
    setLoadError(null);
    fetchCustomInstructions()
      .then((v) => {
        setBase(v);
        setDraft(v);
      })
      .catch((e: unknown) => setLoadError(e instanceof Error ? e.message : String(e)));
  }, []);
  React.useEffect(load, [load]);

  const dirty = base != null && draft != null && changed(base, draft);
  const problem = draft ? validateCustomInstructions(draft) : null;

  const save = async () => {
    if (!draft || !dirty || problem || saving) return;
    setSaving(true);
    try {
      const saved = await saveCustomInstructions(draft);
      setBase(saved);
      setDraft(saved);
      toast({ title: "カスタム指示を保存しました", description: "次に送る発話から反映されます。" });
    } catch (e) {
      toast({
        title: "保存できませんでした",
        description: e instanceof Error ? e.message : String(e),
        variant: "destructive",
      });
    } finally {
      setSaving(false);
    }
  };

  /// テキストエリアで ⌘/Ctrl + Enter を押すと保存する。
  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey) && !e.nativeEvent.isComposing) {
      e.preventDefault();
      void save();
    }
  };

  const set = (patch: Partial<CustomInstructionsDraft>) => setDraft((d) => (d ? { ...d, ...patch } : d));

  return (
    <SettingsPage title="パーソナライズ" description="AI があなたに合わせて答えるための設定です。">
      <SettingsSection
        title="カスタム指示"
        description="チャットで発話を送るたびに、AI へ伝えます。ほかのメンバーには表示されません。"
      >
        {loadError ? (
          <EmptyState
            icon={AlertCircle}
            title="カスタム指示を読み込めませんでした"
            description={loadError}
            action={
              <Button variant="outline" size="sm" onClick={load}>
                再読み込み
              </Button>
            }
            className="m-4 py-10"
          />
        ) : !draft ? (
          <div className="flex flex-col gap-4 px-5 py-5" aria-busy="true" aria-label="読み込み中">
            <Skeleton className="h-4 w-28" />
            <Skeleton className="h-20 w-full" />
            <Skeleton className="h-4 w-28" />
            <Skeleton className="h-20 w-full" />
            <Skeleton className="h-9 w-48" />
          </div>
        ) : (
          <>
            <SettingsRow label="あなたについて" htmlFor="ai-about" description="職種・担当・よく扱う資料など。" stacked>
              <div className="w-full">
                <Textarea
                  id="ai-about"
                  value={draft.about}
                  onChange={(e) => set({ about: e.target.value })}
                  onKeyDown={onKeyDown}
                  placeholder="例: 法人営業。提案書と見積の作成が多い。"
                  rows={4}
                  className="w-full"
                  aria-describedby="ai-about-count"
                  aria-invalid={instructionChars(draft.about) > CUSTOM_INSTRUCTION_MAX_CHARS || undefined}
                />
                <CharCount id="ai-about-count" text={draft.about} />
              </div>
            </SettingsRow>
            <SettingsRow
              label="答え方の希望"
              htmlFor="ai-style"
              description="書き方・構成・出典の付け方など。"
              stacked
            >
              <div className="w-full">
                <Textarea
                  id="ai-style"
                  value={draft.style}
                  onChange={(e) => set({ style: e.target.value })}
                  onKeyDown={onKeyDown}
                  placeholder="例: 結論を先に。社内文書を引用するときは出典を必ず付ける。"
                  rows={4}
                  className="w-full"
                  aria-describedby="ai-style-count"
                  aria-invalid={instructionChars(draft.style) > CUSTOM_INSTRUCTION_MAX_CHARS || undefined}
                />
                <CharCount id="ai-style-count" text={draft.style} />
              </div>
            </SettingsRow>
            <SettingsRow label="口調">
              <Select value={draft.tone} onValueChange={(v) => set({ tone: v as CustomInstructionTone })}>
                <SelectTrigger className="h-9 w-full sm:w-48" aria-label="口調">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {TONES.map((t) => (
                    <SelectItem key={t.value} value={t.value}>
                      {t.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </SettingsRow>
          </>
        )}
      </SettingsSection>

      <SettingsSection
        title={
          <>
            記憶と検索
            <SoonBadge />
          </>
        }
      >
        <SettingsRow label="会話を記憶する" description="過去のチャットから好みや前提を覚え、次の回答に活かします。">
          <Switch checked={false} disabled aria-label="会話を記憶する" />
        </SettingsRow>
        <SettingsRow label="記憶を消去" description="これまでに覚えた内容をすべて忘れます。">
          <Button variant="outline" size="sm" disabled>
            記憶を消去
          </Button>
        </SettingsRow>
        <SettingsRow label="Web 検索を既定で使う" description="新しいチャットで Web 検索を最初から有効にします。">
          <Switch checked={false} disabled aria-label="Web 検索を既定で使う" />
        </SettingsRow>
      </SettingsSection>

      {dirty ? (
        <div className="sticky bottom-4 z-10 flex flex-wrap items-center justify-between gap-3 rounded-lg border bg-card px-4 py-3 shadow-md max-md:bottom-[max(0.75rem,env(safe-area-inset-bottom))]">
          <p className={cn("text-[13px]", problem ? "text-destructive" : "text-muted-foreground")} role="status">
            {problem ?? "保存していない変更があります"}
          </p>
          <div className="flex gap-2">
            <Button variant="outline" size="sm" onClick={() => setDraft(base)} disabled={saving}>
              取り消す
            </Button>
            <Button size="sm" onClick={() => void save()} loading={saving} disabled={!!problem}>
              変更を保存
            </Button>
          </div>
        </div>
      ) : null}
    </SettingsPage>
  );
}
