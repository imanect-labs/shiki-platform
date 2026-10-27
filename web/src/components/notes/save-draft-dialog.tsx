"use client";

/// 下書きノートを「ドライブに保存」するダイアログ（issue #282）と、下書き画面（ノート・
/// スライド・CSV）共通のクローム・状態バッジ・タブ・空状態。
///
/// 名前と保存先フォルダを選び、POST /notes で実体化する。保存先の初期値は呼び出し側が
/// `defaultFolder` で渡す（AI の指定 → 由来ノートのフォルダ → ルートの順・#942）。任意で
/// フォルダを選び直せる（後で移動も可）。保存自体は呼び出し側（onConfirm）が行う。

import { FileWarning, Folder, FolderInput, MessageSquare, PencilLine, Save } from "lucide-react";
import Link from "next/link";
import * as React from "react";

import { FolderPicker } from "@/components/artifacts/folder-picker";
import { EditorStatusBadge, EditorUnavailable } from "@/components/shell/editor-loading";
import {
  ChromePrimaryButton,
  type ChromeMenuItem,
  type PageChromeSpec,
} from "@/components/shell/page-header-context";
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
import { useMe } from "@/hooks/use-me";
import { useStorageScopeReady } from "@/lib/user-storage";
import { cn } from "@/lib/utils";

export type SaveTarget = { name: string; parentId: string | null };

/// 保存の失敗をユーザー向けの文言にする。保存先フォルダへ書けない（403）ときは、選び直せば
/// 済むことを伝える（初期値は AI の指定や由来ノートの場所で、閲覧だけのフォルダのこともある）。
export function saveFailedMessage(e: unknown, parentId: string | null): string {
  const forbidden = e instanceof Error && /\(403\)/.test(e.message);
  return forbidden && parentId
    ? "このフォルダには保存する権限がありません。保存先を選び直してください。"
    : "保存に失敗しました。";
}

