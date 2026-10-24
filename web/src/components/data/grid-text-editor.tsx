"use client";

/// データグリッドのセルの編集欄（glide の既定の文字入力の置き換え）。
///
/// glide の既定の編集欄は、確定（Enter / Tab）の値を**キー押下の時点で描画済みの状態**から取る。打鍵の直後に
/// Enter を押すと直前の打鍵が状態に載る前の値（＝変更なし）で確定し、何も保存せずに 1 つ下へ移動していた
/// （連続編集で PATCH が出ない・続けて打った文字が下の行に入る・D-10）。ここでは確定の値を**入力欄そのもの**
/// から読む。編集欄を開いた直後の打鍵が欄の外に落ちて先頭の文字が欠けないよう、フォーカスは描画の確定時
/// （layout effect）に同期で移す。
///
/// **開く前の打鍵（3-A-1）**: glide は編集欄の部品をページで最初に開くときだけ遅延読み込みする。その間の打鍵は
/// グリッドに届き、glide は開きかけの編集欄を打った文字で開き直し（先頭の文字が消える）、Enter は「変更なしで
/// 確定」として閉じていた。グリッド側（`useGridTypeAhead`）がその間の打鍵を預かり、ここで開いたときに受け取る。
///
/// **開いている間のフォーカス（3-D-4）**: glide はセルのクリックの後、グリッドへのフォーカスを次の描画フレームまで
/// 遅らせる。保存・SSE の反映の再描画でフレームが遅れると、その間に開いた編集欄からグリッドがフォーカスを
/// 奪い、続く打鍵がグリッドに落ちて（編集欄を開き直し・Enter で何も保存せずに閉じる）入力が失われていた。
/// 利用者が閉じる操作（確定・Esc・欄の外のクリック）をしていないのにグリッドへフォーカスが移ったら、編集欄へ
/// 戻す。
///
/// 見た目は glide の既定の編集欄（GrowingEntry）と同じクラスを使う（版はロックファイルで固定）。Shift+Enter は
/// 改行、Esc は glide が取消として扱う（ここでは止めない）。IME の変換中の Enter は確定に使わない。

import type { GridKeyEventArgs, Item, ProvideEditorComponent, TextCell } from "@glideapps/glide-data-grid";
import * as React from "react";

type Movement = readonly [-1 | 0 | 1, -1 | 0 | 1];

/// 編集欄が開く前に預かった打鍵。
export type TypeAhead = {
  /// 預かり始めた時刻（開かないまま古くなった預かりは捨てる）。
  at: number;
  /// 開いた欄に足す文字。
  text: string;
  /// 開く前に確定（Enter / Tab）が押された: 開いたらその移動で確定する。
  finish: Movement | null;
};

/// 預かりの寿命。遅延読み込みは通常数十 ms で終わる。これを過ぎても開かなければ（開かないセルだった等）、
/// 預かりをやめて打鍵をグリッドへ素通しする。
const TYPE_AHEAD_MS = 1500;

/// glide の編集欄の部品が読み込み済みか（遅延読み込みはページで 1 回だけ。済んだ後は同じイベントの中で開く）。
let overlayLoaded = false;

/// 編集欄が開く前の打鍵を預かる（3-A-1）。返す `onKeyDown` を DataEditor に、`take` / `drop` を編集欄に渡す。
///
/// 預かるのは、編集欄の部品がまだ読み込まれていないときに、編集を始める打鍵（文字・Enter・F2・Space）を
/// 編集できるセルで受けてから、編集欄が開くまでの間だけ。
export function useGridTypeAhead(isEditable: (cell: Item) => boolean, selected: () => Item | undefined) {
  const pending = React.useRef<TypeAhead | null>(null);
  const editableRef = React.useRef(isEditable);
  editableRef.current = isEditable;
  const selectedRef = React.useRef(selected);
  selectedRef.current = selected;

  const onKeyDown = React.useCallback((e: GridKeyEventArgs) => {
    const ahead = pending.current;
    if (ahead && performance.now() - ahead.at > TYPE_AHEAD_MS) pending.current = null;
    else if (ahead) {
      // Esc は glide に任せて開きかけの編集欄を閉じさせる。
      if (e.key === "Escape") {
        pending.current = null;
        return;
      }
      if (ahead.finish === null) {
        if (e.key === "Enter" && !e.shiftKey) ahead.finish = [0, 1];
        else if (e.key === "Tab") ahead.finish = [e.shiftKey ? -1 : 1, 0];
        else if (e.key === "Backspace") ahead.text = ahead.text.slice(0, -1);
        else if (e.key.length === 1 && !e.ctrlKey && !e.metaKey) ahead.text += e.key;
      }
      // 確定の後の打鍵（次のセルへの入力）は、開いた欄の確定と順序を保てないので捨てる（人の打鍵の速さでは
      // 読み込みの数十 ms に収まらない）。
      e.cancel();
      return;
    }
    if (overlayLoaded) return;
    const printable = e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey;
    if (!printable && e.key !== "Enter" && e.key !== "F2") return;
    const cell = selectedRef.current();
    if (!cell || !editableRef.current(cell)) return;
    pending.current = { at: performance.now(), text: "", finish: null };
  }, []);

  /// 編集欄の部品が読み込まれた（glide が provideEditor を呼んだ）。文字の編集欄でなければ預かりを捨てる。
  const loaded = React.useCallback((textEditor: boolean) => {
    overlayLoaded = true;
    if (!textEditor) pending.current = null;
  }, []);
  const peek = React.useCallback((): TypeAhead | null => {
    const ahead = pending.current;
    return ahead && performance.now() - ahead.at <= TYPE_AHEAD_MS ? { ...ahead } : null;
  }, []);
  const drop = React.useCallback(() => {
    pending.current = null;
  }, []);
  return React.useMemo(() => ({ onKeyDown, loaded, peek, drop }), [onKeyDown, loaded, peek, drop]);
}

