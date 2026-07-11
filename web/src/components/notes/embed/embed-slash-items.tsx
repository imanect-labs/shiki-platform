"use client";

/// 埋め込み挿入のスラッシュコマンド項目（Task 11P.6）。
///
/// genui はチャットの AI 生成（emit_ui）から貼り付く想定のためスラッシュには出さず、
/// ユーザーが手で挿入するのは iframe（Web/ミニアプリ）とドライブファイル参照の 2 種。
/// URL/ノート ID は簡易プロンプトで受ける（将来はミニアプリ/ファイルのピッカーに置換）。

import { FileText, MonitorSmartphone } from "lucide-react";

import type { SlashItem } from "../slash-command";
import { isSafeHttpUrl } from "./types";

/// スラッシュメニューに追加する埋め込み項目を返す（`extraSlashItems` フックへ渡す）。
export function embedSlashItems(): SlashItem[] {
  return [
    {
      title: "埋め込み: Web / ミニアプリ",
      description: "別オリジンの安全な iframe（https）",
      icon: MonitorSmartphone,
      keywords: ["embed", "iframe", "app", "web", "umekomi"],
      command: (editor, range) => {
        const src = window.prompt("埋め込む URL（https）を入力してください");
        editor.chain().focus().deleteRange(range).run();
        if (!src || !isSafeHttpUrl(src)) return;
        editor.chain().focus().insertShikiEmbed({ kind: "iframe", src }).run();
      },
    },
    {
      title: "埋め込み: ドライブファイル",
      description: "閲覧者本人の権限で解決",
      icon: FileText,
      keywords: ["embed", "drive", "file", "image", "umekomi"],
      command: (editor, range) => {
        const nodeId = window.prompt("埋め込むファイルのノート ID を入力してください");
        editor.chain().focus().deleteRange(range).run();
        if (!nodeId) return;
        editor.chain().focus().insertShikiEmbed({ kind: "drive", node_id: nodeId.trim() }).run();
      },
    },
  ];
}
