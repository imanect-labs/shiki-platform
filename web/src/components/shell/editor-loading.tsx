"use client";

/// 編集画面の読み込み中プレースホルダ（human 要望: ぐるぐるスピナーだけは安っぽい）と、
/// 文書エディタ共通の導線部品（戻る・状態バッジ・スマホの見出し・開けないときの案内）。
///
/// 読み込み中プレースホルダは、開こうとしている文書の「形」をスケルトンで先に見せ、shimmer を流して待ち時間を
/// 演出する。種別ごとに骨格を変える（doc=段落・slide=キャンバス＋フィルムストリップ・
/// sheet=グリッド）。ブランドの季節色をアクセントに小さく添える。

import { FolderOpen, type LucideIcon } from "lucide-react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import * as React from "react";

import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { canGoBackInApp } from "@/lib/in-app-history";
import { seasonVar, currentSeasonIndex } from "@/lib/season";
import { cn } from "@/lib/utils";

type Kind = "doc" | "slide" | "sheet";

function Bar({ w, className }: { w: string; className?: string }) {
  return <div className={cn("shiki-shimmer h-3.5 rounded-md", className)} style={{ width: w }} />;
}

function DocSkeleton() {
  return (
    <div className="mx-auto flex w-full max-w-2xl flex-col gap-3.5 px-2">
      <div className="shiki-shimmer mb-2 h-7 w-1/2 rounded-md" />
      <Bar w="100%" />
      <Bar w="96%" />
      <Bar w="88%" />
      <div className="h-2" />
      <div className="shiki-shimmer h-5 w-1/3 rounded-md" />
      <Bar w="92%" />
      <Bar w="99%" />
      <Bar w="70%" />
    </div>
  );
}

function SlideSkeleton() {
  return (
    <div className="mx-auto flex w-full max-w-3xl gap-4 px-2">
      <div className="hidden w-28 shrink-0 flex-col gap-2.5 sm:flex">
        {[0, 1, 2].map((i) => (
          <div key={i} className="shiki-shimmer aspect-video w-full rounded-md" />
        ))}
      </div>
      <div className="shiki-shimmer aspect-video flex-1 rounded-xl" />
    </div>
  );
}

function SheetSkeleton() {
  return (
    <div className="mx-auto w-full max-w-3xl px-2">
      <div className="overflow-hidden rounded-lg border border-border/60">
        {[0, 1, 2, 3, 4, 5].map((r) => (
          <div key={r} className="flex gap-px bg-border/40">
            {[0, 1, 2, 3].map((c) => (
              <div
                key={c}
                className={cn(
                  "h-8 flex-1 bg-card",
                  r === 0 && "bg-muted",
                )}
              >
                <div className="shiki-shimmer m-1.5 h-4 rounded-sm" style={{ opacity: r === 0 ? 0.9 : 0.6 }} />
              </div>
            ))}
          </div>
        ))}
      </div>
    </div>
  );
}

