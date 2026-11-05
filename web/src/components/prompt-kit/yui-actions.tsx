// Markdown の ```ui（Yui）のボタン（app・ask・plan）を、会話の回答・ミニアプリの画面につなぐ（#604）。
//
// ボタンの id はブロックのソースの SHA-256 の先頭 16 桁と図の中の id（a1 など）を合わせた
// `{hash}:{a1}`。サーバー（crates/yui の block_hash）と同じ規則で求める。押せるか・何を送るかの
// 判断はサーバーが保存済みの本文（画面）から読み直して行うので、ここは値を送って結果を出すだけ。
// どこへ送るかは置き場所（[`YuiActionHost`]）が決める。
"use client";

import * as React from "react";
import type { AppActionEvent, AppActionResult } from "@imanect/yui/react";
import { figureActions, type Figure } from "@imanect/yui";

import {
  UiActionAlreadyInvoked,
  UiActionServerError,
  UiActionUncertain,
  UiActionUnavailable,
} from "@/lib/artifact-api";
import type { UiActionRun } from "@/lib/chat-api";

/// 起動したワークフローの記録 1 件（会話の記録・ミニアプリの記録の共通の形）。
/// 結果（`reply`）・失敗の理由（`failure`）は押した本人の記録にだけ付く（GAP-09）。
export type YuiRun = Pick<UiActionRun, "action_id" | "label" | "workflow_name" | "status" | "invoked_at"> &
  Partial<Pick<UiActionRun, "waiting_for" | "failure" | "reply" | "has_reply" | "result_expired">> & {
    run_id?: string | null;
    values?: { label: string; value: string }[];
  };

/// 図のボタンを実行できる場所（確定した回答のメッセージ・ミニアプリの画面）。無ければボタンは無効（表示だけ）。
export type YuiActionHost = {
  /// ボタンを実行する（成功で解決し、誤りは `@/lib/artifact-api` の `UiAction*` を投げる）。
  /// 冪等キーはワークフローのボタンにだけ付く。
  invoke: (key: string, values: Record<string, unknown>, idempotencyKey: string | undefined) => Promise<unknown>;
  /// ワークフローのボタンが押せるか（実行先がサーバーで解決できているか）。
  isBound: (key: string) => boolean;
  /// 送り済みの 1 回だけのボタン（会話に送るボタン）。
  invokedActions: readonly string[];
  /// 起動したワークフローの記録。
  uiRuns: readonly YuiRun[];
  /// 生成直後の仮のメッセージ（id がサーバーのものではないので、読み直すまで押せない）。
  /// `reloading` はすぐ読み直される、`stopped`（生成の停止）は読み直さない。
  unsaved?: "reloading" | "stopped";
  /// 実行の後に読み直す（新しい発話・起動の記録を出す）。
  onChanged: () => void;
  /// 結果が付いていない古い記録を開いたときに、その 1 件を結果つきで読む（GAP-09）。
  loadRun?: (runId: string) => Promise<YuiRun>;
  /// `app` の図を「アプリとして保存」する（会話の回答の図だけ）。
  saveAsApp?: (block: string, title: string) => void;
  /// 押す前に分かる、ワークフローのボタンを使えない理由（使う人に足りない権限・GAP-37）。使えるなら null。
  /// 判定はサーバー（`GET /mini-apps/{id}/access`）が決め、押されたときもサーバーが本人の権限で改めて確かめる。
  blockedReason?: (workflowName: string) => string | null;
  /// 止めたボタンが押されたときに確かめ直し、使えない理由を返す（使えるなら null・確かめられなかったときも null にして
  /// サーバーに任せる）。渡すと、止めたボタンも押せる形で描く（共有してもらった後に、開き直さずに押せる・BUG-217）。
  recheckBlocked?: (workflowName: string) => Promise<string | null>;
};

export const YuiActionContext = React.createContext<YuiActionHost | null>(null);

/// 引用・箇条書きの中か。操作を持つ図はトップレベルだけ押せる（サーバーも入れ子の図は読まない）。
export const YuiNestedContext = React.createContext(false);

const ACTION_KINDS = new Set(["app", "ask", "plan"]);

/// ブロックの識別子（CRLF を LF にし、末尾の空白を落としたソースの SHA-256 の先頭 16 桁）。
export async function blockHash(source: string): Promise<string> {
  const normalized = source.replace(/\r\n/g, "\n").trimEnd();
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(normalized));
  return Array.from(new Uint8Array(digest).slice(0, 8), (b) => b.toString(16).padStart(2, "0")).join("");
}

/// 冪等キー（送信ごとに作る。同じ送信の再送でワークフローを二度起動しない）。
function newIdempotencyKey(): string {
  return typeof crypto.randomUUID === "function"
    ? crypto.randomUUID()
    : Array.from(crypto.getRandomValues(new Uint8Array(16)), (b) => b.toString(16).padStart(2, "0")).join("");
}

