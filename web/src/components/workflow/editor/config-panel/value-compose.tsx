"use client";

/// 値の欄の「項目を並べる」と「リストを並べる」の行（BUG-118）。
///
/// 項目のまとまりはキーと値の行に、リストは 1 件ずつの行に分け、行ごとの値の欄（値の入れ方を選べる）は呼び出し側が
/// 描く（`renderSlot`・値の欄の部品と循環させない）。行と IR の値の行き来は value-shape.ts にある。
/// - 行は入力の途中の状態（キーが空・値が無い）を持てる。IR に入れるのは、キーと値がそろった行だけ。
/// - 外で値が変わった（元に戻す・AI の編集など）ときは、行を値から作り直す。
/// - キーをテーブルの列から選ばせるとき（`keys`）は、ほかの行で選んだ列を選べなくする。

import * as React from "react";
import { Plus, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import type { ValueExpr } from "@/generated/workflow-ir";
import {
  listItems,
  listOfItems,
  objectOfRows,
  objectRowIssue,
  objectRows,
  rowId,
  sameComposed,
  type ListItem,
  type ObjectRow,
} from "./value-shape";

/// キーの候補の 1 つ（テーブルの列）。`text` は文章の列（差し込めるようにする）、`numeric` は数値の列（打った数字を
/// 数として入れる・テーブルは数字の文字を数値の列に入れない）。
export type ComposeKeyOption = { key: string; label: string; text?: boolean; numeric?: boolean };

/// 項目のキーを一覧から選ばせるとき（テーブルの列）。
export type ComposeKeys = { options: ComposeKeyOption[] };

/// 行の値の欄に渡すもの。`label` は読み上げと data-value-expr、`shownLabel` は見出しに出す文字。
export type SlotProps = {
  label: string;
  shownLabel: string;
  value: ValueExpr | undefined;
  onChange: (next: ValueExpr) => void;
  depth: number;
  actions?: React.ReactNode;
  text?: boolean;
  numeric?: "always";
  autoFocus?: boolean;
};

export type RenderSlot = (props: SlotProps) => React.ReactNode;

/// 行と、行を作ったときの値（同じ値のあいだは行を作り直さない）。
function useRows<R>(
  value: unknown,
  split: (v: unknown) => R[],
  build: (rows: R[]) => unknown,
  onChange: (next: ValueExpr) => void,
): [R[], (next: R[]) => void] {
  const [state, setState] = React.useState(() => ({ rows: split(value), synced: value }));
  let rows = state.rows;
  if (state.synced !== value && !sameComposed(build(rows), value)) {
    rows = split(value);
    setState({ rows, synced: value });
  }
  const update = (next: R[]) => {
    const built = build(next);
    const same = sameComposed(built, value);
    setState({ rows: next, synced: same ? value : built });
    if (!same) onChange(built as ValueExpr);
  };
  return [rows, update];
}

function RemoveButton({ label, onClick }: { label: string; onClick: () => void }) {
  return (
    <Button variant="ghost" size="icon" className="size-7 shrink-0 pointer-coarse:size-9" aria-label={label} onClick={onClick}>
      <X className="size-3.5" aria-hidden />
    </Button>
  );
}

function AddButton({ label, onClick, disabled }: { label: string; onClick: () => void; disabled?: boolean }) {
  return (
    <Button variant="outline" size="sm" className="h-7 gap-1 text-xs pointer-coarse:h-9" onClick={onClick} disabled={disabled}>
      <Plus className="size-3.5" aria-hidden />
      {label}
    </Button>
  );
}

const ROW_ISSUE_TEXT = {
  blank_key: { column: "値を入れる列を選んでください。", key: "キーを入れてください。" },
  duplicate: { column: "同じ列がほかの行にもあります。別の列を選んでください。", key: "同じキーの項目がほかにもあります。別のキーにしてください。" },
  no_value: { column: "値を入れるまで、この列は保存しません。", key: "値を入れるまで、この項目は保存しません。" },
} as const;

/// 列から選ぶときの行の並び（テーブルの列の並び・一覧に無いキーは後ろ）。保存した値のキーは名前の順に並び直るので、
/// 開き直したときも列の並びで出す。
function byColumnOrder(rows: ObjectRow[], keys: ComposeKeys | undefined): ObjectRow[] {
  if (!keys) return rows;
  const order = (key: string) => {
    const i = keys.options.findIndex((o) => o.key === key);
    return i < 0 ? keys.options.length : i;
  };
  return [...rows].sort((a, b) => order(a.key) - order(b.key));
}

/// 「項目を並べる」: キーと値の行。
export function ObjectComposer({
  value,
  onChange,
  depth,
  keys,
  renderSlot,
}: {
  value: ValueExpr | undefined;
  onChange: (next: ValueExpr) => void;
  depth: number;
  keys?: ComposeKeys;
  renderSlot: RenderSlot;
}) {
  const [rows, update] = useRows<ObjectRow>(value, (v) => byColumnOrder(objectRows(v), keys), objectOfRows, onChange);
  // 列の一覧はテーブルを読んでから届くので、届いたときに 1 度だけ列の並びにする（並びだけで値は変わらない）。
  const [ordered, setOrdered] = React.useState(Boolean(keys));
  if (keys && !ordered) {
    setOrdered(true);
    update(byColumnOrder(rows, keys));
  }
  const [added, setAdded] = React.useState<number | null>(null);
  const noun = keys ? "column" : "key";
  const setRow = (id: number, patch: Partial<ObjectRow>) => update(rows.map((r) => (r.id === id ? { ...r, ...patch } : r)));
  const add = () => {
    const id = rowId();
    setAdded(id);
    update([...rows, { id, key: "", value: undefined }]);
  };
  const allUsed = keys ? keys.options.every((o) => rows.some((r) => r.key === o.key)) : false;

  return (
    <div className="space-y-1.5 rounded-md border bg-muted/40 p-2" data-testid="compose-object">
      {rows.length === 0 ? (
        <p className="text-[11px] leading-relaxed text-muted-foreground">
          {keys ? "値を入れる列を「列を追加」で選びます。" : "「項目を追加」で、キーと値の組を追加します。"}
        </p>
      ) : null}
      {rows.map((r, i) => {
        const option = keys?.options.find((o) => o.key === r.key);
        const name = option?.label ?? (r.key !== "" ? r.key : `${i + 1} 行目`);
        const issue = objectRowIssue(rows, i);
        return (
          <div key={r.id} role="group" aria-label={name} className="space-y-1.5 rounded-md border bg-background p-1.5" data-testid="compose-row">
            <div className="flex min-w-0 items-center gap-1">
              {keys ? (
                <Select value={r.key} onValueChange={(key) => setRow(r.id, { key })}>
                  <SelectTrigger className="h-8 min-w-0 flex-1 text-xs" aria-label="列" autoFocus={added === r.id}>
                    <SelectValue placeholder="列を選んでください" />
                  </SelectTrigger>
                  <SelectContent>
                    {keys.options.map((o) => (
                      <SelectItem
                        key={o.key}
                        value={o.key}
                        // ほかの行で選んだ列は選べない（同じ列を 2 回書くと、後の行は保存しない）。
                        disabled={rows.some((x) => x.id !== r.id && x.key === o.key)}
                      >
                        {o.label}
                      </SelectItem>
                    ))}
                    {/* 列の一覧に無いキー（列を消した後の定義など）も、選んだまま見せる。 */}
                    {r.key !== "" && !option ? <SelectItem value={r.key}>{r.key}</SelectItem> : null}
                  </SelectContent>
                </Select>
              ) : (
                <Input
                  value={r.key}
                  onChange={(e) => setRow(r.id, { key: e.target.value })}
                  placeholder="キー（英数字の名前）"
                  aria-label="キー"
                  autoFocus={added === r.id}
                  className="h-8 min-w-0 flex-1 font-mono text-xs max-md:text-base"
                />
              )}
              <RemoveButton label={`「${name}」を外す`} onClick={() => update(rows.filter((x) => x.id !== r.id))} />
            </div>
            {renderSlot({
              label: `「${name}」の値`,
              shownLabel: "値",
              value: r.value,
              onChange: (v) => setRow(r.id, { value: v }),
              depth: depth + 1,
              text: option?.text,
              numeric: option?.numeric ? "always" : undefined,
            })}
            {issue ? (
              <p className="text-[11px] leading-relaxed text-muted-foreground" data-testid="compose-row-issue">
                {ROW_ISSUE_TEXT[issue][noun]}
              </p>
            ) : null}
          </div>
        );
      })}
      <AddButton label={keys ? "列を追加" : "項目を追加"} onClick={add} disabled={allUsed} />
    </div>
  );
}

/// 「リストを並べる」: 1 件ずつの行。
export function ListComposer({
  label,
  value,
  onChange,
  depth,
  renderSlot,
}: {
  label: string;
  value: ValueExpr | undefined;
  onChange: (next: ValueExpr) => void;
  depth: number;
  renderSlot: RenderSlot;
}) {
  const [items, update] = useRows<ListItem>(value, listItems, listOfItems, onChange);
  const [added, setAdded] = React.useState<number | null>(null);
  const add = () => {
    const id = rowId();
    setAdded(id);
    update([...items, { id, value: undefined }]);
  };
  return (
    <div className="space-y-1.5 rounded-md border bg-muted/40 p-2" data-testid="compose-list">
      {items.length === 0 ? (
        <p className="text-[11px] leading-relaxed text-muted-foreground">「1 件追加」で、リストに値を追加します。</p>
      ) : null}
      {items.map((it, i) => (
        <div
          key={it.id}
          role="group"
          aria-label={`${i + 1} 件目`}
          className="space-y-1 rounded-md border bg-background p-1.5"
          data-testid="compose-item"
        >
          {renderSlot({
            label: `${label}の ${i + 1} 件目`,
            shownLabel: `${i + 1} 件目`,
            value: it.value,
            onChange: (v) => update(items.map((x) => (x.id === it.id ? { ...x, value: v } : x))),
            depth: depth + 1,
            autoFocus: added === it.id,
            actions: <RemoveButton label={`${i + 1} 件目を外す`} onClick={() => update(items.filter((x) => x.id !== it.id))} />,
          })}
          {it.value === undefined ? (
            <p className="text-[11px] leading-relaxed text-muted-foreground" data-testid="compose-row-issue">
              値を入れるまで、この 1 件は保存しません。
            </p>
          ) : null}
        </div>
      ))}
      <AddButton label="1 件追加" onClick={add} />
    </div>
  );
}
