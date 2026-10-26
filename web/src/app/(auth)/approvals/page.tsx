"use client";

/// 承認の受信箱（ワークフローの承認待ち・GAP-08・D-27）。
///
/// 自分宛て（本人・所属ロール）の承認待ちのうち、行が自分に見えるものだけをサーバが返す。項目を開くと詳細の
/// シートで判断する（判断は自分の権限で表の状態を進める・ワークフローはその変更を受けて続く）。判断を送ったら
/// その項目を一覧から外し、読み直してサーバの状態で描き直す（押した事実の正はサーバ・PIT-57）。

import * as React from "react";
import { AlertTriangle, ClipboardCheck, Loader2, RefreshCw, Table2 } from "lucide-react";

import { ApprovalSheet } from "@/components/approvals/approval-sheet";
import {
  deadlineSoon,
  deadlineText,
  fieldValueText,
  useRefNames,
  userLabel,
} from "@/components/approvals/approval-values";
import { PageContainer } from "@/components/shell/page-container";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { toast } from "@/components/ui/use-toast";
import { listApprovals, notifyApprovalsChanged, type ApprovalItem } from "@/lib/approvals-api";
import { cn } from "@/lib/utils";

/// 一覧の行に出す列の数（残りは詳細で見る）。
const SUMMARY_FIELDS = 3;

export default function ApprovalsPage() {
  const [items, setItems] = React.useState<ApprovalItem[] | null>(null);
  const [cursor, setCursor] = React.useState<string | null>(null);
  const [error, setError] = React.useState(false);
  const [loadingMore, setLoadingMore] = React.useState(false);
  const [open, setOpen] = React.useState<ApprovalItem | null>(null);

  const load = React.useCallback(async () => {
    try {
      const page = await listApprovals();
      setItems(page.items);
      setCursor(page.next_cursor ?? null);
      setError(false);
    } catch {
      setError(true);
      setItems((prev) => prev ?? []);
    }
  }, []);

  React.useEffect(() => {
    void load();
    const onFocus = () => void load();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [load]);

  const more = async () => {
    if (!cursor) return;
    setLoadingMore(true);
    try {
      const page = await listApprovals(cursor);
      setItems((prev) => {
        const seen = new Set((prev ?? []).map((i) => i.approval_id));
        return [...(prev ?? []), ...page.items.filter((i) => !seen.has(i.approval_id))];
      });
      setCursor(page.next_cursor ?? null);
    } catch {
      toast({ variant: "destructive", title: "続きを読み込めませんでした" });
    } finally {
      setLoadingMore(false);
    }
  };

  const remove = React.useCallback((gone: ApprovalItem) => {
    setItems((prev) => (prev ?? []).filter((i) => i.approval_id !== gone.approval_id));
    notifyApprovalsChanged();
  }, []);

  const onDecided = React.useCallback(
    (done: ApprovalItem) => {
      setOpen(null);
      remove(done);
      toast({ title: "判断を送りました", description: "ワークフローが続きを進めます。" });
      void load();
    },
    [load, remove],
  );

  const onGone = React.useCallback((gone: ApprovalItem) => remove(gone), [remove]);

  // 申請した人・ユーザー／部署の列の表示名を引く。
  const userIds = React.useMemo(() => {
    const ids = new Set<string>();
    for (const i of items ?? []) {
      ids.add(i.record.owner);
      for (const f of i.record.fields) if (f.field_type === "user_ref" && typeof f.value === "string") ids.add(f.value);
    }
    return [...ids].sort();
  }, [items]);
  const roleIds = React.useMemo(() => {
    const ids = new Set<string>();
    for (const i of items ?? [])
      for (const f of i.record.fields) if (f.field_type === "role_ref" && typeof f.value === "string") ids.add(f.value);
    return [...ids].sort();
  }, [items]);
  useRefNames(userIds, roleIds);

  return (
    <PageContainer
      title="承認"
      description="ワークフローがあなたの判断を待っている申請です。判断すると表の状態が進み、ワークフローが続きます。"
      className="max-w-3xl"
      actions={
        <Button variant="outline" size="sm" onClick={() => void load()} aria-label="読み直す">
          <RefreshCw className="size-4" aria-hidden />
          <span className="hidden sm:inline">読み直す</span>
        </Button>
      }
    >
      {error && items && items.length === 0 ? (
        <div role="alert" className="flex flex-col items-center gap-3 rounded-xl border border-destructive/30 px-6 py-12 text-center">
          <AlertTriangle className="size-6 text-destructive" aria-hidden />
          <p className="text-sm">承認待ちを読み込めませんでした。</p>
          <Button variant="outline" size="sm" onClick={() => void load()}>
            もう一度読み込む
          </Button>
        </div>
      ) : items === null ? (
        <div className="flex items-center justify-center gap-2 py-16 text-sm text-muted-foreground">
          <Loader2 className="size-4 animate-spin" aria-hidden />
          読み込み中…
        </div>
      ) : items.length === 0 ? (
        <EmptyState
          seasonal
          icon={ClipboardCheck}
          title="承認待ちはありません"
          description="ワークフローがあなたの判断を待つと、ここに出ます。"
        />
      ) : (
        <>
          <ul className="flex flex-col gap-2" aria-label="承認待ちの一覧" data-testid="approval-list">
            {items.map((item) => (
              <li key={item.approval_id}>
                <ApprovalRow item={item} onOpen={() => setOpen(item)} />
              </li>
            ))}
          </ul>
          {cursor ? (
            <div className="mt-4 flex justify-center">
              <Button variant="outline" size="sm" onClick={() => void more()} disabled={loadingMore}>
                {loadingMore ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
                さらに読み込む
              </Button>
            </div>
          ) : null}
        </>
      )}
      <ApprovalSheet item={open} onClose={() => setOpen(null)} onDecided={onDecided} onGone={onGone} />
    </PageContainer>
  );
}

function ApprovalRow({ item, onOpen }: { item: ApprovalItem; onOpen: () => void }) {
  const soon = deadlineSoon(item.deadline);
  return (
    <button
      type="button"
      onClick={onOpen}
      className={cn(
        "flex w-full flex-col gap-1.5 rounded-xl border bg-card px-4 py-3 text-left transition-colors",
        "hover:bg-accent/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
      )}
      data-testid="approval-item"
    >
      <span className="flex w-full min-w-0 items-start justify-between gap-3">
        <span className="min-w-0 truncate text-[15px] font-medium">{item.title}</span>
        <Badge variant={soon ? "warning" : "muted"} className="shrink-0">
          {deadlineText(item.deadline)}
        </Badge>
      </span>
      <span className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
        <span className="inline-flex min-w-0 items-center gap-1">
          <Table2 className="size-3.5 shrink-0" aria-hidden />
          <span className="truncate">{item.table.name}</span>
        </span>
        <span className="truncate">申請: {userLabel(item.record.owner)}</span>
        <span className="truncate">状態: {item.from_state}</span>
      </span>
      {item.record.fields.length ? (
        <span className="line-clamp-2 min-w-0 break-words text-xs text-foreground/80">
          {item.record.fields
            .slice(0, SUMMARY_FIELDS)
            .map((f) => `${f.display_name}: ${fieldValueText(f.field_type, f.value)}`)
            .join(" ／ ")}
        </span>
      ) : null}
    </button>
  );
}
