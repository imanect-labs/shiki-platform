"use client";

/// 自動実行のダイアログの、いま使えるようにしているものの一覧（D-90）。
///
/// [`DelegationList`] は、ワークフローが誰の代わりに何を使えるようにしているか（委譲の一覧）を 1 行ずつ言う。
/// 名前はテーブルの一覧・ドライブ・同意計画から引き、引けない対象は「名前を読み込めないテーブル」のように言う（ID を出さず、
/// 名前のように「」で囲まない）。引いている間は「読み込み中…」と言う（引けなかったように見せない）。

import * as React from "react";

import { useDriveItem } from "@/components/workflow/editor/config-panel/forms-files";
import { cachedUserName, resolveUserNames } from "@/lib/user-names";
import type { DelegationEntry } from "@/lib/workflow-api";

/// 名前を引けない対象の呼び名（付録 D の N4.8）。名前のように「」で囲まない。
const UNNAMED = "名前を読み込めない対象";
/// 名前を引けない人の呼び名（「さん」を付けない）。
const UNNAMED_PERSON = "名前を読み込めない人";
/// 名前を引いている間の文言（引けなかったように見せない・付録 C の P5）。
const LOADING = "読み込み中…";

/// 対象の名前（引いている間は `loading`、引けなければ `missing`）。
type ObjectName = { kind: "name"; name: string } | { kind: "loading" } | { kind: "missing" };

/// 委譲 1 件を 1 つの句で言う（付録 D の N4.8 の「行」）。名前を引けない対象は「」で囲まずに言う（名前のように見せない）。
function phrase(kind: string | null, relation: string, name: ObjectName): string {
  if (name.kind === "loading") return LOADING;
  const write = relation === "editor" || relation === "owner";
  const named = name.kind === "name";
  const n = named ? name.name : "";
  switch (kind) {
    case "data_table": {
      const what = named ? `テーブル「${n}」` : "名前を読み込めないテーブル";
      return write ? `${what}に書き込む` : `${what}を読む`;
    }
    case "folder": {
      const what = named ? `フォルダー「${n}」` : "名前を読み込めないフォルダー";
      return write ? `${what}にファイルを保存する` : `${what}のファイルを読む`;
    }
    case "file": {
      const what = named ? `ファイル「${n}」` : "名前を読み込めないファイル";
      return write ? `${what}に書き込む` : `${what}を読む`;
    }
    case "secret":
      return named ? `シークレット「${n}」を使う` : "名前を読み込めないシークレットを使う";
    case "workflow":
      return named ? `ワークフロー「${n}」を実行する` : "名前を読み込めないワークフローを実行する";
    default:
      return UNNAMED;
  }
}

/// ドライブのフォルダーやファイルの委譲の句（名前はドライブから引く）。
function DrivePhrase({ d }: { d: DelegationEntry }) {
  const { item, loading } = useDriveItem(d.objectId);
  const name: ObjectName = loading ? { kind: "loading" } : item ? { kind: "name", name: item.name } : { kind: "missing" };
  return <>{phrase(d.objectKind, d.relation, name)}</>;
}

/// いま使えるようにしているもの（委譲の一覧）。`tableName`・`otherName` は名前の引き方（見つからなければ undefined）。
/// `tableLoading` はそのテーブルの名前を引いている間か（そのあいだは引けなかったと言わない）。
export function DelegationList({
  delegations,
  meId,
  tableName,
  otherName,
  tableLoading,
}: {
  delegations: DelegationEntry[];
  meId: string | null;
  tableName: (id: string) => string | undefined;
  otherName: (kind: string, id: string) => string | undefined;
  tableLoading?: (id: string) => boolean;
}) {
  /// 人の名前を引き終えたか（引き終えるまでは「読み込み中…」と言い、引けなかったように見せない）。
  const [peopleLoaded, setPeopleLoaded] = React.useState(false);
  const [, setNames] = React.useState(0);
  const people = React.useMemo(() => [...new Set(delegations.map((d) => d.delegator))], [delegations]);
  React.useEffect(() => {
    let active = true;
    void resolveUserNames(people).then((learned) => {
      if (!active) return;
      setPeopleLoaded(true);
      if (learned) setNames((n) => n + 1);
    });
    return () => {
      active = false;
    };
  }, [people]);
  const who = (id: string): string => {
    if (id === meId) return "あなたが使えるようにしました";
    const known = cachedUserName(id);
    if (known) return `${known}さんが使えるようにしました`;
    if (known === undefined && !peopleLoaded) return LOADING;
    return `${UNNAMED_PERSON}が使えるようにしました`;
  };
  const line = (d: DelegationEntry): React.ReactNode => {
    if (!d.objectKind || !d.objectId) return UNNAMED;
    if (d.objectKind === "folder" || d.objectKind === "file") return <DrivePhrase d={d} />;
    const found = d.objectKind === "data_table" ? tableName(d.objectId) : otherName(d.objectKind, d.objectId);
    const name: ObjectName = found
      ? { kind: "name", name: found }
      : d.objectKind === "data_table" && tableLoading?.(d.objectId)
        ? { kind: "loading" }
        : { kind: "missing" };
    return phrase(d.objectKind, d.relation, name);
  };
  return (
    <ul className="space-y-1.5" data-testid="enable-delegations">
      {delegations.map((d) => (
        <li key={`${d.delegator}:${d.scope}:${d.objectRef}`} className="rounded-md border px-2.5 py-2 text-xs">
          <span className="block font-medium [overflow-wrap:anywhere]">{line(d)}</span>
          <span className="block text-muted-foreground">{who(d.delegator)}</span>
        </li>
      ))}
    </ul>
  );
}