/// `typeAhead` を受け取る編集欄を作る（グリッドごとに 1 回・部品の同一性を保つ）。
export function makeGridTextEditor(typeAhead: { peek: () => TypeAhead | null; drop: () => void }) {
  const Editor: ProvideEditorComponent<TextCell> = (p) => <GridTextEditor {...p} typeAhead={typeAhead} />;
  return Editor;
}

type EditorProps = React.ComponentProps<ProvideEditorComponent<TextCell>> & {
  typeAhead?: { peek: () => TypeAhead | null; drop: () => void };
};

export function GridTextEditor({ value, onChange, onFinishedEditing, isHighlighted, initialValue, typeAhead }: EditorProps) {
  const ref = React.useRef<HTMLTextAreaElement>(null);
  const initial = React.useRef(value.data ?? "");
  // 開く前に預かった打鍵（取り出しは副作用のない peek・消すのは開いた後）。
  const [ahead] = React.useState(() => typeAhead?.peek() ?? null);
  const start = ahead?.text
    ? // Enter・F2 で開いた欄（元の値を選んだ状態）は打った文字で置き換え、打鍵で開いた欄は足す。
      isHighlighted
      ? ahead.text
      : initial.current + ahead.text
    : initial.current;
  const [text, setText] = React.useState(start);
  /// 利用者が閉じる操作をした（確定・Esc・欄の外を押した）。その後のフォーカスの移動は奪われたのではない。
  const leaving = React.useRef(false);
  const mounted = React.useRef(true);

  const finishWith = (data: string, movement: Movement) => {
    leaving.current = true;
    // 開いたまま変えずに確定したら保存せず移動だけする（glide の既定と同じ）。打鍵で開いた欄（initialValue あり）は
    // 最初の 1 文字が既に入力なので、元の値と同じに見えても変更として渡す（同値なら onCellEdited が捨てる）。
    const changed = initialValue !== undefined || data !== initial.current;
    onFinishedEditing(changed ? { ...value, data } : undefined, movement);
  };

  React.useLayoutEffect(() => {
    mounted.current = true;
    const ta = ref.current;
    if (!ta) return;
    ta.focus();
    const n = ta.value.length;
    ta.setSelectionRange(isHighlighted && !ahead?.text ? 0 : n, n);
    if (ahead) {
      typeAhead?.drop();
      if (ahead.text) onChange({ ...value, data: ta.value });
      if (ahead.finish) finishWith(ta.value, ahead.finish);
    }
    return () => {
      mounted.current = false;
    };
    // 開いたときだけ（打鍵ごとに選択を戻さない）。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 欄の外を押した（glide はそれで確定して閉じる）ことを、フォーカスの移動より先に知る。
  React.useEffect(() => {
    const onDown = (e: Event) => {
      const box = ref.current?.closest(".gdg-clip-region");
      if (!box || !(e.target instanceof Node) || !box.contains(e.target)) leaving.current = true;
    };
    document.addEventListener("pointerdown", onDown, true);
    document.addEventListener("contextmenu", onDown, true);
    return () => {
      document.removeEventListener("pointerdown", onDown, true);
      document.removeEventListener("contextmenu", onDown, true);
    };
  }, []);

  const finish = (e: React.KeyboardEvent<HTMLTextAreaElement>, movement: Movement) => {
    e.preventDefault();
    e.stopPropagation();
    finishWith(e.currentTarget.value, movement);
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
        defaultValue={start}
        onChange={(e) => {
          setText(e.target.value);
          onChange({ ...value, data: e.target.value });
        }}
        onBlur={(e) => {
          if (leaving.current) return;
          // グリッド（編集欄の外・同じグリッドの中）へ移ったときだけ戻す。ページの他の場所へは移らせる。
          const ta = e.currentTarget;
          const portal = ta.closest("#portal");
          const to = e.relatedTarget;
          if (!portal || !(to instanceof Node) || portal.contains(to) || !portal.parentElement?.contains(to)) return;
          // 移動が終わってから戻す（blur の中で focus を呼ぶと、進行中の移動に上書きされる）。次の打鍵より先に走る。
          queueMicrotask(() => {
            if (mounted.current && !leaving.current && document.activeElement !== ta) ta.focus({ preventScroll: true });
          });
        }}
        onKeyDown={(e) => {
          if (e.nativeEvent.isComposing || e.keyCode === 229) return;
          if (e.key === "Escape") leaving.current = true;
          else if (e.key === "Enter" && !e.shiftKey) finish(e, [0, 1]);
          else if (e.key === "Tab") finish(e, [e.shiftKey ? -1 : 1, 0]);
        }}
      />
    </div>
  );
}