export function SaveDraftDialog({
  open,
  onOpenChange,
  defaultName,
  saving,
  onConfirm,
  entityLabel = "ノート",
  description = "下書きをノートとして保存します。保存後はバージョン管理・共有・共同編集ができます。",
  defaultFolder = null,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  defaultName: string;
  saving: boolean;
  onConfirm: (target: SaveTarget) => void;
  /// 保存対象の呼称（ノート/スライド/CSV・名前欄のラベルに使う）。
  entityLabel?: string;
  /// ダイアログの説明文（対象種別ごとに差し替え）。
  description?: string;
  /// 保存先の初期値（`null`=ルート・`undefined`=解決中）。
  defaultFolder?: { id: string; name: string } | null;
}) {
  const [name, setName] = React.useState(defaultName);
  const [folder, setFolder] = React.useState<{ id: string; name: string } | null>(null);
  const [pickerOpen, setPickerOpen] = React.useState(false);
  /// ユーザーが保存先を選び直したか（選び直した後は初期値で上書きしない）。
  const [folderTouched, setFolderTouched] = React.useState(false);

  // ダイアログを**開いた瞬間だけ**既定名へ戻す（open が false→true の遷移時のみ）。開いたまま
  // defaultName（呼び出し元の activeName）が AI 流し込みで変わっても、ユーザーの入力中の名前/保存先を
  // 黙ってリセットしない。
  const prevOpen = React.useRef(false);
  React.useEffect(() => {
    if (open && !prevOpen.current) {
      setName(defaultName);
      setFolderTouched(false);
    }
    prevOpen.current = open;
  }, [open, defaultName]);
  // 保存先の初期値は解決が遅れて届くことがあるので、選び直していない間は追従する。
  React.useEffect(() => {
    if (open && !folderTouched) setFolder(defaultFolder ?? null);
  }, [open, defaultFolder, folderTouched]);
  const chooseFolder = (next: { id: string; name: string } | null) => {
    setFolderTouched(true);
    setFolder(next);
  };

  // 保存先の初期値を解決している間は保存させない（意図せずルートへ作らない）。
  const resolving = defaultFolder === undefined && !folderTouched;
  const canSave = name.trim().length > 0 && !saving && !resolving;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>ドライブに保存</DialogTitle>
          <DialogDescription>{description}</DialogDescription>
        </DialogHeader>

        <div className="space-y-3">
          <label className="block space-y-1.5">
            <span className="text-sm font-medium">{entityLabel}名</span>
            <Input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={`${entityLabel}名`}
              data-testid="save-draft-name"
              autoFocus
            />
          </label>

          <div className="space-y-1.5">
            <span className="text-sm font-medium">保存先</span>
            <button
              type="button"
              onClick={() => setPickerOpen(true)}
              data-testid="save-draft-location"
              className="flex w-full items-center gap-2 rounded-md border border-border bg-card px-3 py-2 text-left text-sm transition-colors hover:border-primary/40 hover:bg-secondary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              <Folder className="size-4 shrink-0 text-muted-foreground" aria-hidden />
              <span className="min-w-0 flex-1 truncate">
                {resolving ? "保存先を確認中…" : folder ? folder.name : "マイドライブ（ルート）"}
              </span>
              <FolderInput className="size-4 shrink-0 text-muted-foreground" aria-hidden />
            </button>
            {folder ? (
              <button
                type="button"
                onClick={() => chooseFolder(null)}
                className="text-xs text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
              >
                ルートに戻す
              </button>
            ) : null}
          </div>
        </div>

        <DialogFooter>
          <Button
            type="button"
            variant="ghost"
            onClick={() => onOpenChange(false)}
            disabled={saving}
          >
            キャンセル
          </Button>
          <Button
            type="button"
            disabled={!canSave}
            data-testid="save-draft-confirm"
            onClick={() => onConfirm({ name: name.trim(), parentId: folder?.id ?? null })}
          >
            {saving ? "保存中…" : "保存"}
          </Button>
        </DialogFooter>

        {/* ピッカーは DialogContent の**中**に置く（React ツリー上の子ダイアログにする）。外の兄弟に
              置くと、ピッカーでの選択がこのダイアログの外側操作とみなされて一緒に閉じ、開き直すと
              保存先がルートへ戻る（＝サブフォルダへ保存できない）。 */}
        <FolderPicker
          open={pickerOpen}
          onOpenChange={setPickerOpen}
          purpose="destination"
          onSelect={(f) => chooseFolder({ id: f.id, name: f.name })}
        />
      </DialogContent>
    </Dialog>
  );
}

// ---------------------------------------------------------------------------
// 下書き画面（ノート・スライド・CSV）共通のクローム・状態バッジ・タブ・空状態。
// ---------------------------------------------------------------------------

/// 下書き画面の読み込み中表示。会話履歴からの復元はユーザー確定（/me）まで待つ（#817）ので、
/// /me が 401 以外（403・5xx・ネットワーク断）で失敗したときは待ち続けず、失敗を伝えて取り直せる
/// ようにする（401 はログイン画面へ送られるので待ち表示のままでよい）。
export function DraftLoading() {
  const scopeReady = useStorageScopeReady();
  const me = useMe();
  if (!scopeReady && me.error) {
    return (
      <div
        className="flex min-h-[40vh] flex-col items-center justify-center gap-2 text-center text-sm"
        role="alert"
      >
        <FileWarning className="size-6 text-muted-foreground/60" aria-hidden />
        <p className="text-destructive">下書きを読み込めませんでした。</p>
        <button
          type="button"
          onClick={me.reload}
          className="text-muted-foreground underline underline-offset-2 hover:text-foreground"
        >
          再試行
        </button>
      </div>
    );
  }
  return (
    <div className="flex min-h-[40vh] flex-col items-center justify-center gap-2 text-center text-sm text-muted-foreground">
      <FileWarning className="size-6 text-muted-foreground/60" aria-hidden />
      下書きを読み込んでいます…
    </div>
  );
}

