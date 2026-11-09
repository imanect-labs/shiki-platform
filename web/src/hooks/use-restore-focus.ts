"use client";

import * as React from "react";

/// 開く元のボタンが Radix の Trigger でないダイアログで、閉じたら開いたときにフォーカスがあった要素（開いたボタン
/// など）へフォーカスを戻す。返す 2 つをそのまま `DialogContent` に渡す。
///
/// Radix の Dialog は閉じたときに常に Trigger へ戻そうとし、Trigger が無いと何もしないので、そのままではフォーカスが
/// ページの先頭（document.body）に落ち、キーボードで操作する人が位置を見失う（BUG-261・BUG-263）。
///
/// `onCloseAutoFocus` を渡すと先に呼ぶ。そこで `preventDefault` して別の要素へ移したときと、戻す先が画面から消えた
/// ときは何もしない。
export function useRestoreFocus(onCloseAutoFocus?: (event: Event) => void): {
  onOpenAutoFocus: (event: Event) => void;
  onCloseAutoFocus: (event: Event) => void;
} {
  // 開いたときにフォーカスがあった要素（閉じたときに戻す先）。
  const opener = React.useRef<HTMLElement | null>(null);
  return React.useMemo(
    () => ({
      // ダイアログの中へフォーカスを移す前に呼ばれるので、まだ開いた元の要素にフォーカスがある。
      onOpenAutoFocus: () => {
        const el = document.activeElement;
        opener.current = el instanceof HTMLElement && el !== document.body ? el : null;
      },
      onCloseAutoFocus: (event: Event) => {
        onCloseAutoFocus?.(event);
        const el = opener.current;
        opener.current = null;
        if (event.defaultPrevented || !el?.isConnected) return;
        event.preventDefault();
        el.focus();
      },
    }),
    [onCloseAutoFocus],
  );
}
