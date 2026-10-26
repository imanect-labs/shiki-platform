"use client";

/// スキルエディタ画面（#919）。ノートと同じ書き味の本文エディタに、プロパティ（知識スコープ・
/// 使うツール・モデル既定）と、AI アシスタント（save_skill の下書きを流し込む）を並べる。
///
/// - 新規: `/skills/new`（チャットの下書きカードからは `?thread=&name=` 付きで開き、その下書きを
///   下書きストアから読む。手編集も同じキーへ書き戻すので再読み込みで消えない）。
/// - 編集: `/skills/{id}`。保存は新しいバージョンとして作る（楽観ロックで版を照合）。
/// - 保存時の検証はサーバ（全件エラー）に委ね、エラーは本文の上にまとめて出す。

import { useRouter } from "next/navigation";
import { AlertCircle, ArrowLeft, Loader2, MessageSquare, Save } from "lucide-react";
import * as React from "react";

import type { SkillBody } from "@/generated/gui-spec";
import Link from "next/link";

import { useLocalActiveThreadStore, writeLocalActiveThread } from "@/components/chat/doc-chat-panel";
import { Button } from "@/components/ui/button";
import { AssistantLayout } from "@/components/shell/assistant-sheet";
import {
  EditorMobileHeading,
  EditorStatusBadge,
  EditorUnavailable,
} from "@/components/shell/editor-loading";
import { ChromePrimaryButton, usePageChrome } from "@/components/shell/page-header-context";
import { toast } from "@/components/ui/use-toast";
import { createSkill, getArtifact, getSkill, updateSkill } from "@/lib/artifact-api";
import { getThreadMessages } from "@/lib/chat-api";
import {
  applyAiSkill,
  draftFromBody,
  draftToBody,
  emptySkillDraft,
  parseSkillDraft,
  parseStoredSkillBody,
  skillDraftStore,
  type SkillDraft,
} from "@/lib/skills/skill-draft";
import { useStorageScopeReady } from "@/lib/user-storage";
import { cn } from "@/lib/utils";
import { SkillAssistant } from "./skill-assistant";
import { SkillBodyEditor } from "./skill-body-editor";
import { SkillExtras, SkillProperties } from "./skill-settings";

export type SkillEditorTarget =
  | { kind: "new"; threadId: string | null; draftName: string | null }
  | { kind: "edit"; id: string };

type Loaded =
  | { state: "loading" }
  | { state: "ready"; version: number | null }
  | { state: "error"; message: string };

/// 保存済みとの比較に使う正規形（名前＋保存する本文）。
function snapshotOf(d: SkillDraft): string {
  return JSON.stringify([d.name.trim(), draftToBody(d)]);
}

/// 会話の履歴から、指定名の最後の下書きスキルを探す（別端末・ストアが消えたときの復元）。
async function recoverFromThread(threadId: string, name: string): Promise<SkillBody | null> {
  const { messages } = await getThreadMessages(threadId);
  let found: SkillBody | null = null;
  for (const m of messages) {
    for (const b of m.content) {
      if (b.type !== "skill_draft") continue;
      const d = parseSkillDraft(b.draft);
      if (d && d.name === name) found = d.body;
    }
  }
  return found;
}