/// 「下書き・未保存」バッジ（文字で示す・ドットは使わない）。
export function DraftBadge({ testId }: { testId?: string }) {
  return (
    <EditorStatusBadge tone="draft" icon={PencilLine} data-testid={testId}>
      下書き・未保存
    </EditorStatusBadge>
  );
}

/// 下書き画面のクローム: 戻る → 保存（主操作）→ アシスタント → ⋯（会話をチャットで開く）。
/// PC はバーに名前と「下書き・未保存」、スマホは本文先頭の状態行に出す（ページ側）。
export function draftChrome({
  back,
  threadId,
  name,
  canSave,
  onSave,
  chatOpen,
  onToggleChat,
  extraMenu = [],
  desktopExtra,
}: {
  back: NonNullable<PageChromeSpec["back"]>;
  threadId: string;
  name: string;
  canSave: boolean;
  onSave: () => void;
  chatOpen: boolean;
  onToggleChat: () => void;
  /// ⋯ に足す画面固有の項目（スライドのエクスポート・プレゼン等）。
  extraMenu?: ChromeMenuItem[];
  desktopExtra?: React.ReactNode;
}): PageChromeSpec {
  if (!threadId) return { back };
  const menu: ChromeMenuItem[] = [
    ...extraMenu,
    {
      key: "chat",
      label: "会話をチャットで開く",
      icon: MessageSquare,
      href: `/c/${threadId}`,
      separatorBefore: extraMenu.length > 0,
    },
  ];
  return {
    title: name,
    menuTitle: name,
    back,
    subtitle: <DraftBadge testId="draft-badge" />,
    desktopExtra,
    primary: (
      <ChromePrimaryButton onClick={onSave} disabled={!canSave} data-testid="draft-save-button">
        <Save aria-hidden />
        <span>
          <span className="max-md:hidden">ドライブに</span>保存
        </span>
      </ChromePrimaryButton>
    ),
    assistant: { active: chatOpen, onToggle: onToggleChat, testId: "note-chat-toggle" },
    menu,
  };
}

/// 同じ会話の複数下書きの切替タブ（別名 = 別下書き）。1 本だけなら出さない。
/// スマホでは折り返さず横スクロール（縦に積み上がって本文を押し下げない）。
export function DraftTabs({
  drafts,
  activeName,
  onOpen,
  className,
}: {
  drafts: ReadonlyArray<{ name: string }>;
  activeName: string;
  onOpen: (name: string) => void;
  className?: string;
}) {
  if (drafts.length <= 1) return null;
  return (
    <div
      className={cn(
        "flex items-center gap-1.5 md:flex-wrap",
        "max-md:overflow-x-auto max-md:overscroll-x-contain max-md:[scrollbar-width:none]",
        className,
      )}
      data-testid="draft-tabs"
    >
      {drafts.map((d) => (
        <button
          key={d.name}
          type="button"
          onClick={() => onOpen(d.name)}
          aria-current={d.name === activeName ? "true" : undefined}
          className={cn(
            "inline-flex shrink-0 items-center gap-1.5 rounded-full border px-3 py-1 text-[13px] transition-colors max-md:h-9",
            d.name === activeName
              ? "border-transparent bg-accent font-medium text-foreground"
              : "border-border/60 bg-card/40 text-muted-foreground hover:bg-secondary hover:text-foreground",
          )}
        >
          <PencilLine className="size-3.5 shrink-0" aria-hidden />
          <span className="max-w-[12rem] truncate">{d.name}</span>
        </button>
      ))}
    </div>
  );
}

/// 下書きが見つからない（?thread= なしで開いた等）ときの案内。チャットで作る導線と
/// ドライブへ戻る導線の 2 つを出す。
export function DraftNotFound({ what }: { what: string }) {
  return (
    <EditorUnavailable
      icon={FileWarning}
      title="下書きが見つかりません"
      description={`チャットから「${what}を作って」と頼むと、ここに下書きが用意されます。`}
      actions={
        <Button asChild>
          <Link href="/">
            <MessageSquare aria-hidden />
            チャットで作成する
          </Link>
        </Button>
      }
    />
  );
}
