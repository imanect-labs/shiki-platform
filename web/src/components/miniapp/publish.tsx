"use client";

/// ミニアプリの下書きと公開（D-16・GAP-36）。
///
/// 保存は下書きのバージョンになり、共有した人（使う人）が開く・押すのは公開したバージョンだけ。作る人（editor）は
/// 下書きを試し、版を選んで公開する。前に公開していたバージョンを公開し直すと公開を戻せる。公開の前に、公開した
/// バージョンとの差（サーバーの `publish-preview` が決める）と、公開を止める誤り（画面の書き損じ）を見せる。共有している
/// 相手に、そのバージョンの部品に要る権限が足りなければ知らせる（公開は止めない・GAP-37）。

import * as React from "react";
import { History, Loader2, TriangleAlert, Undo2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { AccessReportView } from "@/components/share/access-report";
import {
  checkMiniAppAccess,
  previewMiniAppPublish,
  publishMiniApp,
  type MiniAppChange,
  type MiniAppVersion,
  type Publication,
  type PublishPreview,
  type ResolvedMiniApp,
} from "@/lib/artifact-api";
import { cn } from "@/lib/utils";

/// 保存した下書きの知らせ（次に起きることと、公開を止める書き損じ）。作成の知らせも同じ文を使う。
export function savedToast(saved: MiniAppVersion): { title: string; description: string } {
  const title = `v${saved.version} を下書きとして保存しました`;
  if (saved.warnings.length > 0) {
    return { title, description: `画面に直す箇所が ${saved.warnings.length} 件あります。直すまで公開できません。` };
  }
  // 画面の入力とワークフローの入力項目の食い違いは、公開を止めない（入力の画面で必須の値が欠けると実行が止まる）。
  if (saved.input_warnings.length > 0) {
    return {
      title,
      description: `画面の入力が、ワークフローの入力項目と ${saved.input_warnings.length} か所合っていません。「書く」か「組み立てる」で確かめてください。`,
    };
  }
  return {
    title,
    description:
      saved.published_version == null
        ? "公開すると、共有した人が使えるようになります。"
        : `公開するまで、共有した人には v${saved.published_version} が見えます。`,
  };
}

/// まだ公開していないアプリを共有するときの注記（共有しても、公開するまで相手は使えない）。
export const UNPUBLISHED_SHARE_NOTE = "まだ公開していません。公開するまで、共有した相手はこのアプリを使えません。";

/// 開いているバージョンの公開の状態（作る人にだけ出す）。
export type PublishState = "unpublished" | "published" | "draft" | "older";

export function publishState(app: ResolvedMiniApp): PublishState {
  const latest = app.latest_version ?? app.version;
  if (app.published_version == null) return "unpublished";
  if (app.version === app.published_version) return "published";
  return app.version === latest ? "draft" : "older";
}

const STATE_LABEL: Record<PublishState, string> = {
  unpublished: "未公開",
  published: "公開中",
  draft: "下書き",
  older: "前のバージョン",
};

/// 公開の状態と操作の帯（作る人だけ）。開いているバージョンを公開する・下書きを試す・公開中を見る・公開を戻す・
/// この内容を下書きにする。
export function MiniAppPublishBar({
  app,
  onShowVersion,
  onPublish,
  onRestore,
  restoring,
}: {
  app: ResolvedMiniApp;
  /// 別のバージョンを開く（`undefined` は公開しているバージョン）。
  onShowVersion: (version: number | undefined) => void;
  /// 公開の確認を開く。
  onPublish: (version: number) => void;
  /// 開いているバージョンの内容で新しい下書きを保存する。
  onRestore: (version: number) => void;
  restoring: boolean;
}) {
  const state = publishState(app);
  const published = app.published_version ?? null;
  const latest = app.latest_version ?? app.version;
  const previous = app.previous_published_version ?? null;
  // 公開を止めるかはサーバーが決める（止めるのは一度も公開していない版だけ・公開を戻すのは止めない）。
  const blocked = app.publish_blocked;
  const problemsNote =
    state === "published"
      ? "この画面には直す箇所があります。直すまで、次のバージョンを公開できません。"
      : blocked
        ? "直す箇所があるため、このバージョンは公開できません。"
        : "この画面には直す箇所があります。前に公開していたバージョンなので、公開を戻すことはできます。";

  const message = (() => {
    switch (state) {
      case "unpublished":
        return app.version === latest
          ? "まだ公開していません。公開するまで、共有した人はこのアプリを使えません。"
          : `v${app.version} を表示しています。まだ公開していません。`;
      case "published":
        return latest > app.version
          ? `共有した人が使っているバージョンです。下書きの v${latest} はまだ公開していません。`
          : "共有した人が使っているバージョンです。保存した変更は、公開するまで共有した人に見えません。";
      case "draft":
        return `下書きを試しています。共有した人には公開中の v${published} が見えます。ボタンを押すと、ワークフローは実際に動きます。`;
      case "older":
        return `v${app.version} を表示しています。共有した人には公開中の v${published} が見えます。`;
    }
  })();

  return (
    <section
      aria-label="公開の状態"
      data-testid="miniapp-publish-bar"
      data-state={state}
      className="mt-3 rounded-lg border border-border bg-secondary/40 px-3 py-2.5 text-sm"
    >
      <div className="flex flex-wrap items-start gap-x-2 gap-y-1">
        <span
          className={cn(
            "shrink-0 rounded px-1.5 py-0.5 text-xs font-semibold",
            state === "published" ? "bg-primary/10 text-primary" : "bg-amber-500/15 text-amber-700 dark:text-amber-400",
          )}
          data-testid="miniapp-publish-state"
        >
          {STATE_LABEL[state]}
        </span>
        <p className="min-w-0 flex-1 basis-56 text-foreground/80 [overflow-wrap:anywhere]">{message}</p>
      </div>
      {app.warnings.length > 0 ? (
        <div role="note" className="mt-2 space-y-1 text-xs text-amber-700 dark:text-amber-400" data-testid="miniapp-publish-problems">
          <p className="flex items-start gap-1.5 font-medium">
            <TriangleAlert className="mt-0.5 size-3.5 shrink-0" aria-hidden />
            {problemsNote}
          </p>
          <ul className="space-y-0.5 pl-5">
            {app.warnings.map((w, i) => (
              <li key={i} className="[overflow-wrap:anywhere]">
                {w.message}
              </li>
            ))}
          </ul>
        </div>
      ) : null}
      <div className="mt-2 flex flex-wrap gap-2">
        {state === "published" && latest > app.version ? (
          <Button size="sm" variant="outline" onClick={() => onShowVersion(latest)}>
            下書きを試す
          </Button>
        ) : null}
        {state !== "published" ? (
          <Button size="sm" disabled={blocked} onClick={() => onPublish(app.version)} data-testid="miniapp-publish">
            {app.version === latest ? "公開" : "このバージョンを公開"}
          </Button>
        ) : null}
        {published != null && state !== "published" ? (
          <Button size="sm" variant="outline" onClick={() => onShowVersion(undefined)}>
            公開中を見る
          </Button>
        ) : null}
        {state === "published" && previous != null ? (
          <Button size="sm" variant="ghost" onClick={() => onPublish(previous)}>
            <Undo2 className="size-4" aria-hidden />
            公開を v{previous} に戻す
          </Button>
        ) : null}
        {app.version !== latest ? (
          <Button size="sm" variant="ghost" disabled={restoring} onClick={() => onRestore(app.version)}>
            {restoring ? <Loader2 className="size-4 animate-spin" aria-hidden /> : <History className="size-4" aria-hidden />}
            この内容を下書きにする
          </Button>
        ) : null}
      </div>
    </section>
  );
}

/// 変わるところ 1 つの言い方。
export function describeChange(c: MiniAppChange): string {
  switch (c.kind) {
    case "description":
      return c.from ? `画面の見出し: 「${c.from}」から「${c.to}」へ` : `画面の見出し: 「${c.to}」`;
    case "screen":
      return "画面の説明・図・ボタン";
    case "skill":
      if (c.from_version == null) return `スキルを追加する（v${c.to_version}）`;
      if (c.to_version == null) return "スキルを外す";
      // 別のスキルに替えたかはサーバーが決める（版の番号だけでは見分けられない）。
      if (c.replaced) return `スキルを替える（v${c.to_version}）`;
      return `スキル: v${c.from_version} から v${c.to_version} へ`;
    case "workflow":
    case "data_view": {
      // 呼び名はエディターの「ワークフロー」「表に出すビュー」とそろえる（GAP-11）。
      const noun = c.kind === "workflow" ? "ワークフロー" : "ビュー";
      if (c.from_version == null) return `${noun}「${c.alias}」を追加する（v${c.to_version}）`;
      if (c.to_version == null) return `${noun}「${c.alias}」を外す`;
      const renamed = c.from_alias != null && c.from_alias !== c.alias;
      if (c.from_version === c.to_version) return `${noun}「${c.from_alias}」の名前を「${c.alias}」にする`;
      return `${noun}「${c.alias}」${renamed ? `（前の名前は「${c.from_alias}」）` : ""}: v${c.from_version} から v${c.to_version} へ`;
    }
  }
}

/// 公開の確認（公開したバージョンとの差・公開を止める誤り）。
export function PublishMiniAppDialog({
  open,
  onOpenChange,
  appId,
  version,
  publishedVersion,
  onPublished,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  appId: string;
  version: number;
  publishedVersion: number | null;
  onPublished: (p: Publication) => void;
}) {
  const [preview, setPreview] = React.useState<PublishPreview | null>(null);
  const [loadError, setLoadError] = React.useState<string | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [busy, setBusy] = React.useState(false);
  const [attempt, setAttempt] = React.useState(0);

  React.useEffect(() => {
    if (!open) return;
    let live = true;
    setPreview(null);
    setLoadError(null);
    setError(null);
    previewMiniAppPublish(appId, version)
      .then((p) => live && setPreview(p))
      .catch((e: unknown) => live && setLoadError(e instanceof Error ? e.message : String(e)));
    return () => {
      live = false;
    };
  }, [open, appId, version, attempt]);

  const publish = async () => {
    setBusy(true);
    setError(null);
    try {
      const p = await publishMiniApp(appId, version);
      onPublished(p);
      onOpenChange(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const description =
    publishedVersion == null
      ? "共有した人が、このアプリを使えるようになります。"
      : version < publishedVersion
        ? `共有した人の画面とボタンの実行先が、v${publishedVersion} から v${version} に戻ります。`
        : `共有した人の画面とボタンの実行先が、v${publishedVersion} から v${version} に変わります。`;
  const problems = preview?.problems ?? [];

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg" data-testid="miniapp-publish-dialog">
        <DialogHeader>
          <DialogTitle>v{version} を公開しますか？</DialogTitle>
          <DialogDescription>{description}</DialogDescription>
        </DialogHeader>
        {preview === null && !loadError ? (
          <div className="flex items-center gap-2 py-4 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin" aria-hidden />
            変わるところを読み込んでいます…
          </div>
        ) : loadError ? (
          <div className="space-y-2 text-sm text-destructive" role="alert">
            <p>変わるところを読み込めませんでした。{loadError}</p>
            <Button size="sm" variant="outline" onClick={() => setAttempt((n) => n + 1)}>
              もう一度読み込む
            </Button>
          </div>
        ) : preview ? (
          <div className="space-y-3 text-sm">
            <div>
              <h3 className="mb-1 text-xs font-medium text-foreground/70">
                {publishedVersion == null ? "公開する内容" : "変わるところ"}
              </h3>
              {preview.changes.length === 0 ? (
                <p className="text-muted-foreground">公開しているバージョンと同じ内容です。</p>
              ) : (
                <ul className="list-disc space-y-0.5 pl-5" data-testid="miniapp-publish-changes">
                  {preview.changes.map((c, i) => (
                    <li key={i} className="[overflow-wrap:anywhere]">
                      {describeChange(c)}
                    </li>
                  ))}
                </ul>
              )}
            </div>
            {/* 共有している相手が、このバージョンの部品に要る権限を持っているか（持ち主だけ・GAP-37）。 */}
            <AccessReportView
              load={() => checkMiniAppAccess(appId, { version })}
              reloadKey={attempt}
              mode="publish"
              subject="app"
            />
            {problems.length > 0 ? (
              <div role="note" className="space-y-1 rounded-lg border border-amber-500/30 bg-amber-500/5 px-3 py-2 text-xs text-amber-700 dark:text-amber-400">
                <p className="font-medium">直す箇所があるため、公開できません。</p>
                <ul className="space-y-0.5 pl-4">
                  {problems.map((p, i) => (
                    <li key={i} className="[overflow-wrap:anywhere]">
                      {p.message}
                    </li>
                  ))}
                </ul>
              </div>
            ) : (
              <p className="text-xs text-muted-foreground" data-testid="miniapp-publish-undo-note">
                {publishedVersion == null
                  ? "公開した後は、公開をやめられません。別のバージョンに切り替えることはできます。"
                  : "前に公開していたバージョンには、あとから戻せます。"}
              </p>
            )}
          </div>
        ) : null}
        {error ? (
          <p className="whitespace-pre-line text-sm text-destructive" role="alert">
            {error}
          </p>
        ) : null}
        <DialogFooter>
          <Button type="button" variant="ghost" onClick={() => onOpenChange(false)} disabled={busy}>
            キャンセル
          </Button>
          <Button type="button" onClick={() => void publish()} disabled={busy || !preview || problems.length > 0}>
            {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
            公開
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
