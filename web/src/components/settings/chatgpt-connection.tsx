"use client";

import * as React from "react";
import { ExternalLink, Loader2, RotateCw, Unlink } from "lucide-react";

import {
  completeChatGptLogin,
  disconnectChatGpt,
  fetchChatGptStatus,
  startChatGptLogin,
  type ChatGptStatus,
} from "@/lib/chatgpt-api";
import { formatDateTime } from "@/lib/format";
import { invalidateLlmModels } from "@/lib/model-choice";
import { OpenAiLogo } from "@/components/icons/openai-logo";
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
import { Skeleton } from "@/components/ui/skeleton";
import { toast } from "@/components/ui/use-toast";
import { IconTile, SettingsEmpty, SettingsItem, SettingsSection, StatusDot } from "./settings-ui";

/// 戻り先の URL の形（貼り間違いをその場で知らせるための目安。最終判断はサーバ）。
const CALLBACK_PREFIX = "http://127.0.0.1:";

function errorMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/// 番号付きの手順 1 つ。
function Step({ n, title, children }: { n: number; title: string; children: React.ReactNode }) {
  return (
    <li className="flex gap-3">
      <span
        aria-hidden
        className="mt-px flex size-6 shrink-0 items-center justify-center rounded-full border text-xs font-semibold text-muted-foreground"
      >
        {n}
      </span>
      <div className="flex min-w-0 flex-1 flex-col gap-2.5">
        <div className="text-[13.5px] font-medium">{title}</div>
        {children}
      </div>
    </li>
  );
}

