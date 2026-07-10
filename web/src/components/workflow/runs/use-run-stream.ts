"use client";

/// run 詳細のライブ更新 hook（Task 10.14）。
///
/// SSE（run_event ストリーム）をトリガに run 詳細を再取得する。イベント本文から
/// UI 状態を組み立てず、常に `GET /runs/{id}`（DB=truth）を正とすることで
/// リプレイ・重複・欠落に対して頑健にする。SSE が張れない環境では 2 秒ポーリングに
/// フォールバックし、terminal（succeeded/failed/cancelled）で止める。

import * as React from "react";

import {
  getRun,
  subscribeRunEvents,
  type RunDetail,
} from "@/lib/workflow-run-api";
import { isTerminalRunStatus } from "./status";

const REFETCH_THROTTLE_MS = 400;
const POLL_INTERVAL_MS = 2000;

export function useRunStream(
  workflowId: string,
  runId: string | null,
): { detail: RunDetail | null; error: string | null; refresh: () => void } {
  const [detail, setDetail] = React.useState<RunDetail | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  // 再取得をトリガするためのシグナル（cancel/retry 直後に呼ぶ）。
  const [tick, setTick] = React.useState(0);
  const refresh = React.useCallback(() => setTick((t) => t + 1), []);

  React.useEffect(() => {
    if (!runId) {
      setDetail(null);
      setError(null);
      return;
    }
    let closed = false;
    let throttling = false;
    let pollTimer: ReturnType<typeof setInterval> | null = null;
    let lastStatus = "";

    const stopPolling = () => {
      if (pollTimer !== null) {
        clearInterval(pollTimer);
        pollTimer = null;
      }
    };

    const load = () =>
      getRun(workflowId, runId)
        .then((d) => {
          if (closed) return;
          lastStatus = d.status;
          setDetail(d);
          setError(null);
          if (isTerminalRunStatus(d.status)) stopPolling();
        })
        .catch((e) => {
          if (!closed) setError(e instanceof Error ? e.message : String(e));
        });

    /// イベント連打（step 単位で複数飛ぶ）を 1 回の再取得にまとめる。
    const scheduleLoad = () => {
      if (throttling) return;
      throttling = true;
      setTimeout(() => {
        throttling = false;
        if (!closed) void load();
      }, REFETCH_THROTTLE_MS);
    };

    void load();
    const unsubscribe = subscribeRunEvents(workflowId, runId, {
      onEvent: scheduleLoad,
      onTerminal: () => void load(),
      onError: () => {
        // SSE 不可（proxy 等）→ terminal までポーリングで追従する。
        if (closed || pollTimer !== null || isTerminalRunStatus(lastStatus)) return;
        pollTimer = setInterval(() => void load(), POLL_INTERVAL_MS);
      },
    });

    return () => {
      closed = true;
      stopPolling();
      unsubscribe();
    };
  }, [workflowId, runId, tick]);

  return { detail, error, refresh };
}