export type YuiActionProps = {
  onAction?: (e: AppActionEvent) => Promise<AppActionResult>;
  actionDisabledReason?: string;
  invokedActions?: readonly string[];
  /// このブロックのボタンで起動したワークフロー（新しいものが後ろ）。
  runs: readonly YuiRun[];
  /// 「アプリとして保存」（会話の回答の `app` の図だけ）。
  saveAsApp?: () => void;
  /// 図の一部のボタンだけを使えないときの、ボタンの文言と理由（図の下に出す・GAP-37）。図のボタンがすべて使えない
  /// ときは `actionDisabledReason` で図ごと止める。
  blockedNotes?: { label: string; reason: string }[];
};

/// 図に渡すボタンの props。操作を持たない種類は `null`。
export function useYuiActions(
  figure: Figure | null,
  source: string,
  complete: boolean,
  closed: boolean,
): YuiActionProps | null {
  const host = React.useContext(YuiActionContext);
  const nested = React.useContext(YuiNestedContext);
  const actionable = figure != null && ACTION_KINDS.has(figure.kind);
  const usable = actionable && host != null && !nested && complete && closed;
  // 結果の分からない送信（通信が途中で切れた）の冪等キー。同じボタン・同じ値で押し直したときは
  // 同じキーで送り、サーバーが受け付けていた場合に二つ目の run を起動しない。
  const pendingKeys = React.useRef(new Map<string, string>());
  const [hash, setHash] = React.useState<string | null>(null);
  React.useEffect(() => {
    if (!usable) return;
    let live = true;
    blockHash(source).then(
      (h) => live && setHash(h),
      () => live && setHash(null),
    );
    return () => {
      live = false;
    };
  }, [usable, source]);

  const onAction = React.useCallback(
    async (e: AppActionEvent): Promise<AppActionResult> => {
      if (!host || host.unsaved || !hash) return { ok: false, message: "いまは実行できません" };
      const key = `${hash}:${e.actionId}`;
      const target = e.action.target;
      // 使う人に足りない権限があるボタンは送らない（理由はボタンの近くにも出している・GAP-37）。押されたら確かめ直し、
      // 共有してもらっていれば、そのまま送る（BUG-217）。
      let blocked = target.kind === "workflow" ? (host.blockedReason?.(target.name) ?? null) : null;
      if (blocked && target.kind === "workflow" && host.recheckBlocked) {
        blocked = await host.recheckBlocked(target.name);
      }
      if (blocked) return { ok: false, message: blocked };
      if (target.kind === "workflow" && !host.isBound(key)) {
        return { ok: false, message: `ワークフロー「${target.name}」が見つからないか、使う権限がありません` };
      }
      const sent = `${key}\u0000${JSON.stringify(e.values)}`;
      const idempotencyKey =
        target.kind === "workflow" ? (pendingKeys.current.get(sent) ?? newIdempotencyKey()) : undefined;
      try {
        await host.invoke(key, e.values as Record<string, unknown>, idempotencyKey);
        pendingKeys.current.delete(sent);
        host.onChanged();
        return {
          ok: true,
          message: target.kind === "workflow" ? `ワークフロー「${target.name}」を起動しました` : "送信しました",
        };
      } catch (err) {
        // 受け付けたか分からない・サーバーの誤り・いま使えない（5xx と通信の切れ）。押し直しは同じキーで送る
        // （受け付けていた場合に二つ目の run を起動しない）。文言は起きたことごとに分ける（GAP-41）。
        if (err instanceof UiActionUncertain || err instanceof UiActionServerError || err instanceof UiActionUnavailable) {
          if (idempotencyKey) pendingKeys.current.set(sent, idempotencyKey);
          return { ok: false, message: err.message };
        }
        pendingKeys.current.delete(sent);
        if (err instanceof UiActionAlreadyInvoked) {
          // 送り済み（別のタブ・二度押し）。会話を読み直して送信済みの表示に揃える。
          host.onChanged();
          return { ok: false, message: target.kind === "workflow" ? "起動の途中です" : "送信済みです" };
        }
        return { ok: false, message: err instanceof Error ? err.message : "実行できませんでした" };
      }
    },
    [host, hash],
  );

  if (!actionable) return null;
  if (!host) return { actionDisabledReason: "この場所ではボタンを使えません", runs: [] };
  if (nested) return { actionDisabledReason: "引用や箇条書きの中の図のボタンは使えません", runs: [] };
  if (!complete) return { actionDisabledReason: "回答の生成が終わると使えます", runs: [] };
  if (!closed) return { actionDisabledReason: "図のブロックが閉じていないため、ボタンは使えません", runs: [] };
  if (host.unsaved === "stopped") {
    return { actionDisabledReason: "生成を止めた回答のボタンは、ページを読み直すと使えます", runs: [] };
  }
  if (host.unsaved || !hash) return { actionDisabledReason: "準備しています…", runs: [] };
  const prefix = `${hash}:`;
  const save = host.saveAsApp;
  // 使う人に足りない権限があるボタン（GAP-37）。理由は図の下に言う。押されたら確かめ直せる置き場所（BUG-217）では
  // ボタンを押せるままにし、確かめ直せない置き場所では、図のボタンがすべて使えなければ図ごと止める。
  const actions = figure ? figureActions(figure) : [];
  const blocked = host.blockedReason
    ? actions.flatMap((a) => {
        const reason = a.target.kind === "workflow" ? host.blockedReason?.(a.target.name) : null;
        return reason ? [{ label: a.label, reason }] : [];
      })
    : [];
  if (blocked.length > 0 && blocked.length === actions.length && !host.recheckBlocked) {
    const reasons = [...new Set(blocked.map((b) => b.reason))];
    return {
      actionDisabledReason: reasons.join(" "),
      runs: host.uiRuns.filter((r) => r.action_id.startsWith(prefix)),
    };
  }
  return {
    onAction,
    ...(blocked.length > 0 ? { blockedNotes: blocked } : {}),
    invokedActions: host.invokedActions.filter((a) => a.startsWith(prefix)).map((a) => a.slice(prefix.length)),
    runs: host.uiRuns.filter((r) => r.action_id.startsWith(prefix)),
    // 会話に送るボタン（-> chat）を持つ図は、会話の無いアプリにできない（保存時に弾かれる）。
    ...(save && figure?.kind === "app" && figureActions(figure).every((a) => a.target.kind === "workflow")
      ? { saveAsApp: () => save(hash, figure.title) }
      : {}),
  };
}

