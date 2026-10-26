"use client";

/// ワークフローの設定で使うテーブルの一覧と、テーブルを選ぶ欄（data.* のブロック・承認を待つブロック・データ変更の
/// きっかけ・有効化の画面が共通に使う）。
///
/// 一覧（GET /data/tables）は更新の新しい順に 200 件で打ち切る。束縛済みのテーブルがその中に無いと、一覧から探す
/// 欄は空欄に見え、列・状態の選択肢も出ない（BUG-68）。そこで一覧に無い束縛済みの id は 1 件ずつ
/// GET /data/tables/{id}（一覧と同じく、閲覧者の見え方で絞った定義が返る）で取り、一覧に足して扱う。読めない id は
/// 空欄のまま黙らずにそう出す（サーバは見えないテーブルにも、無い・削除済みと同じ 404 を返すので、どちらかは
/// 言い分けない）。

import * as React from "react";

import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { DataApiError, getTable, listTables, type DataTable } from "@/lib/data-api";

/// id のテーブルの読み込みの状態。
export type TableLookup =
  /// 束縛していない。
  | { state: "none" }
  | { state: "loading" }
  | { state: "ok"; table: DataTable }
  /// 見えない・無い・削除された（404）、または id の形が違う（400）。
  | { state: "unreadable" }
  /// それ以外の誤り（通信・サーバ）。
  | { state: "failed" };

export type DataTables = {
  /// 選べるテーブル（一覧に、一覧に無かった束縛済みのテーブルを足したもの）。一覧の読み込み中は null。
  tables: DataTable[] | null;
  /// 一覧を読めなかったときの誤り（束縛済みのテーブルは、それでも 1 件ずつ取りに行く）。
  error: string | null;
  /// id のテーブル（[`useDataTables`] の `bound` に渡した id を引く）。
  lookup: (id: string | null | undefined) => TableLookup;
};

type Fetched = DataTable | "loading" | "unreadable" | "failed";

/// 自分が使えるテーブルの一覧と、束縛済みのテーブル（パネルを開くたびに一覧を 1 回だけ読む・`enabled` が偽の間は
/// 読まない）。`bound` のうち一覧に無い id だけを 1 件ずつ取る。
export function useDataTables({
  bound = [],
  enabled = true,
}: { bound?: readonly (string | null | undefined)[]; enabled?: boolean } = {}): DataTables {
  const [listed, setListed] = React.useState<DataTable[] | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [fetched, setFetched] = React.useState<ReadonlyMap<string, Fetched>>(() => new Map());
  // 取りに行った id（同じ id を 2 度取らない・応答が来る前の再描画でも）。
  const requested = React.useRef(new Set<string>());
  const alive = React.useRef(true);
  React.useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  React.useEffect(() => {
    if (!enabled) return;
    let active = true;
    listTables()
      .then((t) => active && setListed(t))
      .catch((e) => {
        if (!active) return;
        setListed([]);
        setError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      active = false;
    };
  }, [enabled]);

  const ids = [...new Set(bound.filter((id): id is string => typeof id === "string" && id !== ""))];
  const key = ids.join("\n");
  React.useEffect(() => {
    if (!enabled || listed === null) return;
    const want = key
      .split("\n")
      .filter((id) => id !== "" && !requested.current.has(id) && !listed.some((t) => t.id === id));
    if (!want.length) return;
    for (const id of want) requested.current.add(id);
    const put = (id: string, v: Fetched) => setFetched((m) => new Map(m).set(id, v));
    for (const id of want) {
      put(id, "loading");
      getTable(id)
        .then((t) => alive.current && put(id, t))
        .catch((e) => {
          if (!alive.current) return;
          const status = e instanceof DataApiError ? e.status : 0;
          put(id, status === 400 || status === 403 || status === 404 ? "unreadable" : "failed");
        });
    }
  }, [enabled, listed, key]);

  const tables = React.useMemo(() => {
    if (listed === null) return null;
    const extra = [...fetched.values()].filter(
      (v): v is DataTable => typeof v === "object" && !listed.some((t) => t.id === v.id),
    );
    return extra.length ? [...listed, ...extra] : listed;
  }, [listed, fetched]);

  const lookup = React.useCallback(
    (id: string | null | undefined): TableLookup => {
      if (!id) return { state: "none" };
      if (tables === null) return { state: "loading" };
      const hit = tables.find((t) => t.id === id);
      if (hit) return { state: "ok", table: hit };
      const f = fetched.get(id);
      // まだ取りに行っていない（束縛が変わった直後の描画）も読み込み中。
      if (f === undefined || f === "loading") return { state: "loading" };
      // 1 件で取れた（返った id の書き方が束縛と違っても、その id で取れたもの）。
      if (typeof f === "object") return { state: "ok", table: f };
      return { state: f };
    },
    [tables, fetched],
  );
  return { tables, error, lookup };
}

/// 束縛済みのテーブルを読めないときの注記。
export const UNREADABLE_TABLE = "このテーブルを読めません（または削除されました）";

/// テーブルの選択（一覧から固定値で選ぶ）。
///
/// 束縛済みの値が選択肢に無いとき（読み込み中・読めない・`eligible` で外れる）も空欄にしない。欄にそう出し、
/// 読めない・選べないときは欄の下に理由を出す。
export function DataTableSelect({
  value,
  onChange,
  source,
  eligible,
  ineligibleNote,
  label = "対象のテーブル",
  id,
}: {
  value: string | null;
  onChange: (id: string) => void;
  source: DataTables;
  /// 選べるテーブル（省略で全部）。
  eligible?: (t: DataTable) => boolean;
  /// 束縛済みのテーブルが `eligible` で外れるときの注記。
  ineligibleNote?: string;
  label?: string;
  id?: string;
}) {
  const options = source.tables === null ? null : eligible ? source.tables.filter(eligible) : source.tables;
  const bound = source.lookup(value);
  const ineligible = bound.state === "ok" && eligible !== undefined && !eligible(bound.table);
  const note =
    bound.state === "unreadable"
      ? UNREADABLE_TABLE
      : bound.state === "failed"
        ? "このテーブルを読み込めませんでした"
        : ineligible
          ? (ineligibleNote ?? "このテーブルは選べません")
          : null;
  // Select は値に合う選択肢が無いと何も出さない（placeholder は値が空のときだけ）ので、出すものを渡す。
  const shown =
    bound.state === "loading" ? (
      <span className="text-muted-foreground">読み込み中…</span>
    ) : bound.state === "unreadable" || bound.state === "failed" ? (
      <span className="text-destructive">読めないテーブル</span>
    ) : ineligible ? (
      bound.table.name
    ) : undefined;
  return (
    <div className="space-y-1">
      <Select value={value ?? ""} onValueChange={onChange} disabled={options === null}>
        <SelectTrigger id={id} aria-label={label} className="h-8 text-[13px]">
          <SelectValue placeholder={options === null ? "読み込み中…" : "テーブルを選ぶ"}>{shown}</SelectValue>
        </SelectTrigger>
        <SelectContent>
          {(options ?? []).map((t) => (
            <SelectItem key={t.id} value={t.id}>
              {t.name}
            </SelectItem>
          ))}
          {options && options.length === 0 ? (
            <div className="px-2 py-1.5 text-xs text-muted-foreground">使えるテーブルがありません</div>
          ) : null}
        </SelectContent>
      </Select>
      {note ? (
        <p className="text-[11px] leading-relaxed text-destructive" data-testid="workflow-table-note">
          {note}
        </p>
      ) : null}
    </div>
  );
}
