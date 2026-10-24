/// 入れ子の Esc の扱い（ダイアログ・シート・ポップオーバーの中の候補一覧）。
///
/// Radix の Dialog / Popover は Esc を document の capture 段で拾うので、中の入力欄が React の
/// `stopPropagation` で止めても外側が先に閉じる（候補一覧の Esc でダイアログごと閉じて入力が消えていた・
/// 2-A-P2-2）。候補一覧を開いている入力欄は `data-local-escape="true"` を付け、外側の Content はその要素から
/// 来た Esc では閉じない（入力欄が自分の一覧だけを閉じる）。
export const LOCAL_ESCAPE_ATTR = "data-local-escape";

/// 内側の部品が自分で処理する Esc か（外側の Dialog / Popover は閉じない）。
export function isLocalEscape(e: KeyboardEvent): boolean {
  const t = e.target;
  return t instanceof Element && t.closest(`[${LOCAL_ESCAPE_ATTR}="true"]`) !== null;
}

/// Content の `onEscapeKeyDown` に重ねる（呼び出し側の処理は内側の Esc でなければそのまま呼ぶ）。
export function guardLocalEscape(handler?: (e: KeyboardEvent) => void) {
  return (e: KeyboardEvent) => {
    if (isLocalEscape(e)) {
      e.preventDefault();
      return;
    }
    handler?.(e);
  };
}
