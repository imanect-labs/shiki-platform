/// チャットで「この発話に使うツール」の選択（#734）。
///
/// モデル選択（model-choice.ts）と同じくスコープ（スレッド id かホーム）ごとに持つが、こちらは
/// **その発話にだけ**効く（送信したら空に戻す・サーバも次の発話へ持ち越さない）ので永続しない。
/// 送信時は呼び出し側が `takeToolChoice(threadId)` で読み出して API の `tools` に載せる。
///
/// 選べるツールは `ToolName`（codegen の語彙）で束縛した一覧に限る。語彙にないツールを並べると
/// 型エラーになり、サーバ側も語彙外は 400 で拒否する。

import * as React from "react";
import { Globe, LayoutTemplate, Search, Terminal, type LucideIcon } from "lucide-react";

import type { ToolName } from "@/generated/gui-spec";

export type SelectableTool = {
  name: ToolName;
  /// チップ・メニューに出す短い名前。
  label: string;
  /// 何をしてくれるか（メニュー・シートの 2 行目）。
  hint: string;
  icon: LucideIcon;
};

/// 利用者が選べるツール。内部用（skill・emit_workflow・fs_* 等）は出さない。
/// 選んでも提示されるツールの集合・承認は変わらない（使えないモードでは AI がそう伝える）。
export const SELECTABLE_TOOLS: SelectableTool[] = [
  { name: "doc_search", label: "社内文書検索", hint: "閲覧できる社内文書から探す", icon: Search },
  { name: "web_search", label: "Web 検索", hint: "インターネットで最新の情報を調べる", icon: Globe },
  { name: "code_interpreter", label: "コード実行", hint: "計算・集計・グラフ作成をコードで行う", icon: Terminal },
  { name: "emit_ui", label: "表・グラフで表示", hint: "結果を表やグラフ、フォームで見せる", icon: LayoutTemplate },
];

const byName = new Map(SELECTABLE_TOOLS.map((t) => [t.name, t]));

export function selectableTool(name: ToolName): SelectableTool | undefined {
  return byName.get(name);
}

const store = new Map<string, ToolName[]>();
const listeners = new Map<string, Set<() => void>>();
const EMPTY: ToolName[] = [];

function notify(scope: string) {
  listeners.get(scope)?.forEach((l) => l());
}

export function readToolChoice(scope: string): ToolName[] {
  return store.get(scope) ?? EMPTY;
}

export function writeToolChoice(scope: string, tools: ToolName[]): void {
  if (tools.length === 0) store.delete(scope);
  else store.set(scope, tools);
  notify(scope);
}

/// 送信時に読み出して空に戻す（この発話にだけ効く）。
export function takeToolChoice(scope: string): ToolName[] {
  const tools = readToolChoice(scope);
  if (tools.length > 0) writeToolChoice(scope, []);
  return tools;
}

/// ホームで選んだツールを新しいスレッドへ移す（最初の発話に載せるため）。
export function moveToolChoice(from: string, to: string): void {
  const tools = readToolChoice(from);
  if (tools.length === 0) return;
  writeToolChoice(to, tools);
  writeToolChoice(from, []);
}

/// スコープのツール選択を購読する（`toggle` で 1 つずつ付け外し）。
export function useToolChoice(scope: string): {
  tools: ToolName[];
  toggle: (name: ToolName) => void;
  clear: (name: ToolName) => void;
} {
  const subscribe = React.useCallback(
    (cb: () => void) => {
      let set = listeners.get(scope);
      if (!set) listeners.set(scope, (set = new Set()));
      set.add(cb);
      return () => set.delete(cb);
    },
    [scope],
  );
  const tools = React.useSyncExternalStore(
    subscribe,
    () => readToolChoice(scope),
    () => EMPTY,
  );
  const toggle = React.useCallback(
    (name: ToolName) => {
      const cur = readToolChoice(scope);
      writeToolChoice(scope, cur.includes(name) ? cur.filter((t) => t !== name) : [...cur, name]);
    },
    [scope],
  );
  const clear = React.useCallback(
    (name: ToolName) => writeToolChoice(scope, readToolChoice(scope).filter((t) => t !== name)),
    [scope],
  );
  return { tools, toggle, clear };
}
