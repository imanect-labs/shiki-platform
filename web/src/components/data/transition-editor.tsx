"use client";

/// FSM のテーブルの状態の列（2-C-P2-4）: 状態のセルは自由入力ではなく「進められる先」のメニューで開き、
/// 遷移 API（`POST …/transition`）を呼ぶ。状態の列は遷移でしか変わらない（PATCH は 400 になる）。
///
/// 候補はサーバの「いま実行できる遷移先」（`GET …/transitions` の `AllowedTransitions`＝`{status_field,
/// current, rev, transitions}`・実行できる人の条件まで評価済み）。メニューの「今」はサーバの `current`、遷移は
/// サーバの `rev` で送る（グリッドの行が古くても、見せた状態からの遷移になる）。読めなければ理由を出す
/// （候補を推測で出さない）。

import { ArrowRight, Loader2 } from "lucide-react";
import * as React from "react";

import type { ProvideEditorComponent, TextCell } from "@glideapps/glide-data-grid";
import { DataApiError, getFsm, type AllowedTransitions, type DataTable, type FsmBody } from "@/lib/data-api";
import { cn } from "@/lib/utils";

/// テーブルの状態の列と、読めたら FSM の定義（作成のダイアログが最初の状態を出すのに使う）。
export type FsmInfo = {
  /// 状態として扱う select 列の参照名（`schema.status_field`）。
  field: string;
  /// FSM の定義（読めなければ `null`）。
  body: FsmBody | null;
};

