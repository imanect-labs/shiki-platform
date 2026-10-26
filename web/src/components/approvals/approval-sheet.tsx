"use client";

/// 承認の詳細と判断（受信箱のシート・GAP-08）。
///
/// - 開いたら 1 件を取り直し、判断ごとに「いま実行できるか」を得る（表示の出し分けだけ・判断の API が改めて
///   認可する）。判断のボタンには文言と一緒に、進める状態とテーブル名を必ず出す（文言だけで判断させない）。
/// - 判断を選んでから送る（押し間違いで決めない）。コメント必須の判断はコメントが無いと送れない。
///   Ctrl+Enter（Mac は ⌘+Enter）で送る。Esc で閉じる。
/// - 送った事実の正はサーバ（送信後は一覧から外して読み直す・PIT-57）。
/// - PC は右からのシート、スマホは全画面のシート。

import * as React from "react";
import Link from "next/link";
import { ArrowRight, ExternalLink, Loader2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Sheet,
  SheetBody,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { Textarea } from "@/components/ui/textarea";
import { useMediaQuery } from "@/hooks/use-media-query";
import {
  ApprovalApiError,
  approvalErrorMessage,
  decideApproval,
  getApproval,
  type ApprovalDecisionKind,
  type ApprovalItem,
} from "@/lib/approvals-api";
import { cn } from "@/lib/utils";
import { deadlineText, fieldValueText, userLabel } from "./approval-values";

/// 判断の種別ごとの見た目（承認は主ボタン・却下は注意・差し戻しは控えめ）。
const KIND_TONE: Record<ApprovalDecisionKind, string> = {
  approve: "data-[selected=true]:border-primary data-[selected=true]:bg-primary/10",
  reject: "data-[selected=true]:border-destructive data-[selected=true]:bg-destructive/10",
  return: "data-[selected=true]:border-[var(--season-autumn)] data-[selected=true]:bg-[color-mix(in_oklab,var(--season-autumn)_10%,transparent)]",
};

export function ApprovalSheet({
  item,
  onClose,
  onDecided,
  onGone,
}: {
  /// 開く承認（null なら閉じている）。
  item: ApprovalItem | null;
  onClose: () => void;
  /// 判断を送れた（一覧から外して読み直す）。
  onDecided: (item: ApprovalItem) => void;
  /// もう判断できない（決まった・期限切れ・見えなくなった）と分かった。
  onGone: (item: ApprovalItem) => void;
}) {
  const desktop = useMediaQuery("(min-width: 768px)");
  return (
    <Sheet open={item !== null} onOpenChange={(open) => !open && onClose()}>
      <SheetContent
        side={desktop ? "right" : "bottom"}
        aria-label="承認の詳細"
        className={cn(desktop ? "max-w-lg" : "h-[100dvh] max-h-[100dvh] rounded-none")}
        data-testid="approval-sheet"
        onOpenAutoFocus={(e) => e.preventDefault()}
      >
        {item ? (
          <ApprovalDetail key={item.approval_id} item={item} onDecided={onDecided} onGone={onGone} />
        ) : null}
      </SheetContent>
    </Sheet>
  );
}

function ApprovalDetail({
  item: listed,
  onDecided,
  onGone,
}: {
  item: ApprovalItem;
  onDecided: (item: ApprovalItem) => void;
  onGone: (item: ApprovalItem) => void;
}) {
  // 1 件を取り直す（判断ごとに、いま実行できるかが付く）。取り直すまでは一覧の内容で見せ、ボタンは押せない。
  const [item, setItem] = React.useState<ApprovalItem>(listed);
  const [loaded, setLoaded] = React.useState(false);
  const [loadError, setLoadError] = React.useState<string | null>(null);
  const [choice, setChoice] = React.useState<ApprovalDecisionKind | null>(null);
  const [comment, setComment] = React.useState("");
  const [sending, setSending] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);

  React.useEffect(() => {
    let alive = true;
    getApproval(listed.approval_id)
      .then((d) => {
        if (!alive) return;
        setItem(d);
        setLoaded(true);
      })
      .catch((e) => {
        if (!alive) return;
        if (e instanceof ApprovalApiError && e.status === 404) {
          setLoadError("この承認はもう決まっているか、見られなくなりました。");
          onGone(listed);
        } else {
          setLoadError("詳細を読み込めませんでした。閉じてもう一度開いてください。");
        }
      });
    return () => {
      alive = false;
    };
  }, [listed, onGone]);

  const selected = item.decisions.find((d) => d.kind === choice) ?? null;
  const needsComment = Boolean(selected?.comment_required);
  const canSend =
    loaded && !sending && selected !== null && selected.allowed !== false && (!needsComment || comment.trim() !== "");

  const send = async () => {
    if (!canSend || !selected) return;
    setSending(true);
    setError(null);
    try {
      await decideApproval(item.approval_id, selected.kind, item.record.rev, comment);
      onDecided(item);
    } catch (e) {
      setError(approvalErrorMessage(e));
      // もう決まった・期限切れ・見えないは、一覧からも外す。
      if (e instanceof ApprovalApiError && (e.status === 404 || e.code === "decided" || e.code === "expired")) {
        onGone(item);
      } else if (e instanceof ApprovalApiError && e.code === "rev_conflict") {
        // ほかの人が行を更新した: 1 件を取り直して新しい rev で押し直せるようにする（選んだ判断とコメントは残す）。
        try {
          setItem(await getApproval(item.approval_id));
        } catch (again) {
          if (again instanceof ApprovalApiError && again.status === 404) onGone(item);
        }
      }
      setSending(false);
    }
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      void send();
    }
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col" onKeyDown={onKeyDown}>
      <SheetHeader className="pr-14">
        <SheetTitle className="text-[17px] leading-snug">{item.title}</SheetTitle>
        <SheetDescription className="text-xs">
          {item.table.name} ・ いまの状態「{item.from_state}」 ・ {deadlineText(item.deadline)}
        </SheetDescription>
      </SheetHeader>
      <SheetBody className="space-y-5">
        {item.instructions ? (
          <p className="whitespace-pre-wrap rounded-lg bg-muted/60 px-3 py-2.5 text-sm leading-relaxed">
            {item.instructions}
          </p>
        ) : null}
        <dl className="grid grid-cols-[minmax(5rem,auto)_1fr] gap-x-4 gap-y-2 text-sm">
          <dt className="text-muted-foreground">申請した人</dt>
          <dd className="min-w-0 break-words">{userLabel(item.record.owner)}</dd>
          {item.record.fields.map((f) => (
            <React.Fragment key={f.key}>
              <dt className="text-muted-foreground">{f.display_name}</dt>
              <dd className="min-w-0 whitespace-pre-wrap break-words">{fieldValueText(f.field_type, f.value)}</dd>
            </React.Fragment>
          ))}
        </dl>
        <Link
          href={`/data/${item.table.id}`}
          className="inline-flex items-center gap-1 text-xs text-primary underline-offset-2 hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <ExternalLink className="size-3.5" aria-hidden />
          テーブル「{item.table.name}」で開く
        </Link>

        {loadError ? (
          <p role="alert" className="rounded-md border border-destructive/40 bg-destructive/5 p-2.5 text-sm text-destructive">
            {loadError}
          </p>
        ) : (
          <fieldset className="space-y-2" disabled={sending}>
            <legend className="mb-1 text-sm font-medium">判断</legend>
            <div role="radiogroup" aria-label="判断" className="grid gap-2">
              {item.decisions.map((d) => {
                const blocked = d.allowed === false;
                return (
                  <button
                    key={d.kind}
                    type="button"
                    role="radio"
                    aria-checked={choice === d.kind}
                    data-selected={choice === d.kind}
                    disabled={!loaded || blocked}
                    onClick={() => setChoice(d.kind)}
                    className={cn(
                      "flex min-h-11 w-full items-center justify-between gap-3 rounded-lg border px-3 py-2 text-left text-sm transition-colors",
                      "hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                      "disabled:cursor-not-allowed disabled:opacity-50",
                      KIND_TONE[d.kind],
                    )}
                  >
                    <span className="font-medium">{d.label}</span>
                    <span className="flex min-w-0 items-center gap-1 text-xs text-muted-foreground">
                      <ArrowRight className="size-3.5 shrink-0" aria-hidden />
                      <span className="truncate">
                        状態を「{d.to}」へ
                        {d.comment_required ? "（コメント必須）" : ""}
                      </span>
                    </span>
                  </button>
                );
              })}
            </div>
            {loaded && item.decisions.every((d) => d.allowed === false) ? (
              <p className="text-xs text-muted-foreground">
                あなたは担当ですが、状態の遷移の規則ではこの承認を判断できません（表の管理者に確認してください）。
              </p>
            ) : null}
          </fieldset>
        )}

        {loadError ? null : (
          <div className="space-y-1.5">
            <label htmlFor="approval-comment" className="text-sm font-medium">
              コメント{needsComment ? <span className="text-destructive">（必須）</span> : "（任意）"}
            </label>
            <Textarea
              id="approval-comment"
              value={comment}
              maxLength={2000}
              rows={3}
              disabled={sending}
              aria-invalid={needsComment && comment.trim() === "" ? true : undefined}
              placeholder={needsComment ? "理由を書いてください" : "申請した人へのひとこと"}
              onChange={(e) => setComment(e.target.value)}
            />
          </div>
        )}
        {error ? (
          <p role="alert" className="rounded-md border border-destructive/40 bg-destructive/5 p-2.5 text-sm text-destructive">
            {error}
          </p>
        ) : null}
      </SheetBody>
      {loadError ? null : (
        <div className="flex items-center justify-between gap-3 border-t px-5 py-3 pb-[max(0.75rem,env(safe-area-inset-bottom))]">
          <span className="hidden text-[11px] text-muted-foreground pointer-fine:inline">Ctrl+Enter で送る</span>
          <Button className="ml-auto min-h-10" onClick={() => void send()} disabled={!canSend} data-testid="approval-send">
            {sending ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
            {selected ? `${selected.label}（「${selected.to}」へ）` : "判断を選んでください"}
          </Button>
        </div>
      )}
    </div>
  );
}