export function EditorLoading({
  kind = "doc",
  message,
}: {
  kind?: Kind;
  message?: string;
}) {
  const accent = seasonVar(currentSeasonIndex());
  return (
    <div
      className="flex h-full w-full flex-col items-center justify-center gap-8 px-6 py-10"
      aria-busy
      role="status"
      data-testid="editor-loading"
    >
      <div className="w-full opacity-70">
        {kind === "slide" ? <SlideSkeleton /> : kind === "sheet" ? <SheetSkeleton /> : <DocSkeleton />}
      </div>
      <div className="flex items-center gap-2.5 text-sm text-muted-foreground">
        {/* 季節色の小さなドットが脈打つ（スピナーより落ち着いた"生きている"合図）。 */}
        <span className="relative flex size-2.5">
          <span
            className="absolute inline-flex h-full w-full animate-ping rounded-full opacity-60"
            style={{ backgroundColor: accent }}
          />
          <span className="relative inline-flex size-2.5 rounded-full" style={{ backgroundColor: accent }} />
        </span>
        {message ?? "開いています…"}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// 文書エディタ共通の導線（戻る・状態バッジ・スマホの見出し・開けないときの案内）。
// 読み込み中・notfound・エラーでも同じ導線を出すため、各エディタページはページ最上位で
// `usePageChrome` を登録し、早期 return の画面にはここの部品を使う。
// ---------------------------------------------------------------------------

/// エディタの「戻る」。直前の画面がアプリ内ならそこへ、無ければ（新しいタブで直接開いた・
/// ログイン直後・外部サイトから来た等）ドライブへ。`history.length` だとアプリ外へ戻ってしまう。
export function useEditorBack(): { onClick: () => void; label: string } {
  const router = useRouter();
  return React.useMemo(
    () => ({
      label: "戻る",
      onClick: () => {
        if (canGoBackInApp()) router.back();
        else router.push("/drive");
      },
    }),
    [router],
  );
}

type BadgeTone = "neutral" | "success" | "draft" | "info" | "danger";

/// 色はセマンティック/四季トークン（status-tokens と同じ割り当て）。文字は前景色と混ぜて
/// 淡い塗りの上でも読める濃さにする（ライト/ダーク共通）。
const BADGE_TONES: Record<BadgeTone, string> = {
  neutral: "bg-muted text-muted-foreground",
  success:
    "bg-season-summer/12 text-[color-mix(in_oklab,var(--season-summer)_70%,var(--foreground))]",
  draft:
    "bg-season-autumn/14 text-[color-mix(in_oklab,var(--season-autumn)_70%,var(--foreground))]",
  info: "bg-primary/10 text-[color-mix(in_oklab,var(--primary)_80%,var(--foreground))]",
  danger: "bg-destructive/10 text-destructive",
};

/// 文書の状態を文字で示すバッジ（淡い塗り＋濃い文字・ドットは使わない）。
/// 「同期済み」「下書き・未保存」「閲覧のみ」など。
export function EditorStatusBadge({
  tone = "neutral",
  icon: Icon,
  className,
  children,
  ...props
}: React.ComponentProps<"span"> & { tone?: BadgeTone; icon?: LucideIcon }) {
  return (
    <span
      className={cn(
        "inline-flex shrink-0 select-none items-center gap-1 whitespace-nowrap rounded-full px-2 py-0.5 text-[11.5px] font-medium leading-4",
        BADGE_TONES[tone],
        className,
      )}
      {...props}
    >
      {Icon ? <Icon className="size-3.5 shrink-0" aria-hidden /> : null}
      {children}
    </span>
  );
}

/// スマホの本文先頭に置く文書名＋状態の行（PC はバーの title/subtitle に出すので出さない）。
/// ノートのように本文に大きな題名がある画面では `name` を省き、状態行だけを出す。
export function EditorMobileHeading({
  name,
  status,
  className,
}: {
  name?: React.ReactNode;
  /// 状態バッジ群（EditorStatusBadge・プレゼンス等）。
  status?: React.ReactNode;
  className?: string;
}) {
  if (!name && !status) return null;
  return (
    <div className={cn("flex min-w-0 flex-col gap-1 px-4 pb-2 pt-1 md:hidden", className)}>
      {status ? <div className="flex min-w-0 flex-wrap items-center gap-1.5">{status}</div> : null}
      {name ? (
        <h1 className="break-words text-xl font-bold leading-snug tracking-tight [overflow-wrap:anywhere]">
          {name}
        </h1>
      ) : null}
    </div>
  );
}

/// 文書を開けなかったとき（notfound・権限なし・サービス停止・エラー）の案内。
/// 必ず「ドライブへ戻る」を出し、画面ごとの導線（会話を開く・解錠など）を `actions` に足す。
export function EditorUnavailable({
  icon,
  title,
  description,
  actions,
  children,
}: {
  icon?: LucideIcon;
  title: string;
  description?: string;
  /// 「ドライブへ戻る」の前に並べる導線（主導線を先に置く）。
  actions?: React.ReactNode;
  /// 案内の下に置く補助 UI（パスワード付き共有リンクの解錠など）。
  children?: React.ReactNode;
}) {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-6 p-4 sm:p-6">
      <EmptyState
        icon={icon}
        title={title}
        description={description}
        className="w-full max-w-md"
        action={
          <div className="flex flex-wrap items-center justify-center gap-2">
            {actions}
            <Button asChild variant="outline" size="default">
              <Link href="/drive">
                <FolderOpen aria-hidden />
                ドライブへ戻る
              </Link>
            </Button>
          </div>
        }
      />
      {children}
    </div>
  );
}
