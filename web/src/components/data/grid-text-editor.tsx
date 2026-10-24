"use client";

/// データグリッドのセルの編集欄（glide の既定の文字入力の置き換え）。
///
/// glide の既定の編集欄は、確定（Enter / Tab）の値を**キー押下の時点で描画済みの状態**から取る。打鍵の直後に
/// Enter を押すと直前の打鍵が状態に載る前の値（＝変更なし）で確定し、何も保存せずに 1 つ下へ移動していた
/// （連続編集で PATCH が出ない・続けて打った文字が下の行に入る・D-10）。ここでは確定の値を**入力欄そのもの**
/// から読む。編集欄を開いた直後の打鍵が欄の外に落ちて先頭の文字が欠けないよう、フォーカスは描画の確定時
/// （layout effect）に同期で移す。
///
/// 見た目は glide の既定の編集欄（GrowingEntry）と同じクラスを使う（版はロックファイルで固定）。Shift+Enter は
/// 改行、Esc は glide が取消として扱う（ここでは止めない）。IME の変換中の Enter は確定に使わない。

import type { ProvideEditorComponent, TextCell } from "@glideapps/glide-data-grid";
import * as React from "react";

export const GridTextEditor: ProvideEditorComponent<TextCell> = ({
  value,
  onChange,
  onFinishedEditing,
  isHighlighted,
  initialValue,
}) => {
  const ref = React.useRef<HTMLTextAreaElement>(null);
  const initial = React.useRef(value.data ?? "");
  const [text, setText] = React.useState(initial.current);

  React.useLayoutEffect(() => {
    const ta = ref.current;
    if (!ta) return;
    ta.focus();
    const n = ta.value.length;
    ta.setSelectionRange(isHighlighted ? 0 : n, n);
    // 開いたときだけ（打鍵ごとに選択を戻さない）。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const finish = (e: React.KeyboardEvent<HTMLTextAreaElement>, movement: readonly [-1 | 0 | 1, -1 | 0 | 1]) => {
    e.preventDefault();
    e.stopPropagation();
    const data = e.currentTarget.value;
    // 開いたまま変えずに確定したら保存せず移動だけする（glide の既定と同じ）。打鍵で開いた欄（initialValue あり）は
    // 最初の 1 文字が既に入力なので、元の値と同じに見えても変更として渡す（同値なら onCellEdited が捨てる）。
    const changed = initialValue !== undefined || data !== initial.current;
    onFinishedEditing(changed ? { ...value, data } : undefined, movement);
  };

  return (
    <div className="gdg-growing-entry gdg-g1y0xocz">
      <div className="gdg-s69h75o" aria-hidden>
        {text + "\n"}
      </div>
      <textarea
        ref={ref}
        className="gdg-input gdg-izpuzkl"
        // 全体の focus-visible の輪は編集欄の枠で切られて上下の線に見えるので消す（枠は glide の編集欄が描く）。
        style={{ boxShadow: "none" }}
        dir="auto"
        defaultValue={initial.current}
        onChange={(e) => {
          setText(e.target.value);
          onChange({ ...value, data: e.target.value });
        }}
        onKeyDown={(e) => {
          if (e.nativeEvent.isComposing || e.keyCode === 229) return;
          if (e.key === "Enter" && !e.shiftKey) finish(e, [0, 1]);
          else if (e.key === "Tab") finish(e, [e.shiftKey ? -1 : 1, 0]);
        }}
      />
    </div>
  );
};
