"use client";

/// スキルエディタの AI アシスタント（#919）。ノート・スライド・CSV の下書きと同じく、
/// 右（スマホは下）のパネルで会話し、AI は save_skill で下書きスキルを返す。返った下書きは
/// その場でエディタへ流し込む（[`Conversation`] の `onSkillDraftOpened`）。
///
/// - 会話は**最初の発話を送ったとき**に作る（開いただけで空の会話を履歴に並べない）。
/// - 編集中の内容は選択コンテキスト（kind=skill_draft）として発話に添える。中身はデータとして
///   枠付きで AI へ渡り（サーバ側）、AI はそれを元に同じ name で save_skill を呼び直す。

import { Sparkles } from "lucide-react";
import * as React from "react";

import { Conversation } from "@/components/chat/conversation";
import { DocChatDraft, type FirstMessage } from "@/components/chat/doc-chat-draft";
import { SkillDraftApplyContext } from "@/components/chat/skill-draft-card";
import { createThread, postMessage } from "@/lib/chat-api";
import { moveModelChoice, readModelChoice } from "@/lib/model-choice";
import { clearPendingSelection, setPendingSelection } from "@/lib/selection-context";
import { draftContextExcerpt, type AiSkillDraft, type SkillDraft } from "@/lib/skills/skill-draft";
import { readToolChoice, settleToolChoice } from "@/lib/tool-choice";
import { storageScopeGeneration } from "@/lib/user-storage";

/// 編集内容を入力欄の添付（選択コンテキスト）へ反映するまでの待ち時間（打鍵ごとに作り直さない）。
const CONTEXT_DEBOUNCE_MS = 300;

function SkillAssistantHint() {
  return (
    <div className="flex flex-col items-center gap-2 px-6 py-14 text-center" data-testid="skill-assistant-hint">
      <Sparkles className="size-6 text-muted-foreground/70" aria-hidden />
      <p className="text-sm font-medium text-foreground">AI と一緒にスキルを作れます</p>
      <p className="max-w-xs text-xs leading-relaxed text-muted-foreground">
        「経費精算の質問に、規程を根拠に答えるスキルを作って」のように頼むと、指示文・説明・
        使うツールを下書きしてエディタに入れます。書いた内容の手直しも頼めます。
      </p>
    </div>
  );
}

export function SkillAssistant({
  threadId,
  onThreadCreated,
  draft,
  onSkillDraft,
  scopeKey,
}: {
  /// この編集で使っている会話（無ければ最初の発話で作る）。
  threadId: string | null;
  onThreadCreated: (threadId: string) => void;
  /// 編集中の内容（発話に添える）。
  draft: SkillDraft;
  /// AI の下書きを受けたとき（エディタへ流し込む）。
  onSkillDraft: (draft: AiSkillDraft) => void;
  /// モデル・ツールの選択の保存先の区別（スキルごと・新規）。
  scopeKey: string;
}) {
  // 編集中の内容を入力欄の選択コンテキストに置く（送信で消費され、次の編集でまた置く）。
  React.useEffect(() => {
    const t = window.setTimeout(() => {
      setPendingSelection({
        kind: "skill_draft",
        draft_name: draft.name.trim() || null,
        excerpt: draftContextExcerpt(draft),
      });
    }, CONTEXT_DEBOUNCE_MS);
    return () => window.clearTimeout(t);
  }, [draft]);
  // パネルを閉じた・画面を離れたら外す（他の画面の入力欄へ持ち込まない）。
  React.useEffect(() => () => clearPendingSelection(), []);

  const draftScope = `skilldraft:${scopeKey}`;
  // この画面で作った会話（最初の応答が購読より先に終わっていたら、読み込み時に下書きを拾う）。
  const [createdHere, setCreatedHere] = React.useState<string | null>(null);
  // 作ったが最初の発話を送れなかった会話（送り直しで使い回し、空の会話を増やさない）。
  const unsentRef = React.useRef<{ id: string; generation: number } | null>(null);

  const startConversation = React.useCallback(
    async (msg: FirstMessage) => {
      const startedAt = storageScopeGeneration();
      const unsent = unsentRef.current;
      unsentRef.current = null;
      let id = unsent && unsent.generation === startedAt ? unsent.id : null;
      if (!id) {
        const title = draft.name.trim() ? `スキル: ${draft.name.trim()}` : "スキルの作成";
        id = (await createThread(title, msg.autonomous)).id;
      }
      // 待つ間にログアウト（他タブを含む）があれば、前の人の発話・選択を送らない（#817）。
      if (startedAt !== storageScopeGeneration()) return;
      const tools = readToolChoice(draftScope);
      try {
        await postMessage(id, msg.text, msg.attachments, {
          agentMode: msg.autonomous,
          autonomous: msg.autonomous,
          context: msg.context,
          skills: msg.skills,
          model: readModelChoice(draftScope),
          tools,
        });
      } catch (e) {
        unsentRef.current = { id, generation: startedAt };
        throw e;
      }
      settleToolChoice(draftScope, tools);
      if (startedAt !== storageScopeGeneration()) return;
      moveModelChoice(draftScope, id);
      setCreatedHere(id);
      onThreadCreated(id);
    },
    [draft.name, draftScope, onThreadCreated],
  );

  return (
    <SkillDraftApplyContext.Provider value={onSkillDraft}>
      <div className="flex h-full min-h-0 w-full flex-col" data-testid="skill-assistant">
        {threadId ? (
          <Conversation
            key={threadId}
            threadId={threadId}
            variant="panel"
            onSkillDraftOpened={onSkillDraft}
            adoptSkillDraftOnLoad={createdHere === threadId}
          />
        ) : (
          <DocChatDraft scope={draftScope} onStart={startConversation} hint={<SkillAssistantHint />} />
        )}
      </div>
    </SkillDraftApplyContext.Provider>
  );
}
