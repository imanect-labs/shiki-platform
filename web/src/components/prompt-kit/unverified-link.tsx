"use client";

/// 回答の出典に無いリンク（#1143）。行き先のホストと注意の印を添え、押したら確認してから開く。
///
/// 社内文書に仕込まれた「ここでパスワードを入力して」のような誘導を、弱いモデルはそのまま回答に書く。
/// リンクの見た目だけでは本物と区別できないので、出典に無い行き先は開く前に必ず一度止める。
/// 中クリック・Ctrl/Cmd クリックでも素通りさせない（新しいタブで直接開けない）。

import * as React from "react";
import { TriangleAlert } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { cn } from "@/lib/utils";

/// 確認ダイアログ（リンク・外部画像のリンクで共有する）。
export function UnverifiedLinkDialog({
  open,
  onOpenChange,
  href,
  label,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  href: string;
  label: string;
}) {
  const proceed = () => {
    onOpenChange(false);
    window.open(href, "_blank", "noopener,noreferrer");
  };
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md" data-testid="unverified-link-dialog">
        <DialogHeader>
          <DialogTitle>出典に無いリンクを開きますか？</DialogTitle>
          <DialogDescription>
            {"このリンクは回答の出典に含まれていません。社内文書などに書かれた内容に AI が誘導されて書いた" +
              "可能性があります。開いた先でパスワードや社員 ID などを求められても、入力しないでください。"}
          </DialogDescription>
        </DialogHeader>
        <div className="rounded-lg border border-border/60 bg-card/40 px-3 py-2.5">
          <p className="text-[13px] font-medium text-foreground">{label}</p>
          <p className="mt-1 break-all font-mono text-[12px] leading-relaxed text-muted-foreground" data-testid="unverified-link-url">
            {href}
          </p>
        </div>
        <DialogFooter>
          <Button type="button" variant="ghost" onClick={() => onOpenChange(false)} autoFocus>
            キャンセル
          </Button>
          <Button type="button" variant="outline" onClick={proceed} data-testid="unverified-link-open">
            開く
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/// 開く前に確認ダイアログを出す要素の属性。
///
/// **本物の `href` を持たせない**: `href` があると、右クリックの「新しいタブで開く」・スマホの長押し
/// メニューやリンクのプレビュー・タブバーへのドラッグ・書き出した PDF の中のリンクで、確認を通らずに
/// 開けてしまう（レビュー指摘）。リンクとして読み上げ・フォーカスできるよう role と tabIndex を付け、
/// 押す・Enter・Space で確認ダイアログを出す。行き先の URL はダイアログで見せる（コピーもそこでできる）。
export function useUnverifiedOpen() {
  const [open, setOpen] = React.useState(false);
  const show = (e: React.SyntheticEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setOpen(true);
  };
  return {
    open,
    setOpen,
    props: {
      role: "link" as const,
      tabIndex: 0,
      onClick: show,
      onKeyDown: (e: React.KeyboardEvent) => {
        if (e.key === "Enter" || e.key === " ") show(e);
      },
    },
  };
}

/// 出典に無いリンク。リンクの文字のあとに行き先と注意の印を添える。
export function UnverifiedLink({
  href,
  label,
  className,
  children,
}: {
  href: string;
  label: string;
  className?: string;
  children: React.ReactNode;
}) {
  const { open, setOpen, props } = useUnverifiedOpen();
  return (
    <>
      <a
        title={`出典に無いリンク。押すと確認してから開きます: ${href}`}
        data-testid="answer-link-unverified"
        className={cn("cursor-pointer", className)}
        {...props}
      >
        {children}
        {/* inline-block: リンクの下線を行き先の表示に引き継がない（本文のリンクと見分けやすくする） */}
        <span className="ml-1 inline-block whitespace-nowrap text-[0.85em] font-normal text-muted-foreground no-underline">
          <TriangleAlert
            aria-label="出典に無いリンク"
            className={cn("mr-0.5 inline size-[0.95em] -translate-y-px align-middle text-amber-600 dark:text-amber-400")}
          />
          {label}
        </span>
      </a>
      <UnverifiedLinkDialog open={open} onOpenChange={setOpen} href={href} label={label} />
    </>
  );
}
