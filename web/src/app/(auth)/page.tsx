"use client";

import * as React from "react";
import { useRouter } from "next/navigation";
import { Leaf } from "lucide-react";

import { useMe } from "@/hooks/use-me";
import { useMediaQuery } from "@/hooks/use-media-query";
import { useHomeFeed } from "@/hooks/use-home-feed";
import {
  createThread,
  setAutonomousMode,
  type Attachment,
  type AutonomousMode,
  type WorkspaceChoice,
} from "@/lib/chat-api";
import type { ActiveCommand } from "@/lib/slash-command";
import { stashPending } from "@/lib/pending-message";
import { storageScopeGeneration } from "@/lib/user-storage";
import { HOME_SCOPE, moveModelChoice } from "@/lib/model-choice";
import { moveToolChoice, readToolChoice, writeToolChoice } from "@/lib/tool-choice";
import { titleFrom } from "@/lib/chat-store";
import { currentSeasonIndex, seasonVar } from "@/lib/season";
import { microSeasonOf } from "@/lib/microseason";
import {
  createKindDef,
  createPrompt,
  homeModeDef,
  homeSuggestions,
  type CreateKind,
  type HomeMode,
  type Suggestion,
} from "@/lib/home-modes";
import { type ArtifactMeta } from "@/lib/artifact-api";
import { Skeleton } from "@/components/ui/skeleton";
import { Composer } from "@/components/chat/composer";
import { ComposerArrow } from "@/components/home/composer-arrow";
import { ContinueSection } from "@/components/home/continue-section";
import { HomeModeOptions } from "@/components/home/home-mode-options";
import { HomeModeTabs } from "@/components/home/home-mode-tabs";
import { PromptSuggestions } from "@/components/chat/prompt-suggestions";
import { ShortcutGrid } from "@/components/home/shortcut-grid";
import { toast } from "@/components/ui/use-toast";

