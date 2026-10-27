"use client";

import * as React from "react";

/// 回答の中の開閉（思考パネル・詳細な思考・操作のタイムライン）を、行を作り直しても保つ記憶。
///
/// 生成中の行と確定後の行は別のコンポーネントで、確定（とその後の読み直し）のたびに作り直される。
/// 開閉を各コンポーネントの state にだけ持つと、確定した瞬間に開いていたパネルが閉じ、その下で
/// 読んでいた位置が（閉じたパネルの高さぶん）飛ぶ。会話ごとの記憶に置き、同じ回答の同じ
/// まとまりなら作り直した後も同じ開閉で描く。
const MemoryContext = React.createContext<Map<string, boolean | null> | null>(null);

/// 記憶の入れ物（会話 1 つにつき 1 つ。スレッドを切り替えたら作り直す）。
export function DisclosureMemoryProvider({ children }: { children: React.ReactNode }) {
  const [memory] = React.useState(() => new Map<string, boolean | null>());
  return <MemoryContext.Provider value={memory}>{children}</MemoryContext.Provider>;
}

/// `useState(initial)` の代わり。同じ `key`（回答 ID ＋まとまりの位置＋部位）なら、作り直しても
/// 前の開閉で始まる。`key` が無い（回答 ID がまだ分からない）・記憶の外では、ただの state。
/// `initial` に null を渡すと「まだ押していない」を表せる（呼び出し側で設定の既定に従う）。
export function useRememberedOpen(key: string | null): [boolean, (open: boolean) => void];
export function useRememberedOpen(
  key: string | null,
  initial: null,
): [boolean | null, (open: boolean) => void];
export function useRememberedOpen(
  key: string | null,
  initial: boolean | null = false,
): [boolean | null, (open: boolean) => void] {
  const memory = React.useContext(MemoryContext);
  const [open, setOpen] = React.useState<boolean | null>(() =>
    key && memory?.has(key) ? (memory.get(key) ?? initial) : initial,
  );
  // 生成中に回答 ID が届いて `key` が付いた場合も、いまの開閉を覚える。
  React.useEffect(() => {
    if (key && memory) memory.set(key, open);
  }, [key, memory, open]);
  return [open, setOpen];
}
