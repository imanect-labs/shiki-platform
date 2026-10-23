"use client";

/// ビュー切替と保存（Task 15.5）。
///
/// 一覧は「自分のビュー」と「共有されたビュー」（ReBAC viewer）。自分のビューの保存は新しい版の追記
/// （不変の版・他の人の画面は変わらない）、他人のビューの変更は「名前を付けて保存」で自分のビューにする。
/// 保存時はサーバが表示設定を検証し、拒否の理由（存在しない列・並べ替えに使えない列・ルックアップ列の
/// 上限など）をそのまま出す。

import { Check, ChevronDown, Eye, Loader2, Save, Share2, Table2, User } from "lucide-react";
import * as React from "react";

import { ArtifactShareDialog } from "@/components/artifacts/share-dialog";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { toast } from "@/components/ui/use-toast";
import { useMe } from "@/hooks/use-me";
import {
  createView,
  updateView,
  type DataTable,
  type DataViewBody,
  type DataViewSummary,
} from "@/lib/data-api";

export function ViewSwitcher({
  table,
  views,
  current,
  mine,
  dirty,
  body,
  onOpen,
  onSaved,
}: {
  table: DataTable;
  views: DataViewSummary[];
  current: { viewId: string | null; name: string; version: number | null };
  mine: boolean;
  dirty: boolean;
  body: DataViewBody;
  onOpen: (viewId: string | null) => void;
  onSaved: (id: string, version: number, name: string) => void;
}) {
  const me = useMe();
  const [saveAs, setSaveAs] = React.useState(false);
  const [name, setName] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const [shareOpen, setShareOpen] = React.useState(false);
  const own = views.filter((v) => v.owner === me.data?.id);
  const shared = views.filter((v) => v.owner !== me.data?.id);

  const fail = (e: unknown) =>
    toast({
      variant: "destructive",
      title: "ビューを保存できませんでした",
      description: e instanceof Error ? e.message : String(e),
    });

  const save = async () => {
    if (!current.viewId || current.version == null) return;
    setBusy(true);
    try {
      const v = await updateView(current.viewId, body, current.version);
      toast({ title: `「${current.name}」を保存しました`, description: `版 ${v.version}` });
      onSaved(v.id, v.version, current.name);
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  };

  const saveNew = async (e: React.FormEvent) => {
    e.preventDefault();
    const n = name.trim();
    if (!n) return;
    setBusy(true);
    try {
      const v = await createView(n, body);
      toast({ title: `ビュー「${n}」を保存しました` });
      setSaveAs(false);
      onSaved(v.id, v.version, n);
    } catch (err) {
      fail(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="flex items-center gap-1.5">
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button variant="outline" size="sm" className="h-8 max-w-56 gap-1.5 text-[13px]" data-testid="view-switcher">
            <Table2 className="size-4 text-muted-foreground" aria-hidden />
            <span className="truncate">{current.name}</span>
            {dirty ? <span className="size-1.5 shrink-0 rounded-full bg-primary" aria-label="未保存の変更" /> : null}
            <ChevronDown className="size-3.5 text-muted-foreground" aria-hidden />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="w-64">
          <DropdownMenuItem onSelect={() => onOpen(null)}>
            <Table2 className="size-4" aria-hidden />
            <span className="flex-1">既定のビュー</span>
            {current.viewId === null ? <Check className="size-4" aria-hidden /> : null}
          </DropdownMenuItem>
          {own.length ? (
            <>
              <DropdownMenuSeparator />
              <DropdownMenuLabel className="text-[11px] text-muted-foreground">自分のビュー</DropdownMenuLabel>
              {own.map((v) => (
                <DropdownMenuItem key={v.id} onSelect={() => onOpen(v.id)}>
                  <User className="size-4" aria-hidden />
                  <span className="flex-1 truncate">{v.name}</span>
                  {current.viewId === v.id ? <Check className="size-4" aria-hidden /> : null}
                </DropdownMenuItem>
              ))}
            </>
          ) : null}
          {shared.length ? (
            <>
              <DropdownMenuSeparator />
              <DropdownMenuLabel className="text-[11px] text-muted-foreground">共有されたビュー</DropdownMenuLabel>
              {shared.map((v) => (
                <DropdownMenuItem key={v.id} onSelect={() => onOpen(v.id)}>
                  <Eye className="size-4" aria-hidden />
                  <span className="flex-1 truncate">{v.name}</span>
                  {current.viewId === v.id ? <Check className="size-4" aria-hidden /> : null}
                </DropdownMenuItem>
              ))}
            </>
          ) : null}
        </DropdownMenuContent>
      </DropdownMenu>
      {mine && dirty ? (
        <Button size="sm" className="h-8 gap-1.5" onClick={save} disabled={busy} data-testid="view-save">
          {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : <Save className="size-4" aria-hidden />}
          保存
        </Button>
      ) : null}
      {dirty || current.viewId === null ? (
        <Button
          variant={mine ? "ghost" : "outline"}
          size="sm"
          className="h-8 text-[13px]"
          onClick={() => {
            setName(current.viewId ? `${current.name} のコピー` : "");
            setSaveAs(true);
          }}
          data-testid="view-save-as"
        >
          名前を付けて保存
        </Button>
      ) : null}
      {mine && current.viewId ? (
        <Button variant="ghost" size="icon" className="size-8" aria-label="ビューを共有" onClick={() => setShareOpen(true)}>
          <Share2 className="size-4" aria-hidden />
        </Button>
      ) : null}

      <Dialog open={saveAs} onOpenChange={setSaveAs}>
        <DialogContent className="max-w-sm">
          <form onSubmit={saveNew} className="space-y-4">
            <DialogHeader>
              <DialogTitle>ビューを保存</DialogTitle>
              <DialogDescription>
                今の列・絞り込み・並べ替えを「{table.name}」のビューとして保存します。共有しても、行は開いた人の権限で絞られます。
              </DialogDescription>
            </DialogHeader>
            <Input
              autoFocus
              aria-label="ビューの名前"
              placeholder="例: 承認待ち（自分の担当）"
              value={name}
              maxLength={128}
              onChange={(e) => setName(e.target.value)}
            />
            <DialogFooter>
              <Button type="button" variant="outline" onClick={() => setSaveAs(false)}>
                キャンセル
              </Button>
              <Button type="submit" disabled={busy || !name.trim()} data-testid="view-save-confirm">
                {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
                保存
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
      {current.viewId ? (
        <ArtifactShareDialog
          open={shareOpen}
          onOpenChange={setShareOpen}
          artifactId={current.viewId}
          name={current.name}
          shareUrl={typeof window !== "undefined" ? `${window.location.origin}/data/${table.id}?view=${current.viewId}` : undefined}
        />
      ) : null}
    </div>
  );
}