export function SkillEditorPage({ target }: { target: SkillEditorTarget }) {
  const router = useRouter();
  const isNew = target.kind === "new";
  const [loaded, setLoaded] = React.useState<Loaded>({ state: "loading" });
  const [draft, setDraft] = React.useState<SkillDraft>(() => emptySkillDraft());
  const draftRef = React.useRef(draft);
  draftRef.current = draft;
  const [saved, setSaved] = React.useState<string>(() => snapshotOf(emptySkillDraft()));
  const [seed, setSeed] = React.useState({ markdown: "", nonce: 0 });
  const [saving, setSaving] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  // 新規はアシスタントと一緒に始める（AI に下書きを頼む導線を最初から見せる）。
  const [chatOpen, setChatOpen] = React.useState(isNew);

  // 会話: 新規は URL（?thread=）、編集はスキルごとに覚えておく（次に開いたとき続きから）。
  const editThreads = useLocalActiveThreadStore(`skill:${target.kind === "edit" ? target.id : "new"}`);
  const [threadId, setThreadId] = React.useState<string | null>(() =>
    target.kind === "new" ? target.threadId : null,
  );
  React.useEffect(() => {
    if (target.kind === "edit") setThreadId(editThreads.read());
  }, [target.kind, editThreads]);
  React.useEffect(
    () => editThreads.subscribe(() => target.kind === "edit" && setThreadId(editThreads.read())),
    [editThreads, target.kind],
  );

  /// 本文ごと内容を置き換える（読み込み・AI の流し込み）。
  const replaceDraft = React.useCallback((next: SkillDraft) => {
    setDraft(next);
    setSeed((s) => ({ markdown: next.instructions, nonce: s.nonce + 1 }));
  }, []);

  // 読み込み。新規の下書きはユーザー確定（/me）後にストアを読む（#817）。
  const scopeReady = useStorageScopeReady();
  // 新規で会話を作ると URL に ?thread= が付くが、それで読み直す（編集中の内容を空に戻す）と
  // 流し込んだ下書きが消える。読み直すのは開く対象（スキル・チャットの下書き）が変わったときだけ。
  const targetKey =
    target.kind === "edit"
      ? `edit:${target.id}`
      : target.draftName
        ? `draft:${target.threadId}:${target.draftName}`
        : "new";
  React.useEffect(() => {
    let cancelled = false;
    setLoaded({ state: "loading" });
    setError(null);
    (async () => {
      if (target.kind === "edit") {
        const [meta, skill] = await Promise.all([getArtifact(target.id), getSkill(target.id)]);
        const next = draftFromBody(meta.name, skill.body);
        if (cancelled) return;
        replaceDraft(next);
        setSaved(snapshotOf(next));
        setLoaded({ state: "ready", version: skill.version });
        return;
      }
      const { threadId: t, draftName } = target;
      if (t && draftName) {
        if (!scopeReady) return;
        let body = parseStoredSkillBody(skillDraftStore.get(t, draftName)?.content ?? "");
        if (!body) {
          body = await recoverFromThread(t, draftName).catch(() => null);
          if (body) skillDraftStore.upsert(t, draftName, JSON.stringify(body), "ai");
        }
        if (cancelled) return;
        replaceDraft(body ? draftFromBody(draftName, body) : emptySkillDraft(draftName));
      } else {
        replaceDraft(emptySkillDraft());
      }
      setSaved(snapshotOf(emptySkillDraft()));
      setLoaded({ state: "ready", version: null });
    })().catch((e: unknown) => {
      if (!cancelled) {
        setLoaded({
          state: "error",
          message: e instanceof Error ? e.message : "スキルを読み込めませんでした",
        });
      }
    });
    return () => {
      cancelled = true;
    };
    // target は targetKey で比較する（毎レンダーの新しいオブジェクトで読み直さない）。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [targetKey, scopeReady, replaceDraft]);

  // チャットの下書きから開いた新規は、手編集も同じキーへ書き戻す（再読み込みで消さない）。
  const storeThread = target.kind === "new" ? target.threadId : null;
  const storeName = target.kind === "new" ? target.draftName : null;
  const storeKey = React.useMemo(
    () => (storeThread && storeName ? { thread: storeThread, name: storeName } : null),
    [storeThread, storeName],
  );
  const update = React.useCallback(
    <K extends keyof SkillDraft>(key: K, value: SkillDraft[K]) => {
      setDraft((d) => ({ ...d, [key]: value }));
    },
    [],
  );
  React.useEffect(() => {
    if (!storeKey || loaded.state !== "ready") return;
    const t = window.setTimeout(() => {
      skillDraftStore.upsert(storeKey.thread, storeKey.name, JSON.stringify(draftToBody(draft)), "user");
    }, 400);
    return () => window.clearTimeout(t);
  }, [draft, storeKey, loaded.state]);

  const onChangeMarkdown = React.useCallback(
    (markdown: string) => update("instructions", markdown),
    [update],
  );

  // AI の下書きをエディタへ流し込む（名前は未入力のときだけ・参照資料/コマンドは保つ）。
  const onSkillDraft = React.useCallback(
    (d: { name: string; body: SkillBody }) => {
      replaceDraft(applyAiSkill(draftRef.current, d.name, d.body));
      setError(null);
    },
    [replaceDraft],
  );

  const onThreadCreated = React.useCallback(
    (id: string) => {
      setThreadId(id);
      if (target.kind === "edit") {
        editThreads.write(id);
      } else {
        // 再読み込みでも同じ会話を続けられるよう URL に残す（履歴は増やさない）。
        const params = new URLSearchParams(window.location.search);
        params.set("thread", id);
        router.replace(`/skills/new?${params.toString()}`, { scroll: false });
      }
    },
    [target.kind, editThreads, router],
  );

  const dirty = loaded.state === "ready" && snapshotOf(draft) !== saved;
  const canSave = loaded.state === "ready" && !saving && (isNew ? draft.name.trim() !== "" : dirty);

  const save = React.useCallback(async () => {
    if (!canSave) return;
    const current = draftRef.current;
    setSaving(true);
    setError(null);
    try {
      if (target.kind === "edit") {
        const version = loaded.state === "ready" ? loaded.version : null;
        const res = await updateSkill(target.id, draftToBody(current), version ?? undefined);
        setSaved(snapshotOf(current));
        setLoaded({ state: "ready", version: res.version });
        toast({ title: `v${res.version} として保存しました` });
      } else {
        const res = await createSkill(current.name.trim(), draftToBody(current));
        if (storeKey) skillDraftStore.remove(storeKey.thread, storeKey.name);
        // 作成までの会話は、このスキルの会話として引き継ぐ（編集画面のアシスタントで続ける）。
        if (threadId) writeLocalActiveThread(`skill:${res.id}`, threadId);
        setSaved(snapshotOf(current));
        toast({ title: `「${current.name.trim()}」を作成しました` });
        router.replace(`/skills/${res.id}`);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "保存に失敗しました");
    } finally {
      setSaving(false);
    }
  }, [canSave, target, loaded, storeKey, threadId, router]);

  // Ctrl/⌘+S で保存する（ブラウザの「ページを保存」を出さない）。
  React.useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "s") {
        e.preventDefault();
        void save();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [save]);

  // 未保存の変更があるまま閉じようとしたら確かめる（下書きストアに残る新規は除く）。
  React.useEffect(() => {
    if (!dirty || storeKey) return;
    const onBeforeUnload = (e: BeforeUnloadEvent) => {
      e.preventDefault();
    };
    window.addEventListener("beforeunload", onBeforeUnload);
    return () => window.removeEventListener("beforeunload", onBeforeUnload);
  }, [dirty, storeKey]);

  const title = draft.name.trim() || "新しいスキル";
  const status = isNew ? (
    <EditorStatusBadge tone="draft">新規・未保存</EditorStatusBadge>
  ) : dirty ? (
    <EditorStatusBadge tone="draft">未保存の変更</EditorStatusBadge>
  ) : loaded.state === "ready" && loaded.version != null ? (
    <EditorStatusBadge tone="success">保存済み v{loaded.version}</EditorStatusBadge>
  ) : null;

  usePageChrome(
    () => ({
      title,
      menuTitle: title,
      subtitle: status,
      back: { href: "/skills", label: "スキル一覧" },
      primary:
        loaded.state === "ready" ? (
          <ChromePrimaryButton
            onClick={() => void save()}
            disabled={!canSave}
            data-testid="skill-save-button"
            title="保存（Ctrl+S）"
          >
            {saving ? <Loader2 className="animate-spin" aria-hidden /> : <Save aria-hidden />}
            <span>{isNew ? "作成" : "保存"}</span>
          </ChromePrimaryButton>
        ) : undefined,
      assistant: {
        active: chatOpen,
        onToggle: () => setChatOpen((v) => !v),
        testId: "skill-chat-toggle",
      },
      menu: threadId
        ? [{ key: "chat", label: "会話をチャットで開く", icon: MessageSquare, href: `/c/${threadId}` }]
        : [],
    }),
    [title, isNew, dirty, loaded, canSave, saving, chatOpen, threadId, save],
  );

  if (loaded.state === "error") {
    return (
      <EditorUnavailable
        title="スキルを開けませんでした"
        description={loaded.message}
        actions={
          <Button asChild>
            <Link href="/skills">
              <ArrowLeft aria-hidden />
              スキル一覧へ戻る
            </Link>
          </Button>
        }
      />
    );
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      <AssistantLayout
        open={chatOpen}
        onClose={() => setChatOpen(false)}
        label="スキルのアシスタント"
        panel={
          <SkillAssistant
            threadId={threadId}
            onThreadCreated={onThreadCreated}
            draft={draft}
            onSkillDraft={onSkillDraft}
            scopeKey={target.kind === "edit" ? target.id : "new"}
          />
        }
      >
        <div className="min-h-0 min-w-0 flex-1 overflow-y-auto overscroll-y-contain">
          <div className="mx-auto max-w-3xl px-4 pb-24 pt-6 max-md:pt-1">
            <EditorMobileHeading className="px-0 pb-2 pt-0" status={status} />
            {loaded.state === "loading" ? (
              <div className="flex items-center justify-center gap-2 py-24 text-sm text-muted-foreground">
                <Loader2 className="size-4 animate-spin" aria-hidden />
                読み込み中…
              </div>
            ) : (
              <>
                {error ? (
                  <div
                    role="alert"
                    className="mb-4 flex gap-2 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive"
                    data-testid="skill-save-error"
                  >
                    <AlertCircle className="mt-0.5 size-4 shrink-0" aria-hidden />
                    <p className="min-w-0 whitespace-pre-wrap break-words">{error}</p>
                  </div>
                ) : null}

                {isNew ? (
                  <input
                    value={draft.name}
                    onChange={(e) => update("name", e.target.value)}
                    placeholder="スキル名（例: expense-assistant）"
                    aria-label="名前"
                    className="w-full bg-transparent text-[1.75rem] font-bold leading-tight tracking-tight outline-none placeholder:text-muted-foreground/50 max-md:text-2xl"
                    autoCapitalize="none"
                    autoCorrect="off"
                    spellCheck={false}
                    data-testid="skill-name-input"
                  />
                ) : (
                  <h1 className="break-words text-[1.75rem] font-bold leading-tight tracking-tight [overflow-wrap:anywhere] max-md:text-2xl">
                    {draft.name}
                  </h1>
                )}
                <textarea
                  value={draft.description}
                  onChange={(e) => update("description", e.target.value)}
                  placeholder="説明: いつ使うスキルか（AI がスキルを選ぶ手がかりになります）"
                  aria-label="説明"
                  rows={1}
                  className={cn(
                    "mt-2 mb-4 w-full resize-none bg-transparent text-[15px] leading-relaxed text-foreground/80 outline-none [field-sizing:content]",
                    "placeholder:text-muted-foreground/60",
                  )}
                  data-testid="skill-description-input"
                />

                <SkillProperties draft={draft} update={update} />

                <div className="mt-6">
                  <SkillBodyEditor seed={seed} onChangeMarkdown={onChangeMarkdown} />
                </div>

                <SkillExtras draft={draft} update={update} />
              </>
            )}
          </div>
        </div>
      </AssistantLayout>
    </div>
  );
}
