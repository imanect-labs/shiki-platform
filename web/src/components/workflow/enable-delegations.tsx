"use client";

/// 自動実行のダイアログの、いま使えるようにしているものの一覧（D-90）。
///
/// [`DelegationList`] は、ワークフローが誰の代わりに何を使えるようにしているか（委譲の一覧）を 1 行ずつ言う。
/// 名前はテーブルの一覧・ドライブ・同意計画から引き、引けない対象は「名前を読み込めない対象」と言う（ID を出さない）。

import * as React from "react";

import { DriveItemName } from "@/components/workflow/editor/config-panel/forms-files";
import { cachedUserName, resolveUserNames } from "@/lib/user-names";
import type { DelegationEntry } from "@/lib/workflow-api";

/// 名前を引けない対象の呼び名（付録 D の N4.8）。
const UNNAMED = "名前を読み込めない対象";

/// 委譲 1 件を 1 つの句で言う（付録 D の N4.8 の「行」）。`name` は対象の名前（引けなければ呼び名）。
function phrase(kind: string | null, relation: string, name: React.ReactNode): React.ReactNode {
  const write = relation === "editor" || relation === "owner";
  switch (kind) {
    case "data_table":
      return write ? <>テーブル「{name}」に書き込む</> : <>テーブル「{name}」を読む</>;
    case "folder":
      return write ? <>フォルダー「{name}」にファイルを保存する</> : <>フォルダー「{name}」のファイルを読む</>;
    case "file":
      return write ? <>ファイル「{name}」に書き込む</> : <>ファイル「{name}」を読む</>;
    case "secret":
      return <>シークレット「{name}」を使う</>;
    case "workflow":
      return <>ワークフロー「{name}」を実行する</>;
    default:
      return UNNAMED;
  }
}

/// いま使えるようにしているもの（委譲の一覧）。`tableName`・`otherName` は名前の引き方（見つからなければ undefined）。
export function DelegationList({
  delegations,
  meId,
  tableName,
  otherName,
}: {
  delegations: DelegationEntry[];
  meId: string | null;
  tableName: (id: string) => string | undefined;
  otherName: (kind: string, id: string) => string | undefined;
}) {
  const [, setNames] = React.useState(0);
  const people = React.useMemo(() => [...new Set(delegations.map((d) => d.delegator))], [delegations]);
  React.useEffect(() => {
    let active = true;
    void resolveUserNames(people).then((learned) => {
      if (active && learned) setNames((n) => n + 1);
    });
    return () => {
      active = false;
    };
  }, [people]);
  const who = (id: string) => (id === meId ? "あなた" : `${cachedUserName(id) ?? "名前を読み込めない人"}さん`);
  const name = (d: DelegationEntry): React.ReactNode => {
    if (!d.objectKind || !d.objectId) return UNNAMED;
    if (d.objectKind === "data_table") return tableName(d.objectId) ?? UNNAMED;
    if (d.objectKind === "folder" || d.objectKind === "file") {
      return <DriveItemName id={d.objectId} fallback={UNNAMED} />;
    }
    return otherName(d.objectKind, d.objectId) ?? UNNAMED;
  };
  return (
    <ul className="space-y-1.5" data-testid="enable-delegations">
      {delegations.map((d) => (
        <li key={`${d.delegator}:${d.scope}:${d.objectRef}`} className="rounded-md border px-2.5 py-2 text-xs">
          <span className="block font-medium [overflow-wrap:anywhere]">
            {d.objectKind ? phrase(d.objectKind, d.relation, name(d)) : UNNAMED}
          </span>
          <span className="block text-muted-foreground">{who(d.delegator)}が使えるようにしました</span>
        </li>
      ))}
    </ul>
  );
}
