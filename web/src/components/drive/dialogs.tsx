"use client";

import * as React from "react";
import { ChevronRight, Home, Loader2 } from "lucide-react";

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
import { useRestoreFocus } from "@/hooks/use-restore-focus";
import { breadcrumb, listChildrenUpTo, type NodeResponse } from "@/lib/storage";
import { cn } from "@/lib/utils";
import { NodeIcon } from "./primitives";

/// テキスト 1 入力のダイアログ（新規フォルダ / リネーム共用）。
export function TextPromptDialog({
  open,
  onOpenChange,
  title,
  description,
  label,
  initialValue = "",
  initialSelection,
  validate,
  submitLabel,
  onSubmit,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description?: string;
  label: string;
  initialValue?: string;
  /// 開いたときに選択しておく範囲（改名で拡張子の手前だけを選ぶ等）。省略時は全体。
  initialSelection?: [number, number];
  /// 入力中の検証。理由を返すとその場で表示し、確定できなくする（サーバも同じ規則で拒否する）。
  validate?: (value: string) => string | null;
  submitLabel: string;
  onSubmit: (value: string) => Promise<void>;
}) {
  const [value, setValue] = React.useState(initialValue);
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const inputRef = React.useRef<HTMLInputElement>(null);
  const invalid = validate ? validate(value) : null;
  // 初期の選択範囲を、利用者が触るまで当て続ける。メニューからダイアログへフォーカスが移る間に
  // Radix がフォーカスを入力へ戻し直し、そのたびに全体を選択するため、一度当てるだけでは消える。
  const selectionPendingRef = React.useRef(false);
  const applyInitialSelection = React.useCallback(() => {
    if (!initialSelection || !selectionPendingRef.current) return;
    const [start, end] = initialSelection;
    // Radix は focus() の直後に select() する。その後に上書きするため 1 フレーム待つ。
    window.requestAnimationFrame(() => {
      if (selectionPendingRef.current) inputRef.current?.setSelectionRange(start, end);
    });
  }, [initialSelection]);

  // 開くたびに初期値へ戻す。
  React.useEffect(() => {
    if (open) {
      setValue(initialValue);
      setError(null);
      setBusy(false);
      selectionPendingRef.current = true;
    }
  }, [open, initialValue]);

  // 開いた時点で入力にフォーカスが済んでいれば（autoFocus が先に当たり、Radix が追加のフォーカスを
  // 省いた場合）、onFocus は二度と来ないので、ここで選択を当てる。
  const applyRef = React.useRef(applyInitialSelection);
  applyRef.current = applyInitialSelection;
  React.useEffect(() => {
    if (open && document.activeElement === inputRef.current) applyRef.current();
  }, [open]);

  const submit = async () => {
    const trimmed = value.trim();
    if (!trimmed) {
      setError("名前を入力してください");
      return;
    }
    if (invalid) return;
    setBusy(true);
    setError(null);
    try {
      await onSubmit(trimmed);
      onOpenChange(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        className="max-w-md"
        // 既定のフォーカスは入力全体を選択する。範囲の指定があれば、その範囲だけを選ぶ。
        onOpenAutoFocus={
          initialSelection
            ? (e) => {
                e.preventDefault();
                inputRef.current?.focus();
                applyInitialSelection();
              }
            : undefined
        }
      >
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          {description ? <DialogDescription>{description}</DialogDescription> : null}
        </DialogHeader>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
          className="flex flex-col gap-3"
        >
          <label className="flex flex-col gap-1.5 text-sm">
            <span className="font-medium">{label}</span>
            <Input
              ref={inputRef}
              autoFocus
              value={value}
              onFocus={applyInitialSelection}
              onChange={(e) => {
                selectionPendingRef.current = false;
                setValue(e.target.value);
                // 「同じ名前があります」等のサーバ由来の理由は、直し始めたら消す。
                if (error) setError(null);
              }}
              onPointerDown={() => {
                selectionPendingRef.current = false;
              }}
              aria-invalid={error || invalid ? true : undefined}
              aria-describedby={error || invalid ? "text-prompt-error" : undefined}
            />
          </label>
          {error || invalid ? (
            <p id="text-prompt-error" role="alert" className="text-sm text-destructive">
              {invalid ?? error}
            </p>
          ) : null}
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)} disabled={busy}>
              キャンセル
            </Button>
            <Button type="submit" disabled={busy || Boolean(invalid)}>
              {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
              {submitLabel}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/// 汎用の確認ダイアログ（削除など）。失敗はダイアログの中に出して読み上げる。閉じたら、開いたときにフォーカスが
/// あった要素（開いたボタン）へフォーカスを戻す（Trigger を使わずに開くので、Radix に任せるとページの先頭に落ちる・
/// `useRestoreFocus`・#1207）。
export function ConfirmDialog({
  open,
  onOpenChange,
  title,
  description,
  confirmLabel,
  destructive,
  onConfirm,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description?: string;
  confirmLabel: string;
  destructive?: boolean;
  onConfirm: () => Promise<void>;
}) {
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const focus = useRestoreFocus();

  React.useEffect(() => {
    if (open) {
      setBusy(false);
      setError(null);
    }
  }, [open]);

  const confirm = async () => {
    setBusy(true);
    setError(null);
    try {
      await onConfirm();
      onOpenChange(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md" {...focus}>
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          {description ? <DialogDescription>{description}</DialogDescription> : null}
        </DialogHeader>
        {error ? (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        ) : null}
        <DialogFooter>
          <Button type="button" variant="ghost" onClick={() => onOpenChange(false)} disabled={busy}>
            キャンセル
          </Button>
          <Button
            type="button"
            variant={destructive ? "destructive" : "default"}
            onClick={() => void confirm()}
            disabled={busy}
          >
            {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
            {confirmLabel}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/// パンくずの 1 段（タッチでは高さ 40px）。
const CRUMB_BUTTON =
  "flex h-8 shrink-0 items-center rounded-md px-1.5 transition-colors hover:bg-accent hover:text-foreground pointer-coarse:h-10";
/// 名前の段（狭い幅では縮めて … で切る。flex にすると文字が切れずに隣の段へはみ出す）。
const CRUMB_NAME_BUTTON =
  "block h-8 min-w-0 max-w-[160px] truncate rounded-md px-1.5 leading-8 transition-colors hover:bg-accent hover:text-foreground pointer-coarse:h-10 pointer-coarse:leading-10";

/// パンくずの先頭（ドライブのトップ）。
const MOVE_ROOT: { id: string | null; name: string } = { id: null, name: "ドライブ" };

/// 移動先フォルダを選ぶダイアログ。フォルダ階層をたどって「ここへ移動」する。
/// 移動対象（とその子孫）は循環を避けるため選択不可（サーバも 400 で拒否）。
/// 開いたときは、移すものがいまあるフォルダーから始める（近くのフォルダーへ移すのに、トップから
/// たどり直させない・GAP-48）。いまある場所そのものへは移せない。
export function MoveDialog({
  open,
  onOpenChange,
  node,
  onMove,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  node: NodeResponse | null;
  /// `null` でルートへ移動。
  onMove: (destinationId: string | null) => Promise<void>;
}) {
  // 移動先ピッカーのパンくず（ルートからの階層）。最後の要素が現在地。
  // サブフォルダに入っても各階層をクリックして戻れる（履歴スタック）。
  const [path, setPath] = React.useState<{ id: string | null; name: string }[]>([MOVE_ROOT]);
  // 始める場所（いまあるフォルダーのパンくず）を読んでいる間は、一覧を読まない（トップや前に開いた場所を一度読んで
  // から移らない）。閉じている間は真にしておき、開いた最初の描画から待つ。
  const [starting, setStarting] = React.useState(true);
  const folderId = path[path.length - 1].id;
  const startId = node?.parent_id ?? null;
  const [folders, setFolders] = React.useState<NodeResponse[]>([]);
  const [cursor, setCursor] = React.useState<string | undefined>(undefined);
  const [hasMore, setHasMore] = React.useState(false);
  const [loading, setLoading] = React.useState(false);
  const [loadingMore, setLoadingMore] = React.useState(false);
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);

  // 開いたら、いまあるフォルダーから（そこまでのパンくずを読む）。読めなければドライブのトップから。
  React.useEffect(() => {
    if (!open) {
      setStarting(true);
      return;
    }
    setError(null);
    setBusy(false);
    if (!startId) {
      setPath([MOVE_ROOT]);
      setStarting(false);
      return;
    }
    let active = true;
    setStarting(true);
    breadcrumb(startId)
      .then((crumbs) => {
        if (active) setPath([MOVE_ROOT, ...crumbs.map((c) => ({ id: c.id, name: c.name }))]);
      })
      .catch(() => {
        if (active) setPath([MOVE_ROOT]);
      })
      .finally(() => {
        if (active) setStarting(false);
      });
    return () => {
      active = false;
    };
  }, [open, startId]);

  // 現在地のサブフォルダを先頭ページから読み込む（フォルダのみ表示）。
  React.useEffect(() => {
    if (!open || starting) return;
    let active = true;
    setLoading(true);
    setFolders([]);
    setCursor(undefined);
    setHasMore(false);
    // 見えないものが多いドライブのトップでは、短いページに続きの位置だけが付くことがある（数回たどる・BUG-187）。
    listChildrenUpTo({ parentId: folderId ?? undefined, sort: "name", want: 100 })
      .then((page) => {
        if (!active) return;
        setFolders(page.items.filter((n) => n.kind === "folder"));
        setCursor(page.next_cursor ?? undefined);
        setHasMore(Boolean(page.next_cursor));
      })
      .catch((e: unknown) => {
        if (active) setError(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (active) setLoading(false);
      });
    return () => {
      active = false;
    };
  }, [open, starting, folderId]);

  // 続きのページを読み込み、フォルダを追記する（>100 件でも移動先に辿り着ける）。
  const loadMoreFolders = async () => {
    if (!cursor || loadingMore) return;
    setLoadingMore(true);
    try {
      const page = await listChildrenUpTo({
        parentId: folderId ?? undefined,
        sort: "name",
        cursor,
        want: 100,
      });
      setFolders((prev) => [...prev, ...page.items.filter((n) => n.kind === "folder")]);
      setCursor(page.next_cursor ?? undefined);
      setHasMore(Boolean(page.next_cursor));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoadingMore(false);
    }
  };

  const move = async () => {
    setBusy(true);
    setError(null);
    try {
      await onMove(folderId);
      onOpenChange(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  // 移動対象自身の中へは入れられない（子孫への移動はサーバが 400 で弾く）。
  const sameAsSource = node?.kind === "folder" && folderId === node.id;
  // いまある場所へは移せない（移しても変わらない）。
  const here = node !== null && !starting && folderId === startId;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-xl">
        <DialogHeader>
          <DialogTitle>{node ? `「${node.name}」を移動` : "移動"}</DialogTitle>
          <DialogDescription>移動先のフォルダーを開いて、「ここへ移動」を押してください。</DialogDescription>
        </DialogHeader>

        {/* パンくず: 祖先はクリックで戻れる（現在地は非活性）。深い階層は中間を … にまとめ、
            1 行に収める（折り返して何段にもならないように）。… は 1 つ上の隠れた階層へ戻る。 */}
        <nav
          aria-label="移動先の階層"
          className="flex min-w-0 items-center gap-0.5 text-sm text-muted-foreground"
        >
          <Home className="size-4 shrink-0" aria-hidden />
          {path.map((seg, i) => {
            const isCurrent = i === path.length - 1;
            // 先頭（ドライブ）と末尾 2 つ以外は隠す。最初に隠れる位置に … を 1 つだけ出す。
            const hidden = path.length > 3 && i > 0 && i < path.length - 2;
            if (hidden && i !== 1) return null;
            return (
              <span
                key={`${seg.id ?? "root"}-${i}`}
                // 先頭（ドライブ）と … は縮めない。名前の段だけを縮める。
                className={cn("flex items-center gap-0.5", i === 0 || hidden ? "shrink-0" : "min-w-0")}
              >
                {i > 0 ? <ChevronRight className="size-3.5 shrink-0" aria-hidden /> : null}
                {hidden ? (
                  <button
                    type="button"
                    aria-label="上の階層へ戻る"
                    onClick={() => setPath((p) => p.slice(0, p.length - 2))}
                    className={CRUMB_BUTTON}
                  >
                    …
                  </button>
                ) : isCurrent ? (
                  <span className="min-w-0 max-w-[160px] truncate px-1 font-medium text-foreground">
                    {seg.name}
                  </span>
                ) : (
                  <button
                    type="button"
                    onClick={() => setPath((p) => p.slice(0, i + 1))}
                    className={i === 0 ? CRUMB_BUTTON : CRUMB_NAME_BUTTON}
                  >
                    {seg.name}
                  </button>
                )}
              </span>
            );
          })}
        </nav>

        {/* 一覧の高さはシート（92dvh）の中に収まる範囲に抑え、外側と二重に伸びないようにする。 */}
        <div className="max-h-[min(55dvh,28rem)] min-h-[200px] min-w-0 overflow-y-auto overscroll-contain rounded-lg border border-border">
          {loading || starting ? (
            <div className="flex items-center justify-center gap-2 py-6 text-sm text-muted-foreground">
              <Loader2 className="size-4 animate-spin" aria-hidden />
              読み込み中…
            </div>
          ) : folders.length === 0 && !hasMore ? (
            <p className="px-3 py-6 text-center text-sm text-muted-foreground">中にフォルダーはありません</p>
          ) : (
            <ul className="divide-y divide-border">
              {folders.map((f) => {
                const disabled = node?.kind === "folder" && f.id === node.id;
                return (
                  <li key={f.id}>
                    <button
                      type="button"
                      disabled={disabled}
                      onClick={() => setPath((p) => [...p, { id: f.id, name: f.name }])}
                      className={cn(
                        "flex w-full items-center gap-3 px-4 py-3 text-left text-[15px] transition-colors hover:bg-accent",
                        disabled && "cursor-not-allowed opacity-40 hover:bg-transparent",
                      )}
                    >
                      <NodeIcon kind="folder" className="size-6 shrink-0" />
                      <span className="flex-1 truncate">{f.name}</span>
                      <ChevronRight className="size-4 shrink-0 text-muted-foreground" aria-hidden />
                    </button>
                  </li>
                );
              })}
              {hasMore ? (
                <li>
                  <button
                    type="button"
                    onClick={() => void loadMoreFolders()}
                    disabled={loadingMore}
                    className="flex w-full items-center justify-center gap-2 px-3 py-2 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                  >
                    {loadingMore ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
                    もっと読み込む
                  </button>
                </li>
              ) : null}
            </ul>
          )}
        </div>

        {here ? (
          <p className="text-sm text-muted-foreground" data-testid="move-here-note">
            いまある場所です。移動先のフォルダーを開いてください。
          </p>
        ) : null}
        {error ? <p className="text-sm text-destructive">{error}</p> : null}

        <DialogFooter>
          <Button type="button" variant="ghost" onClick={() => onOpenChange(false)} disabled={busy}>
            キャンセル
          </Button>
          <Button type="button" onClick={() => void move()} disabled={busy || starting || sameAsSource || here}>
            {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
            ここへ移動
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