/// テーブルにピンされた FSM（状態の列があるテーブルだけ・読めなければ定義なし）。
export function useFsm(table: DataTable): FsmInfo | null {
  const field = table.schema.status_field ?? null;
  const ref = table.schema.fsm_ref ?? null;
  const [body, setBody] = React.useState<FsmBody | null>(null);
  const refKey = ref ? `${ref.artifact_id}@${ref.version}` : "";
  React.useEffect(() => {
    let active = true;
    setBody(null);
    if (!ref || !field) return;
    getFsm(ref.artifact_id, ref.version)
      .then((r) => active && setBody(r.body))
      .catch(() => {
        // FSM の閲覧権限が無い（テーブルの editor でも遷移はできる）: 作成のダイアログは最初の状態の名前を出さない。
      });
    return () => {
      active = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refKey, field]);
  if (!field || !ref) return null;
  return { field, body };
}

/// 遷移の失敗の利用者向けの文言。
export function transitionErrorMessage(e: unknown): string {
  if (e instanceof DataApiError) {
    if (e.status === 403) return "この状態へ進める権限がありません（進められる人が決まっている遷移です）";
    if (e.revConflict) return "他の人が先にこの行を更新しました。最新の内容を読み込み直しました";
    if (e.status === 400) return "今の状態からは、その状態へ進められません";
    return e.message;
  }
  return e instanceof Error ? e.message : String(e);
}

/// 進められる先を読めなかったときの文言。
function loadErrorMessage(e: unknown): string {
  if (e instanceof DataApiError) {
    if (e.fieldUnavailable) return "状態の列を見る権限がないため、進められる先を出せません";
    if (e.status === 404) return "この行は見つかりません（削除されたか、見る権限がなくなりました）";
    if (e.status === 403) return "この表を編集する権限がないため、状態を進められません";
    return e.message;
  }
  return "進められる先を読み込めませんでした。もう一度開いてください";
}

/// 状態のセルのメニュー（glide の provideEditor 用）。開くとサーバに進められる先を聞き、選ぶと
/// `onPick(to, rev)`（`rev` はサーバが返した今の rev）を呼んで閉じる（セルの値は書き換えない）。
export function transitionCellEditor(opts: {
  label: string;
  /// グリッドに見えている今の状態（サーバの答えが届くまでの表示）。
  current: string | null;
  /// いま実行できる遷移先をサーバに聞く。
  load: () => Promise<AllowedTransitions>;
  onPick: (to: string, rev: number) => void;
}): { editor: ProvideEditorComponent<TextCell>; disablePadding: true; disableStyling: true } {
  const Editor: ProvideEditorComponent<TextCell> = ({ onFinishedEditing }) => {
    const [active, setActive] = React.useState(0);
    const [busy, setBusy] = React.useState(false);
    const [state, setState] = React.useState<
      | { kind: "loading"; current: string | null }
      | { kind: "ready"; current: string | null; rev: number; targets: string[] }
      | { kind: "failed"; current: string | null; message: string }
    >({ kind: "loading", current: opts.current });
    const listRef = React.useRef<HTMLUListElement>(null);
    const rootRef = React.useRef<HTMLDivElement>(null);
    React.useEffect(() => {
      let alive = true;
      opts
        .load()
        .then((r) => alive && setState({ kind: "ready", current: r.current ?? null, rev: r.rev, targets: r.transitions }))
        .catch((e: unknown) => alive && setState((s) => ({ kind: "failed", current: s.current, message: loadErrorMessage(e) })));
      return () => {
        alive = false;
      };
    }, []);
    React.useLayoutEffect(() => {
      (listRef.current ?? rootRef.current)?.focus();
    }, [state.kind]);
    const targets = state.kind === "ready" ? state.targets : [];
    const pick = (to: string) => {
      if (busy || state.kind !== "ready") return;
      setBusy(true);
      opts.onPick(to, state.rev);
      onFinishedEditing(undefined, [0, 0]);
    };
    return (
      <div
        ref={rootRef}
        tabIndex={-1}
        className="w-64 rounded-lg border bg-popover p-1.5 text-popover-foreground shadow-md outline-none"
        data-testid="transition-editor"
        aria-busy={state.kind === "loading" || undefined}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            e.stopPropagation();
            onFinishedEditing(undefined, [0, 0]);
          }
        }}
      >
        <p className="px-2 pb-1.5 pt-1 text-[11px] text-muted-foreground">
          「{opts.label}」を進める（今: {state.current ?? "未設定"}）
        </p>
        {state.kind === "loading" ? (
          <p className="flex items-center gap-1.5 px-2 py-2 text-xs text-muted-foreground" role="status">
            <Loader2 className="size-3.5 animate-spin" aria-hidden />
            進められる先を確かめています…
          </p>
        ) : state.kind === "failed" ? (
          <p className="px-2 py-2 text-xs text-destructive" role="alert">
            {state.message}
          </p>
        ) : targets.length === 0 ? (
          <p className="px-2 py-2 text-xs text-muted-foreground">この状態から、あなたが進められる先はありません。</p>
        ) : (
          <ul
            ref={listRef}
            role="listbox"
            aria-label={`「${opts.label}」の進め先`}
            tabIndex={-1}
            aria-activedescendant={`transition-opt-${active}`}
            className="outline-none"
            onKeyDown={(e) => {
              if (e.key === "ArrowDown") {
                e.preventDefault();
                setActive((a) => Math.min(targets.length - 1, a + 1));
              } else if (e.key === "ArrowUp") {
                e.preventDefault();
                setActive((a) => Math.max(0, a - 1));
              } else if (e.key === "Enter") {
                e.preventDefault();
                e.stopPropagation();
                const to = targets[active];
                if (to) pick(to);
              } else if (e.key === "Escape") {
                e.preventDefault();
                e.stopPropagation();
                onFinishedEditing(undefined, [0, 0]);
              }
            }}
          >
            {targets.map((to, i) => (
              <li
                key={to}
                id={`transition-opt-${i}`}
                role="option"
                aria-selected={i === active}
                onMouseDown={(e) => e.preventDefault()}
                onMouseEnter={() => setActive(i)}
                onClick={() => pick(to)}
                className={cn(
                  "flex cursor-default items-center gap-2 rounded-sm px-2 py-1.5 text-[13px]",
                  i === active && "bg-accent",
                )}
              >
                {busy && i === active ? (
                  <Loader2 className="size-3.5 shrink-0 animate-spin text-muted-foreground" aria-hidden />
                ) : (
                  <ArrowRight className="size-3.5 shrink-0 text-muted-foreground" aria-hidden />
                )}
                <span className="truncate">{to}</span>
              </li>
            ))}
          </ul>
        )}
      </div>
    );
  };
  Editor.displayName = "TransitionCellEditor";
  return { editor: Editor, disablePadding: true, disableStyling: true };
}
