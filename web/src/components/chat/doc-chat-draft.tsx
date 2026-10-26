"use client";

/// ドキュメントのチャットパネルの「まだ会話が無い」状態（最初の発話の入力欄）。
///
/// パネルを開いただけでは会話を作らない（開いただけで履歴に空の会話が並ぶのを防ぐ）。
/// 最初の発話を送った時点で親（[`DocChatPanel`]）が会話を作り、そのまま発話を送る。

import { Sparkles } from "lucide-react";
import * as React from "react";

import { Composer } from "@/components/chat/composer";
import type { Attachment } from "@/lib/chat-api";
import { setPendingSelection, type SelectionContext } from "@/lib/selection-context";
import type { ActiveCommand } from "@/lib/slash-command";

/// 会話を作る前の入力欄から送る最初の発話。
export type FirstMessage = {
  text: string;
  attachments: Attachment[];
  /// エディタの選択コンテキスト（選択→AI 指示）。
  context?: SelectionContext;
  /// エージェントモード（スラッシュコマンドなら常に true）。
  autonomous: boolean;
  /// **この発話にだけ**適用する skill（スラッシュコマンド・#387）。
  skills?: { artifactId: string; version?: number | null }[];
};

/// パネルの空会話の案内（空白のままにしない）。会話の作成前後で同じ見た目にするため共有する。
export function PanelEmptyHint() {
  return (
    <div
      className="flex flex-col items-center gap-2 px-6 py-14 text-center"
      data-testid="panel-empty-hint"
    >
      <Sparkles className="size-6 text-muted-foreground/70" aria-hidden />
      <p className="text-sm font-medium text-foreground">
        このドキュメントについて AI に相談できます
      </p>
      <p className="max-w-xs text-xs leading-relaxed text-muted-foreground">
        質問・要約・編集の依頼ができます。本文を選択して「AI
        に依頼」を押すと、選択箇所を指定して指示できます。
      </p>
    </div>
  );
}

export function DocChatDraft({
  scope,
  onStart,
  onDirtyChange,
  hint,
}: {
  /// モデル・ツールの選択の保存先（ホームや他の文書と共有しない）。作った会話へ引き継ぐ。
  scope: string;
  /// 会話を作って最初の発話を渡す。失敗したら reject する（入力欄に案内を出す）。
  onStart: (msg: FirstMessage) => Promise<void>;
  /// 入力欄に送れる入力（本文・コマンド・添付）があるか（書きかけ）が変わったときに呼ぶ。
  /// 外れる（アンマウント）ときは false。
  onDirtyChange?: (dirty: boolean) => void;
  /// 空の会話の案内（既定はドキュメント向けの [`PanelEmptyHint`]）。
  hint?: React.ReactNode;
}) {
  // エージェントモード（会話内の入力欄と同じく、この発話から切り替えられる）。
  const [autonomous, setAutonomous] = React.useState(false);
  const [starting, setStarting] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  React.useEffect(() => () => onDirtyChange?.(false), [onDirtyChange]);

  const submit = React.useCallback(
    (text: string, attachments: Attachment[], context?: SelectionContext, command?: ActiveCommand) => {
      if (starting) return;
      setStarting(true);
      setError(null);
      // スラッシュコマンドはこの発話にだけ skill を適用し、長ホライズンで実行する（会話内と同じ）。
      // 失敗したら入力欄が本文・添付・コマンドを戻す（restoreOnError）。選択箇所はここで戻す。
      return onStart({
        text,
        attachments,
        context,
        autonomous: autonomous || command != null,
        skills: command ? [{ artifactId: command.skillId, version: command.skillVersion }] : undefined,
      }).then(
        () => setStarting(false),
        (e: unknown) => {
          if (context) setPendingSelection(context);
          // 理由（許可されていないモデル等）を伝える。分からなければ一般的な案内にする。
          const reason = e instanceof Error && e.message ? `（${e.message}）` : "";
          setError(`送信できませんでした${reason}。もう一度お試しください。`);
          setStarting(false);
          throw e;
        },
      );
    },
    [starting, autonomous, onStart],
  );

  return (
    <div className="flex h-full min-h-0 flex-col" data-testid="doc-chat-draft">
      <div className="min-h-0 flex-1 overflow-y-auto px-4 py-5">
        {hint ?? <PanelEmptyHint />}
      </div>
      <div className="shrink-0 px-4 pt-2 pb-[max(0.75rem,env(safe-area-inset-bottom))] md:pb-3">
        {error ? (
          <p className="mb-2 text-center text-xs text-destructive" role="alert">
            {error}
          </p>
        ) : null}
        <Composer
          onSubmit={submit}
          disabled={starting}
          autonomous={autonomous}
          onAutonomousChange={setAutonomous}
          scope={scope}
          onHasInputChange={onDirtyChange}
          restoreOnError
          autoFocus
        />
      </div>
    </div>
  );
}