/// ログインのダイアログ。開いた時点で認可 URL を取り（ポップアップブロックを避けて通常のリンクで開く）、
/// 許可のあとブラウザが開いた `127.0.0.1` の URL を貼ってもらう。
function ConnectDialog({
  open,
  onOpenChange,
  onConnected,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onConnected: (s: ChatGptStatus) => void;
}) {
  const [authorizeUrl, setAuthorizeUrl] = React.useState<string | null>(null);
  const [urlError, setUrlError] = React.useState<string | null>(null);
  const [opened, setOpened] = React.useState(false);
  const [pasted, setPasted] = React.useState("");
  const [submitError, setSubmitError] = React.useState<string | null>(null);
  const [submitting, setSubmitting] = React.useState(false);
  const [attempt, setAttempt] = React.useState(0);
  const inputId = React.useId();
  const errorId = React.useId();

  // 開くたびに新しいログインを始める（前の途中状態はサーバ側で捨てられる）。
  React.useEffect(() => {
    if (!open) return;
    let active = true;
    setAuthorizeUrl(null);
    setUrlError(null);
    setOpened(false);
    setPasted("");
    setSubmitError(null);
    startChatGptLogin()
      .then((u) => active && setAuthorizeUrl(u))
      .catch((e: unknown) => active && setUrlError(errorMessage(e)));
    return () => {
      active = false;
    };
  }, [open, attempt]);

  const trimmed = pasted.trim();
  const looksWrong = trimmed !== "" && !trimmed.startsWith(CALLBACK_PREFIX);
  const shownError = submitError ?? (looksWrong ? "「http://127.0.0.1:」で始まる URL を貼ってください。" : null);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!trimmed || looksWrong || submitting) return;
    setSubmitting(true);
    setSubmitError(null);
    try {
      const status = await completeChatGptLogin(trimmed);
      onConnected(status);
    } catch (err) {
      setSubmitError(errorMessage(err));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={(o) => !submitting && onOpenChange(o)}>
      <DialogContent className="sm:max-w-lg">
        <form onSubmit={submit} className="flex flex-col gap-5">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <OpenAiLogo className="size-5 shrink-0" />
              ChatGPT と連携
            </DialogTitle>
            <DialogDescription>
              あなたの ChatGPT Plus / Pro のプラン枠で GPT モデルを使えるようにします。使えるのはあなただけです。
            </DialogDescription>
          </DialogHeader>

          <ol className="flex flex-col gap-5">
            <Step n={1} title="ChatGPT で許可する">
              <p className="text-[12.5px] leading-relaxed text-muted-foreground">
                新しいタブで ChatGPT が開きます。ログインして「許可」を選んでください。
              </p>
              {urlError ? (
                <div className="flex flex-wrap items-center gap-2 text-[12.5px] text-destructive" role="alert">
                  {urlError}
                  <Button type="button" variant="outline" size="sm" onClick={() => setAttempt((a) => a + 1)}>
                    <RotateCw aria-hidden />
                    やり直す
                  </Button>
                </div>
              ) : (
                <div>
                  <Button
                    asChild={authorizeUrl !== null}
                    variant={opened ? "outline" : "default"}
                    size="sm"
                    disabled={authorizeUrl === null}
                  >
                    {authorizeUrl ? (
                      <a href={authorizeUrl} target="_blank" rel="noopener noreferrer" onClick={() => setOpened(true)}>
                        <ExternalLink aria-hidden />
                        {opened ? "ChatGPT をもう一度開く" : "ChatGPT を開く"}
                      </a>
                    ) : (
                      <>
                        <Loader2 className="animate-spin" aria-hidden />
                        準備しています
                      </>
                    )}
                  </Button>
                </div>
              )}
            </Step>

            <Step n={2} title="開いたページの URL を貼る">
              <p className="text-[12.5px] leading-relaxed text-muted-foreground">
                {"許可すると「このサイトにアクセスできません」などのページ（127.0.0.1）が開きます。" +
                  "それで問題ありません。そのページのアドレスバーの URL をすべてコピーして、ここに貼ってください。"}
              </p>
              <label htmlFor={inputId} className="sr-only">
                ChatGPT から戻ったページの URL
              </label>
              <Input
                id={inputId}
                value={pasted}
                onChange={(e) => {
                  setPasted(e.target.value);
                  setSubmitError(null);
                }}
                placeholder="http://127.0.0.1:1455/auth/callback?code=…"
                autoComplete="off"
                spellCheck={false}
                inputMode="url"
                aria-invalid={shownError ? true : undefined}
                aria-describedby={shownError ? errorId : undefined}
                className="font-mono text-[12.5px] md:text-[12.5px]"
              />
              {shownError ? (
                <p id={errorId} role="alert" className="text-[12.5px] leading-relaxed text-destructive">
                  {shownError}
                </p>
              ) : (
                <p className="text-[12px] text-muted-foreground">15 分以内に貼ってください。</p>
              )}
            </Step>
          </ol>

          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={submitting}>
              キャンセル
            </Button>
            <Button type="submit" disabled={!trimmed || looksWrong || submitting || authorizeUrl === null}>
              {submitting ? <Loader2 className="animate-spin" aria-hidden /> : null}
              接続する
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/// 設定 → 接続済みアプリの「ChatGPT プラン」（#854・プレビュー環境だけで有効）。
///
/// 環境で無効（state=unavailable）なら何も出さない。トークンはサーバ側だけにあり、ここには来ない。
export function ChatGptConnectionSection() {
  const [status, setStatus] = React.useState<ChatGptStatus | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [reloadKey, setReloadKey] = React.useState(0);
  const [connectOpen, setConnectOpen] = React.useState(false);
  const [confirmOpen, setConfirmOpen] = React.useState(false);
  const [disconnecting, setDisconnecting] = React.useState(false);

  React.useEffect(() => {
    let active = true;
    setError(null);
    fetchChatGptStatus()
      .then((s) => active && setStatus(s))
      .catch((e: unknown) => active && setError(errorMessage(e)));
    return () => {
      active = false;
    };
  }, [reloadKey]);

  if (status?.state === "unavailable") return null;

  const disconnect = async () => {
    setDisconnecting(true);
    try {
      await disconnectChatGpt();
      // チャットのモデル選択が、解除した ChatGPT のモデルを出し続けないようにする。
      invalidateLlmModels();
      setConfirmOpen(false);
      toast({ title: "ChatGPT との連携を解除しました" });
      setReloadKey((k) => k + 1);
    } catch (e) {
      toast({ title: "連携を解除できませんでした", description: errorMessage(e), variant: "destructive" });
    } finally {
      setDisconnecting(false);
    }
  };

  const listed = status?.models ?? [];

  return (
    <SettingsSection
      title="ChatGPT プラン"
      description="あなたの ChatGPT Plus / Pro のプラン枠で GPT モデルを使います（プレビュー環境限定）。連携したあなただけが使えます。"
    >
      {error ? (
        <div className="flex flex-col items-start gap-3 px-5 py-5 text-[13px]">
          <p className="text-muted-foreground">連携の状態を読み込めませんでした（{error}）</p>
          <Button variant="outline" size="sm" onClick={() => setReloadKey((k) => k + 1)}>
            <RotateCw aria-hidden />
            再読み込み
          </Button>
        </div>
      ) : !status ? (
        <div className="px-5 py-4" aria-busy="true" aria-label="読み込み中">
          <Skeleton className="h-10 w-full rounded-xl" />
        </div>
      ) : (
        <>
          <ul>
            <SettingsItem
              icon={<IconTile icon={OpenAiLogo} />}
              title={
                <>
                  ChatGPT
                  {status.state === "connected" ? <StatusDot>接続中</StatusDot> : null}
                  {status.state === "needs_reauth" ? <StatusDot on={false}>再ログインが必要</StatusDot> : null}
                </>
              }
              meta={
                status.state === "connected" || status.state === "needs_reauth" ? (
                  <>
                    {status.email ? <span className="truncate">{status.email}</span> : null}
                    {status.connected_at ? <span>{formatDateTime(status.connected_at)} に接続</span> : null}
                  </>
                ) : status.state === "other_user" ? (
                  "この環境では別の利用者が連携しています"
                ) : (
                  "未接続"
                )
              }
              action={
                status.state === "connected" ? (
                  <Button variant="outline" size="sm" onClick={() => setConfirmOpen(true)}>
                    <Unlink aria-hidden />
                    <span className="max-sm:sr-only">連携を解除</span>
                  </Button>
                ) : status.state === "not_connected" ? (
                  <Button size="sm" onClick={() => setConnectOpen(true)}>
                    <OpenAiLogo />
                    ChatGPT でログイン
                  </Button>
                ) : null
              }
            />
          </ul>
          {status.state === "needs_reauth" ? (
            // 操作が 2 つあるので行を分ける（狭い幅でアカウントの表示を潰さない）。
            <div className="shiki-dash-top flex flex-wrap items-center justify-between gap-3 px-5 py-3.5">
              <p className="min-w-0 text-xs leading-relaxed text-muted-foreground">
                ChatGPT の認証の期限が切れました。再ログインするか、連携を解除してください。
              </p>
              <div className="flex shrink-0 items-center gap-2">
                <Button variant="outline" size="sm" onClick={() => setConfirmOpen(true)}>
                  <Unlink aria-hidden />
                  連携を解除
                </Button>
                <Button size="sm" onClick={() => setConnectOpen(true)}>
                  <OpenAiLogo />
                  再ログイン
                </Button>
              </div>
            </div>
          ) : null}
          {status.state === "connected" ? (
            listed.length > 0 ? (
              <div className="shiki-dash-top px-5 py-3.5">
                <div className="text-xs text-muted-foreground">このアカウントで使えるモデル</div>
                {listed.some((m) => m.enabled) ? null : (
                  <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                    組織の管理者が 管理コンソール → AI → モデル で公開すると、モデル選択に出ます。
                  </p>
                )}
                <ul className="mt-2 flex flex-wrap gap-1.5">
                  {listed.map((m) => (
                    <li
                      key={m.slug}
                      className="inline-flex items-center gap-1.5 rounded-md border px-2 py-0.5 text-xs"
                      title={m.enabled ? "モデル選択に出ています" : "この環境のモデル選択には出ていません"}
                    >
                      <span className={m.enabled ? undefined : "text-muted-foreground"}>{m.display_name}</span>
                      {m.enabled ? <StatusDot>選べます</StatusDot> : null}
                    </li>
                  ))}
                </ul>
              </div>
            ) : (
              <SettingsEmpty>使えるモデルの一覧を取得できませんでした。</SettingsEmpty>
            )
          ) : null}
        </>
      )}

      <ConnectDialog
        open={connectOpen}
        onOpenChange={setConnectOpen}
        onConnected={(s) => {
          // 使えるようになったモデルをチャットのモデル選択に出す。
          invalidateLlmModels();
          setStatus(s);
          setConnectOpen(false);
          toast({
            title: "ChatGPT と連携しました",
            description: s.models.some((m) => m.enabled)
              ? "モデル選択に ChatGPT のモデルが出ます。"
              : "組織の管理者がモデルを公開すると、モデル選択に出ます。",
          });
        }}
      />

      <Dialog open={confirmOpen} onOpenChange={(o) => !disconnecting && setConfirmOpen(o)}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>ChatGPT との連携を解除しますか？</DialogTitle>
            <DialogDescription>
              解除すると、ChatGPT のモデルは使えなくなります。もう一度使うには、再度ログインしてください。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setConfirmOpen(false)} disabled={disconnecting}>
              キャンセル
            </Button>
            <Button variant="destructive" onClick={disconnect} disabled={disconnecting}>
              {disconnecting ? <Loader2 className="animate-spin" aria-hidden /> : <Unlink aria-hidden />}
              連携を解除
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </SettingsSection>
  );
}
