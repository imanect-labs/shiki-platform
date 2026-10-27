import * as React from "react";

/// 一つだけ選ぶ並び（tablist・radiogroup）のキーボード操作。
/// 選んでいる項目だけを Tab で止まる場所にし（roving tabindex）、←→↑↓ で隣、Home / End で端の項目を
/// 選んでフォーカスを移す（WAI-ARIA の tabs / radio group の作法）。
export function useRovingSelect<T>(
  items: readonly T[],
  selected: T,
  onSelect: (item: T) => void,
): {
  ref: (index: number) => (el: HTMLButtonElement | null) => void;
  tabIndex: (index: number) => 0 | -1;
  onKeyDown: (index: number) => (e: React.KeyboardEvent) => void;
} {
  const refs = React.useRef<(HTMLButtonElement | null)[]>([]);
  const current = Math.max(0, items.indexOf(selected));

  const move = (next: number) => {
    onSelect(items[next]);
    refs.current[next]?.focus();
  };

  return {
    ref: (index) => (el) => {
      refs.current[index] = el;
    },
    tabIndex: (index) => (index === current ? 0 : -1),
    onKeyDown: (index) => (e) => {
      const n = items.length;
      let next: number | null = null;
      if (e.key === "ArrowRight" || e.key === "ArrowDown") next = (index + 1) % n;
      else if (e.key === "ArrowLeft" || e.key === "ArrowUp") next = (index - 1 + n) % n;
      else if (e.key === "Home") next = 0;
      else if (e.key === "End") next = n - 1;
      if (next === null) return;
      e.preventDefault();
      move(next);
    },
  };
}
