"use client";

/// FSM のテーブルの状態の列（2-C-P2-4）: 状態のセルは自由入力ではなく「進められる先」のメニューで開き、
/// 遷移 API（`POST …/transition`）を呼ぶ。状態の列は遷移でしか変わらない（PATCH は 400 になる）。
///
/// 候補は FSM の定義の「今の状態から出る遷移」の行き先。定義を読めない人（FSM の閲覧権限が無い editor）には
/// 今の状態以外の選択肢を出し、遷移の可否はサーバが決める（定義外は 400・実行できる人の条件は 403）。

import { ArrowRight, Loader2 } from "lucide-react";
import * as React from "react";

import type { ProvideEditorComponent, TextCell } from "@glideapps/glide-data-grid";
import { DataApiError, getFsm, type DataTable, type FsmBody } from "@/lib/data-api";
import { cn } from "@/lib/utils";

/// テーブルの状態の列と、読めたら FSM の定義。
export type FsmInfo = {
  /// 状態として扱う select 列の参照名（`schema.status_field`）。
  field: string;
  /// FSM の定義（読めなければ `null`）。
  body: FsmBody | null;
};

/// 今の状態から進められる先（定義が読めなければ選択肢のうち今の状態以外）。`known` は定義に基づくか。
export function transitionTargets(
  fsm: FsmInfo,
  current: string | null,
  options: readonly string[],
): { targets: string[]; known: boolean } {
  if (!fsm.body) return { targets: options.filter((o) => o !== current), known: false };
  const out: string[] = [];
  for (const t of fsm.body.transitions) {
    if (t.from === current && !out.includes(t.to)) out.push(t.to);
  }
  return { targets: out, known: true };
}

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
        // FSM の閲覧権限が無い（テーブルの editor でも遷移はできる）: 候補を絞らずに出す。
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

/// 状態のセルのメニュー（glide の provideEditor 用）。選ぶと `onPick` を呼んで閉じる（セルの値は書き換えない）。
export function transitionCellEditor(opts: {
  label: string;
  current: string | null;
  targets: string[];
  known: boolean;
  onPick: (to: string) => void;
}): { editor: ProvideEditorComponent<TextCell>; disablePadding: true; disableStyling: true } {
  const Editor: ProvideEditorComponent<TextCell> = ({ onFinishedEditing }) => {
    const [active, setActive] = React.useState(0);
    const [busy, setBusy] = React.useState(false);
    const listRef = React.useRef<HTMLUListElement>(null);
    React.useLayoutEffect(() => {
      listRef.current?.focus();
    }, []);
    const pick = (to: string) => {
      if (busy) return;
      setBusy(true);
      opts.onPick(to);
      onFinishedEditing(undefined, [0, 0]);
    };
    return (
      <div
        className="w-64 rounded-lg border bg-popover p-1.5 text-popover-foreground shadow-md"
        data-testid="transition-editor"
      >
        <p className="px-2 pb-1.5 pt-1 text-[11px] text-muted-foreground">
          「{opts.label}」を進める（今: {opts.current ?? "未設定"}）
        </p>
        {opts.targets.length === 0 ? (
          <p className="px-2 py-2 text-xs text-muted-foreground">この状態から進められる先はありません。</p>
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
                setActive((a) => Math.min(opts.targets.length - 1, a + 1));
              } else if (e.key === "ArrowUp") {
                e.preventDefault();
                setActive((a) => Math.max(0, a - 1));
              } else if (e.key === "Enter") {
                e.preventDefault();
                e.stopPropagation();
                const to = opts.targets[active];
                if (to) pick(to);
              } else if (e.key === "Escape") {
                e.preventDefault();
                e.stopPropagation();
                onFinishedEditing(undefined, [0, 0]);
              }
            }}
          >
            {opts.targets.map((to, i) => (
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
        {!opts.known && opts.targets.length > 0 ? (
          <p className="px-2 pb-1 pt-1.5 text-[11px] text-muted-foreground">
            進められない先を選ぶと理由を表示します。
          </p>
        ) : null}
      </div>
    );
  };
  Editor.displayName = "TransitionCellEditor";
  return { editor: Editor, disablePadding: true, disableStyling: true };
}