/// ホーム＝新規チャットの起点。中央のコンポーザに入力するとスレッドを作成して会話画面へ
/// 遷移し、最初のメッセージは会話画面側で SSE 送信する。
///
/// 上から: 日付と七十二候 → あいさつ → モード切替とその設定 → コンポーザ → 候補 → 葉の区切り →
/// 続きから／最近の共有 → ショートカット（#964）。
export default function HomePage() {
  const router = useRouter();
  const { data, loading } = useMe();
  const feed = useHomeFeed();
  const [starting, setStarting] = React.useState(false);
  const [mode, setModeState] = React.useState<HomeMode>("chat");
  // 選択中の skill（チャットモードで次に開始するチャットへ version 込みでピンされる・Phase 6）。
  const [skill, setSkill] = React.useState<ArtifactMeta | null>(null);
  // エージェントモードのワークスペースの作成場所と承認モード（Phase 6 UX・#350）。
  const [workspace, setWorkspace] = React.useState<WorkspaceChoice | null>(null);
  const [approvalMode, setApprovalMode] = React.useState<AutonomousMode>("require_approval");
  const [createKind, setCreateKind] = React.useState<CreateKind>("note");
  // 表示名はメールのローカル部から導出する（表示名フィールドはサーバ側実装が入る後続 PR で対応）。
  const name = data?.email?.split("@")[0] ?? null;
  const desktop = useMediaQuery("(min-width: 768px)");
  // 会話画面へ移る前に画面を離れていたら、遷移で引き戻さない（承認モードの設定を待つ間に別の画面へ行った等）。
  const mounted = React.useRef(true);
  React.useEffect(
    () => () => {
      mounted.current = false;
    },
    [],
  );

  // スキルはチャットのモードでだけ選べる。他のモードへ移ったら、見えない選択を残さない。
  const setMode = (next: HomeMode) => {
    if (next !== "chat") setSkill(null);
    setModeState(next);
  };

  const sharedFiles = React.useMemo(
    () => (feed.shares ?? []).filter((n) => n.kind !== "folder"),
    [feed.shares],
  );
  const suggestions = React.useMemo(
    () => homeSuggestions(mode, sharedFiles),
    [mode, sharedFiles],
  );

  const startChat = async (
    text: string,
    attachments: Attachment[],
    _context?: unknown,
    command?: ActiveCommand,
  ) => {
    if (starting || !text.trim()) return;
    setStarting(true);
    const startedAt = storageScopeGeneration();
    try {
      // スラッシュコマンドは skill を確実にピンし、長ホライズン（エージェントモード）で開始する。
      // 通常チャットは max_steps=6 で、質問→計画→調査のような多段の作法が途中で切れるため。
      const commanded = command != null;
      const asAutonomous = mode === "agent" || commanded;
      // 作成モードは作るものの種類を先頭に添える（会話に出る発話もこの文。隠れた指示は足さない）。
      const body = mode === "create" && !commanded ? createPrompt(createKind, text) : text;
      // スキルピッカー（チャットモード）の選択は**ピン**（このスレッドで最初からロード済み）。
      // コマンドは**その発話だけ**の適用なので、ピンにはせず送信時に渡す（#387）。
      const pinned = mode === "chat" ? skill : null;
      // 作業フォルダはエージェントのモードで選んだときだけ使う（他のモードでは見えていない選択）。
      const thread = await createThread(titleFrom(body), asAutonomous, {
        skill: pinned ? { artifactId: pinned.id, version: pinned.currentVersion } : undefined,
        workspace: mode === "agent" ? workspace ?? undefined : undefined,
      });
      // 承認モードは、エージェントのモードで利用者が明示的に選んだときだけ緩める（Codex P1）。
      // コマンドを選ぶ行為は「長ホライズンで実行してよい」の同意であって「事前許可の範囲を
      // 広げてよい」の同意ではない。thread に永続する設定なので、最初の発話を送る前に確定させる。
      if (mode === "agent" && approvalMode !== "require_approval") {
        const applied = await setAutonomousMode(thread.id, approvalMode)
          .then((r) => r.mode)
          .catch(() => null);
        if (applied !== approvalMode) {
          toast({
            description:
              "選んだ承認モードにできませんでした（組織の設定で禁止されている可能性があります）。承認必須で始めます。",
          });
        }
      }
      stashPending(
        thread.id,
        {
          text: body,
          attachments,
          autonomous: asAutonomous,
          skills: command
            ? [{ artifactId: command.skillId, version: command.skillVersion }]
            : undefined,
        },
        startedAt,
      );
      // ホームで選んだモデルを新しいスレッドへ引き継ぐ（ホーム側は Auto に戻る・#553）。
      moveModelChoice(HOME_SCOPE, thread.id);
      // ツールの選択も最初の発話に載せるため新しいスレッドへ移す（#734）。
      moveToolChoice(HOME_SCOPE, thread.id);
      // 社内検索のモードは、最初の発話で社内文書検索を必ず使う（その発話にだけ効く選択）。
      if (mode === "search") {
        const tools = readToolChoice(thread.id);
        if (!tools.includes("doc_search")) writeToolChoice(thread.id, [...tools, "doc_search"]);
      }
      if (mounted.current) router.push(`/c/${thread.id}`);
    } catch {
      toast({ description: "チャットを開始できませんでした。ログイン状態をご確認ください。" });
      setStarting(false);
    }
  };

  // スマホの入力欄は 1 行なので、短い文言にする（長い文は途中で切れて読めない）。
  const def = mode === "create" ? createKindDef(createKind) : homeModeDef(mode);
  const placeholder = desktop ? def.placeholder : def.short;
  const pick = (s: Suggestion) => startChat(s.text, s.attachment ? [s.attachment] : []);

  // スマホは上寄せ（モードを切り替えて設定の段の高さが変わっても、あいさつ・切替が上下に動かない）。
  return (
    <div className="mx-auto flex min-h-full w-full max-w-3xl flex-col px-4 pb-10 pt-[12vh] sm:justify-center sm:py-10">
      {/* スマホは要素を絞る（説明文・候補の注記・最近の共有・ショートカットは出さない。ナビはサイドバーにある）。 */}
      <div className="flex flex-col items-center gap-6 sm:gap-8">
        <div className="text-center">
          {loading ? (
            <>
              <Skeleton className="mx-auto mb-3 h-4 w-48" />
              <Skeleton className="mx-auto h-9 w-72" />
            </>
          ) : (
            <>
              <SeasonLine />
              <h1 className="text-[28px] font-semibold tracking-tight text-foreground sm:text-[32px]">
                {name ? `${name} さん、${timeGreeting(new Date())}` : "Shiki へようこそ"}
              </h1>
            </>
          )}
          <p className="mt-2 hidden text-sm text-muted-foreground sm:block">
            何でも尋ねてください。社内文書も自動で検索して答えます。
          </p>
        </div>

        <div className="flex w-full flex-col items-center gap-3">
          <HomeModeTabs value={mode} onChange={setMode} />
          <HomeModeOptions
            mode={mode}
            skill={skill}
            onSkillChange={setSkill}
            workspace={workspace}
            onWorkspaceChange={setWorkspace}
            approvalMode={approvalMode}
            onApprovalModeChange={setApprovalMode}
            createKind={createKind}
            onCreateKindChange={setCreateKind}
          />
          <div className="relative mt-1 w-full max-w-2xl">
            {/* 入力欄の右外から、手描き風の点線矢印で「ここから話しかけてね」を演出（文字に重ねない）。 */}
            <ComposerArrow className="absolute -right-28 -top-16 z-10 hidden xl:block" />
            <Composer
              onSubmit={startChat}
              // 書きかけは残す（別の画面へ行って戻っても続きから書ける）。
              draftKey="shiki.composer.draft:home"
              autoFocus
              disabled={starting}
              placeholder={placeholder}
              className="w-full"
              // モードの切替は入力欄の外（上）に置くので、入力欄にはトグルを出さない。
              // エージェントのときは「+」の作成の保存先にワークスペースを使う。
              autonomous={mode === "agent"}
              workspace={workspace}
            />
          </div>
        </div>

        <PromptSuggestions items={suggestions} onPick={pick} />

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

        <ContinueSection threads={feed.threads} shares={feed.shares} />

        <div className="hidden w-full sm:block">
          <ShortcutGrid />
        </div>
      </div>
    </div>
  );
}

/// 時間帯のあいさつ（端末の時刻）。
function timeGreeting(now: Date): string {
  const h = now.getHours();
  if (h >= 4 && h < 11) return "おはようございます";
  if (h >= 11 && h < 18) return "こんにちは";
  return "こんばんは";
}

const WEEKDAYS = ["日", "月", "火", "水", "木", "金", "土"] as const;

/// あいさつの上の一行（日付と七十二候）。葉の差し色は今の季節。
/// サーバと端末で時刻がずれるため、`useMe` の読み込み後（クライアント）にだけ描く。
function SeasonLine() {
  const now = new Date();
  const kou = microSeasonOf(now);
  return (
    <p className="mb-2 inline-flex items-center gap-1.5 text-[12.5px] text-muted-foreground">
      <Leaf
        className="size-3.5 shrink-0"
        style={{ color: seasonVar(currentSeasonIndex()) }}
        strokeWidth={2.5}
        aria-hidden
      />
      <span>
        {now.getMonth() + 1}月{now.getDate()}日（{WEEKDAYS[now.getDay()]}）・
        <span title={`二十四節気「${kou.term}」の候`}>
          {kou.name}
          <span className="hidden sm:inline">（{kou.reading}）</span>
        </span>
      </span>
    </p>
  );
}
