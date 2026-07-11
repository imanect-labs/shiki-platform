"use client";

/// スラッシュコマンド（Task 11P.3）: 「/」でブロック挿入メニューを開く。
/// 挿入系（見出し/リスト/チェックリスト/表/コード/引用/区切り線）を提供し、
/// 埋め込み（11P.6）と AI アクション（11P.5）は登録フックで後段 PR が追加する。

import { Extension, type Editor, type Range } from "@tiptap/core";
import { ReactRenderer } from "@tiptap/react";
import Suggestion, { type SuggestionProps } from "@tiptap/suggestion";
import {
  Code2,
  Heading1,
  Heading2,
  Heading3,
  List,
  ListChecks,
  ListOrdered,
  Minus,
  Quote,
  Table2,
  type LucideIcon,
} from "lucide-react";
import * as React from "react";

export interface SlashItem {
  title: string;
  description: string;
  icon: LucideIcon;
  keywords: string[];
  command: (editor: Editor, range: Range) => void;
}

/// 既定の挿入コマンド群（11P.3 スコープ）。
export function defaultSlashItems(): SlashItem[] {
  return [
    {
      title: "見出し 1",
      description: "大見出し",
      icon: Heading1,
      keywords: ["h1", "heading", "midashi"],
      command: (editor, range) =>
        editor.chain().focus().deleteRange(range).setNode("heading", { level: 1 }).run(),
    },
    {
      title: "見出し 2",
      description: "中見出し",
      icon: Heading2,
      keywords: ["h2", "heading"],
      command: (editor, range) =>
        editor.chain().focus().deleteRange(range).setNode("heading", { level: 2 }).run(),
    },
    {
      title: "見出し 3",
      description: "小見出し",
      icon: Heading3,
      keywords: ["h3", "heading"],
      command: (editor, range) =>
        editor.chain().focus().deleteRange(range).setNode("heading", { level: 3 }).run(),
    },
    {
      title: "箇条書きリスト",
      description: "シンプルなリスト",
      icon: List,
      keywords: ["bullet", "list", "ul"],
      command: (editor, range) =>
        editor.chain().focus().deleteRange(range).toggleBulletList().run(),
    },
    {
      title: "番号付きリスト",
      description: "順序のあるリスト",
      icon: ListOrdered,
      keywords: ["ordered", "number", "ol"],
      command: (editor, range) =>
        editor.chain().focus().deleteRange(range).toggleOrderedList().run(),
    },
    {
      title: "チェックリスト",
      description: "タスクの完了を管理",
      icon: ListChecks,
      keywords: ["task", "todo", "check"],
      command: (editor, range) =>
        editor.chain().focus().deleteRange(range).toggleTaskList().run(),
    },
    {
      title: "表",
      description: "3×3 の表を挿入",
      icon: Table2,
      keywords: ["table", "hyou"],
      command: (editor, range) =>
        editor
          .chain()
          .focus()
          .deleteRange(range)
          .insertTable({ rows: 3, cols: 3, withHeaderRow: true })
          .run(),
    },
    {
      title: "コードブロック",
      description: "シンタックスハイライト付きコード",
      icon: Code2,
      keywords: ["code", "pre"],
      command: (editor, range) =>
        editor.chain().focus().deleteRange(range).setCodeBlock().run(),
    },
    {
      title: "引用",
      description: "引用ブロック",
      icon: Quote,
      keywords: ["quote", "blockquote"],
      command: (editor, range) =>
        editor.chain().focus().deleteRange(range).setBlockquote().run(),
    },
    {
      title: "区切り線",
      description: "水平線で区切る",
      icon: Minus,
      keywords: ["hr", "divider", "rule"],
      command: (editor, range) =>
        editor.chain().focus().deleteRange(range).setHorizontalRule().run(),
    },
  ];
}

interface MenuProps {
  items: SlashItem[];
  command: (item: SlashItem) => void;
}

interface MenuHandle {
  onKeyDown: (event: KeyboardEvent) => boolean;
}

