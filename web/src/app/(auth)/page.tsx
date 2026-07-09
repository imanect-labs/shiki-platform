"use client";

import * as React from "react";
import { useRouter } from "next/navigation";
import { Leaf } from "lucide-react";

import { useMe } from "@/hooks/use-me";
import { createThread, type Attachment } from "@/lib/chat-api";
import { stashPending } from "@/lib/pending-message";
import { titleFrom } from "@/lib/chat-store";
import { currentSeasonIndex, seasonVar } from "@/lib/season";
import { Skeleton } from "@/components/ui/skeleton";
import { Composer } from "@/components/chat/composer";
import { ComposerArrow } from "@/components/home/composer-arrow";
import { SkillPicker } from "@/components/home/skill-picker";
import { type ArtifactMeta } from "@/lib/artifact-api";
import { type WorkspaceChoice } from "@/lib/chat-api";
import { PromptSuggestions } from "@/components/chat/prompt-suggestions";
import { ShortcutGrid } from "@/components/home/shortcut-grid";
import { toast } from "@/components/ui/use-toast";

/// ホーム＝新規チャットの起点。中央のコンポーザに入力するとスレッドを作成して会話画面へ
/// 遷移し、最初のメッセージは会話画面側で SSE 送信する。下部に候補プロンプトとショートカット。
export default function HomePage() {
  const router = useRouter();
  const { data, loading } = useMe();
  const [starting, setStarting] = React.useState(false);
  // 選択中の skill（次に開始するチャットへ version 込みでピンされる・Phase 6）。
  const [skill, setSkill] = React.useState<ArtifactMeta | null>(null);
  // エージェントモード（＝Autonomous）とワークスペースの作成場所（Phase 6 UX）。
  const [autonomous, setAutonomous] = React.useState(false);
  const [workspace, setWorkspace] = React.useState<WorkspaceChoice | null>(null);
  // 表示名はメールのローカル部から導出する（表示名フィールドはサーバ側実装が入る後続 PR で対応）。
  const name = data?.email?.split("@")[0] ?? null;

  const startChat = async (text: string, attachments: Attachment[]) => {
    if (starting || !text.trim()) return;
    setStarting(true);
    try {
      const thread = await createThread(titleFrom(text), autonomous, {
        // 選択時点の現行版をピンする（開始までに新版が保存されても選んだ版で適用）。
        skill: skill ? { artifactId: skill.id, version: skill.currentVersion } : undefined,
        workspace: autonomous ? workspace ?? undefined : undefined,
      });
      stashPending(thread.id, { text, attachments });
      router.push(`/c/${thread.id}`);
    } catch {
      toast({ description: "チャットを開始できませんでした。ログイン状態をご確認ください。" });
      setStarting(false);
    }
  };

  return (
    <div className="mx-auto flex min-h-full w-full max-w-3xl flex-col justify-center px-4 py-10">
      <div className="flex flex-col items-center gap-8">
        <div className="text-center">
          {loading ? (
            <Skeleton className="mx-auto h-9 w-72" />
          ) : (
            <h1 className="text-[28px] font-semibold tracking-tight text-foreground sm:text-[32px]">
              {name ? `${name} さん、こんにちは` : "Shiki へようこそ"}
            </h1>
          )}
          <p className="mt-2 text-sm text-muted-foreground">
            何でも尋ねてください。社内文書も自動で検索して答えます。
          </p>
        </div>

        <div className="relative w-full max-w-2xl">
          {/* 入力欄の右上から、手描き風の点線矢印で「ここから話しかけてね」を演出 */}
          <ComposerArrow className="absolute -top-16 right-4 z-10 hidden md:block" />
          <Composer
            onSubmit={startChat}
            autoFocus
            disabled={starting}
            className="w-full"
            autonomous={autonomous}
            onAutonomousChange={setAutonomous}
            workspace={workspace}
            onWorkspaceChange={setWorkspace}
          />
        </div>

        <SkillPicker selected={skill} onSelect={setSkill} />

        <PromptSuggestions onPick={(text) => startChat(text, [])} />

        {/* 荒い破線の区切り（中央に四季の葉を一つ・差し色＝今の季節・太枠のみ）。軽やかな「間」。 */}
        <div className="flex w-full max-w-2xl items-center gap-3 px-2" aria-hidden>
          <span className="shiki-dash-x h-4 flex-1" />
          <Leaf
            className="size-5 shrink-0"
            style={{ color: seasonVar(currentSeasonIndex()) }}
            strokeWidth={2.5}
          />
          <span className="shiki-dash-x h-4 flex-1" />
        </div>

        <ShortcutGrid />
      </div>
    </div>
  );
}
