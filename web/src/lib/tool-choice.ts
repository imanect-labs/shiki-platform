/// チャットで「この発話に使うツール」の選択（#734）。
///
/// モデル選択（model-choice.ts）と同じくスコープ（スレッド id かホーム）ごとに持つが、こちらは
/// **その発話にだけ**効く（送ったら空に戻す・サーバも次の自由入力の発話へ持ち越さない）ので
/// 永続しない。送信時は呼び出し側が `readToolChoice(threadId)` で読んで API の `tools` に載せ、
/// **受理されてから** `settleToolChoice` で空に戻す（400・通信失敗では選択を残す）。
///
/// 選べるツールの集合は codegen の `USER_SELECTABLE_TOOLS`（正本は
/// `crates/agent-core/src/vocab.rs`。サーバもこの外は 400）。ここに置くのは表示（名前・説明・
/// アイコン）だけで、`Record<UserSelectableTool, …>` で束縛するため、集合が増減すると
/// 表示の過不足が型エラーになる（手書きの選択肢一覧は作らない）。

import * as React from "react";
import { Globe, LayoutTemplate, Search, Terminal, type LucideIcon } from "lucide-react";

import { USER_SELECTABLE_TOOLS, type UserSelectableTool } from "@/generated/gui-spec";

export type SelectableTool = {
  name: UserSelectableTool;
  /// チップ・メニューに出す短い名前。
  label: string;
  /// 何をしてくれるか（メニュー・シートの 2 行目）。
  hint: string;
  icon: LucideIcon;
};

const DISPLAY: Record<UserSelectableTool, Omit<SelectableTool, "name">> = {
  doc_search: { label: "社内文書検索", hint: "閲覧できる社内文書から探す", icon: Search },
  web_search: { label: "Web 検索", hint: "インターネットで最新の情報を調べる", icon: Globe },
  code_interpreter: { label: "コード実行", hint: "計算・集計・グラフ作成をコードで行う", icon: Terminal },
  emit_ui: { label: "表・グラフで表示", hint: "結果を表やグラフ、フォームで見せる", icon: LayoutTemplate },
};

/// 利用者が選べるツール（並びは codegen の定数の順）。
/// 選んでも提示されるツールの集合・承認は変わらない（使えないモードでは AI がそう伝える）。
export const SELECTABLE_TOOLS: readonly SelectableTool[] = USER_SELECTABLE_TOOLS.map((name) => ({
  name,
  ...DISPLAY[name],
}));

const byName = new Map(SELECTABLE_TOOLS.map((t) => [t.name, t]));

export function selectableTool(name: UserSelectableTool): SelectableTool | undefined {
  return byName.get(name);
}

const store = new Map<string, UserSelectableTool[]>();
const listeners = new Map<string, Set<() => void>>();
const EMPTY: UserSelectableTool[] = [];

function notify(scope: string) {
  listeners.get(scope)?.forEach((l) => l());
}

export function readToolChoice(scope: string): UserSelectableTool[] {
  return store.get(scope) ?? EMPTY;
}

export function writeToolChoice(scope: string, tools: UserSelectableTool[]): void {
  if (tools.length === 0) store.delete(scope);
  else store.set(scope, tools);
  notify(scope);
}

/// 送った発話が**受理されたら**、送った選択を空に戻す（この発話にだけ効く）。
///
/// 送信時に `readToolChoice` で読んだ配列をそのまま渡す。POST の間に利用者が選び直していたら
/// （配列が差し替わっていたら）その選択は次の発話のものなので消さない。失敗時は呼ばない
/// （選択が残り、そのまま送り直せる）。
export function settleToolChoice(scope: string, sent: readonly UserSelectableTool[]): void {
  if (sent.length > 0 && readToolChoice(scope) === sent) writeToolChoice(scope, []);
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
  tools: UserSelectableTool[];
  toggle: (name: UserSelectableTool) => void;
  clear: (name: UserSelectableTool) => void;
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
    (name: UserSelectableTool) => {
      const cur = readToolChoice(scope);
      writeToolChoice(scope, cur.includes(name) ? cur.filter((t) => t !== name) : [...cur, name]);
    },
    [scope],
  );
  const clear = React.useCallback(
    (name: UserSelectableTool) => writeToolChoice(scope, readToolChoice(scope).filter((t) => t !== name)),
    [scope],
  );
  return { tools, toggle, clear };
}