const SlashMenu = React.forwardRef<MenuHandle, MenuProps>(function SlashMenu(
  { items, command },
  ref,
) {
  const [selected, setSelected] = React.useState(0);
  React.useEffect(() => setSelected(0), [items]);

  React.useImperativeHandle(ref, () => ({
    onKeyDown: (event: KeyboardEvent) => {
      if (event.key === "ArrowDown") {
        setSelected((s) => (s + 1) % Math.max(items.length, 1));
        return true;
      }
      if (event.key === "ArrowUp") {
        setSelected((s) => (s - 1 + items.length) % Math.max(items.length, 1));
        return true;
      }
      if (event.key === "Enter") {
        const item = items[selected];
        if (item) command(item);
        return true;
      }
      return false;
    },
  }));

  if (items.length === 0) {
    return (
      <div className="w-64 rounded-lg border bg-popover p-2 text-sm text-muted-foreground shadow-lg">
        該当するコマンドがありません
      </div>
    );
  }
  return (
    <div
      role="menu"
      aria-label="ブロックを挿入"
      className="max-h-80 w-72 overflow-y-auto rounded-lg border bg-popover p-1.5 shadow-lg"
      data-testid="slash-menu"
    >
      {items.map((item, index) => {
        const Icon = item.icon;
        return (
          <button
            key={item.title}
            type="button"
            role="menuitem"
            onMouseEnter={() => setSelected(index)}
            onClick={() => command(item)}
            className={`flex w-full items-center gap-3 rounded-md px-2.5 py-2 text-left text-sm transition-colors ${
              index === selected ? "bg-accent text-accent-foreground" : "text-foreground"
            }`}
          >
            <span className="flex size-8 shrink-0 items-center justify-center rounded-md border bg-background">
              <Icon className="size-4 text-muted-foreground" />
            </span>
            <span className="min-w-0">
              <span className="block truncate font-medium">{item.title}</span>
              <span className="block truncate text-xs text-muted-foreground">
                {item.description}
              </span>
            </span>
          </button>
        );
      })}
    </div>
  );
});

/// メニューの絶対配置コンテナをカーソル矩形に追従させる。
function positionPopup(popup: HTMLDivElement, clientRect: (() => DOMRect | null) | null) {
  const rect = clientRect?.();
  if (!rect) return;
  const margin = 8;
  popup.style.left = `${Math.min(rect.left, window.innerWidth - 300)}px`;
  const spaceBelow = window.innerHeight - rect.bottom;
  if (spaceBelow < 340) {
    popup.style.top = "auto";
    popup.style.bottom = `${window.innerHeight - rect.top + margin}px`;
  } else {
    popup.style.bottom = "auto";
    popup.style.top = `${rect.bottom + margin}px`;
  }
}

/// スラッシュコマンド拡張を作る。`extraItems` で後段 PR（埋め込み/AI）が項目を足す。
export function createSlashCommand(extraItems: () => SlashItem[] = () => []) {
  return Extension.create({
    name: "slashCommand",
    addProseMirrorPlugins() {
      return [
        Suggestion<SlashItem, SlashItem>({
          editor: this.editor,
          char: "/",
          startOfLine: false,
          command: ({ editor, range, props }) => props.command(editor, range),
          items: ({ query }) => {
            const all = [...defaultSlashItems(), ...extraItems()];
            const q = query.toLowerCase();
            if (!q) return all;
            return all.filter(
              (item) =>
                item.title.toLowerCase().includes(q) ||
                item.keywords.some((k) => k.includes(q)),
            );
          },
          render: () => {
            let component: ReactRenderer<MenuHandle, MenuProps> | null = null;
            let popup: HTMLDivElement | null = null;
            return {
              onStart: (props: SuggestionProps<SlashItem, SlashItem>) => {
                component = new ReactRenderer(SlashMenu, {
                  props: {
                    items: props.items,
                    command: (item: SlashItem) => props.command(item),
                  },
                  editor: props.editor,
                });
                popup = document.createElement("div");
                popup.style.position = "fixed";
                popup.style.zIndex = "50";
                popup.appendChild(component.element);
                document.body.appendChild(popup);
                positionPopup(popup, props.clientRect ?? null);
              },
              onUpdate: (props: SuggestionProps<SlashItem, SlashItem>) => {
                component?.updateProps({
                  items: props.items,
                  command: (item: SlashItem) => props.command(item),
                });
                if (popup) positionPopup(popup, props.clientRect ?? null);
              },
              onKeyDown: ({ event }) => {
                if (event.key === "Escape") {
                  popup?.remove();
                  return true;
                }
                return component?.ref?.onKeyDown(event) ?? false;
              },
              onExit: () => {
                popup?.remove();
                component?.destroy();
                component = null;
                popup = null;
              },
            };
          },
        }),
      ];
    },
  });
}
