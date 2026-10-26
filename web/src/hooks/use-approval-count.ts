"use client";

/// サイドバーの「承認」の件数バッジ（GAP-08・D-27）。
///
/// フォーカスが戻ったとき・60 秒ごと・受信箱で判断したとき（{@link APPROVALS_CHANGED_EVENT}）に読み直す。
/// 隠れているタブでは読まない。読めなかったら件数を出さない（バッジを消す・誤った数を出さない）。

import * as React from "react";

import { APPROVALS_CHANGED_EVENT, countApprovals } from "@/lib/approvals-api";

const POLL_MS = 60_000;

export type ApprovalCount = { count: number; more: boolean };

export function useApprovalCount(): ApprovalCount | null {
  const [value, setValue] = React.useState<ApprovalCount | null>(null);
  React.useEffect(() => {
    let alive = true;
    let inflight = false;
    const load = () => {
      if (inflight || (typeof document !== "undefined" && document.visibilityState === "hidden")) return;
      inflight = true;
      countApprovals()
        .then((c) => alive && setValue({ count: c.count, more: c.more }))
        .catch(() => alive && setValue(null))
        .finally(() => {
          inflight = false;
        });
    };
    load();
    const timer = window.setInterval(load, POLL_MS);
    const onVisible = () => {
      if (document.visibilityState === "visible") load();
    };
    window.addEventListener("focus", load);
    window.addEventListener(APPROVALS_CHANGED_EVENT, load);
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      alive = false;
      window.clearInterval(timer);
      window.removeEventListener("focus", load);
      window.removeEventListener(APPROVALS_CHANGED_EVENT, load);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, []);
  return value;
}

/// バッジの表記（0 件なら出さない・99 を超えたら 99+）。
export function approvalBadge(c: ApprovalCount | null): string | null {
  if (!c || c.count === 0) return null;
  return c.more ? "99+" : String(c.count);
}