/// 本文に ```ui（Yui）のブロックがあるか。生成の後にメッセージを読み直す（ボタンの束縛と id を得る）かの判断に使う。
export function hasYuiBlock(text: string): boolean {
  return /^[ \t]*(?:>[ \t]*)*(?:`{3,}|~{3,})[ \t]*y?ui\b/im.test(text);
}

/// 動いている起動があるか（起動中・順番待ち・実行中。待機中は含めない）。
export function hasOpenRuns(runs: readonly YuiRun[] | undefined): boolean {
  return (runs ?? []).some((r) => r.status === "starting" || r.status === "queued" || r.status === "running");
}

/// 待機中（時間・できごと・承認を待っている）の起動があるか。
export function hasWaitingRuns(runs: readonly YuiRun[] | undefined): boolean {
  return (runs ?? []).some((r) => r.status === "waiting");
}

/// 待機中の起動しか無いときに読み直す間隔（何日も待つ承認を 3 秒ごとに読み直さない・BUG-20）。
export const WAITING_REFRESH_MS = 30_000;

/// 起動の記録を読み直す間隔。動いている起動があれば `activeMs`、待機中だけなら 30 秒、終わっていれば読み直さない。
export function runsRefreshDelay(runs: readonly YuiRun[] | undefined, activeMs = 3000): number | null {
  if (hasOpenRuns(runs)) return activeMs;
  if (hasWaitingRuns(runs)) return WAITING_REFRESH_MS;
  return null;
}

/// 起動の記録が終わるまで読み直す（間隔は [`runsRefreshDelay`]）。待機中だけのときは、画面に戻った時
/// （フォーカス・タブの表示）にも読み直す。`tick` は読み直しを終えるたびに変わる値（失敗しても次を予約する）。
export function useRunsRefresh(
  runs: readonly YuiRun[] | undefined,
  refresh: () => void,
  tick: unknown,
  activeMs = 3000,
): void {
  const delay = runsRefreshDelay(runs, activeMs);
  const latest = React.useRef(refresh);
  latest.current = refresh;
  React.useEffect(() => {
    if (delay == null) return;
    const t = window.setTimeout(() => latest.current(), delay);
    return () => window.clearTimeout(t);
  }, [delay, tick]);
  const waitingOnly = delay === WAITING_REFRESH_MS;
  React.useEffect(() => {
    if (!waitingOnly) return;
    const onVisible = () => {
      if (document.visibilityState === "visible") latest.current();
    };
    const onFocus = () => latest.current();
    document.addEventListener("visibilitychange", onVisible);
    window.addEventListener("focus", onFocus);
    return () => {
      document.removeEventListener("visibilitychange", onVisible);
      window.removeEventListener("focus", onFocus);
    };
  }, [waitingOnly]);
}
