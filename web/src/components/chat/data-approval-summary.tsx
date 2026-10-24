"use client";

/// 業務データの書込（`data_record_write`）の承認カードの中身（C P3）: 生の record_id と JSON の代わりに、
/// テーブル名・対象レコードの表示名・変える列（表示名と、更新なら今の値 → 新しい値）を見せる。
///
/// 名前・今の値は承認する人本人の権限で読む（読めない列・行は出せない＝理由を添えて JSON のまま）。表示のための
/// 読み取りで、承認の判断・実行の権限はサーバが別に検査する。

import { ArrowRight, Loader2 } from "lucide-react";
import * as React from "react";

import { getRecord, getTable, listTables, type DataRecord, type DataTable } from "@/lib/data-api";
import { displayCell, formatValue } from "@/lib/data-view";
import { cachedRoleName, resolveRoleNames } from "@/lib/role-names";
import { cachedUserName, resolveUserNames } from "@/lib/user-names";

type WriteInput = {
  table?: unknown;
  op?: unknown;
  record_id?: unknown;
  data?: unknown;
};

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

async function resolveTable(ref: string): Promise<DataTable | null> {
  if (UUID_RE.test(ref)) return getTable(ref).catch(() => null);
  const all = await listTables().catch(() => [] as DataTable[]);
  const hit = all.find((t) => t.name === ref);
  return hit ? getTable(hit.id).catch(() => hit) : null;
}

function titleOf(table: DataTable, r: DataRecord | null): string | null {
  if (!r) return null;
  const title = [...table.schema.fields].filter((f) => f.type === "text").sort((a, b) => (a.order ?? 0) - (b.order ?? 0))[0];
  const v = title ? (r.data as Record<string, unknown> | null)?.[title.key] : null;
  return typeof v === "string" && v ? v : null;
}

export function DataWriteSummary({ input }: { input: unknown }) {
  const p = (input ?? {}) as WriteInput;
  const tableRef = typeof p.table === "string" ? p.table : null;
  const op = p.op === "update" ? "update" : "create";
  const recordId = typeof p.record_id === "string" ? p.record_id : null;
  const data = p.data && typeof p.data === "object" && !Array.isArray(p.data) ? (p.data as Record<string, unknown>) : {};
  const [state, setState] = React.useState<{ loading: boolean; table: DataTable | null; record: DataRecord | null }>({
    loading: true,
    table: null,
    record: null,
  });
  React.useEffect(() => {
    let active = true;
    void (async () => {
      const table = tableRef ? await resolveTable(tableRef) : null;
      const record = table && op === "update" && recordId ? await getRecord(table.id, recordId).catch(() => null) : null;
      // ユーザー・部署の列は id ではなく表示名で見せる（引けなければ id のまま・C2 N12 と同じ）。
      if (table) {
        const values = (type: string) =>
          table.schema.fields
            .filter((f) => f.type === type)
            .flatMap((f) => [data[f.key], (record?.data as Record<string, unknown> | null)?.[f.key]])
            .flatMap((v) => (Array.isArray(v) ? v : [v]))
            .filter((v): v is string => typeof v === "string" && v !== "");
        await Promise.all([resolveUserNames(values("user_ref")), resolveRoleNames(values("role_ref"))]);
      }
      if (active) setState({ loading: false, table, record });
    })();
    return () => {
      active = false;
    };
    // data は承認の要求ごとに固定（名前を引く材料にだけ使う・毎回作り直す object なので依存に入れない）。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tableRef, op, recordId]);

  if (state.loading) {
    return (
      <p className="mb-2 flex items-center gap-1.5 text-[12px] text-muted-foreground">
        <Loader2 className="size-3.5 animate-spin" aria-hidden />
        内容を確かめています…
      </p>
    );
  }
  const { table, record } = state;
  const fieldOf = (k: string) => table?.schema.fields.find((f) => f.key === k) ?? null;
  const title = table ? titleOf(table, record) : null;
  const show = (k: string, v: unknown) => {
    const f = fieldOf(k);
    if (v === null || v === undefined || v === "") return "（空）";
    const r = { id: "", rev: 0, data: { [k]: v }, link_labels: record?.link_labels ?? {} } as unknown as DataRecord;
    return f ? displayCell(r, { key: k, field: f }, cachedUserName, cachedRoleName).text || "（空）" : formatValue(v, null, k);
  };
  return (
    <div className="mb-2 space-y-1.5 text-[13px]" data-testid="data-write-summary">
      <p className="text-foreground [overflow-wrap:anywhere]">
        {op === "create" ? (
          <>
            テーブル「<b>{table?.name ?? tableRef ?? "（不明）"}</b>」にレコードを追加します。
          </>
        ) : (
          <>
            テーブル「<b>{table?.name ?? tableRef ?? "（不明）"}</b>」の
            {title ? (
              <>
                「<b>{title}</b>」
              </>
            ) : (
              "レコード"
            )}
            を変更します。
          </>
        )}
      </p>
      {Object.keys(data).length ? (
        <dl className="divide-y rounded-lg border bg-card/60 text-[12px]">
          {Object.entries(data).map(([k, v]) => {
            const f = fieldOf(k);
            const before = record ? (record.data as Record<string, unknown> | null)?.[k] : undefined;
            return (
              <div key={k} className="flex items-start gap-2 px-2.5 py-1.5">
                <dt className="w-24 shrink-0 truncate text-muted-foreground" title={k}>
                  {f?.display_name || k}
                </dt>
                <dd className="flex min-w-0 flex-1 flex-wrap items-center gap-1 [overflow-wrap:anywhere]">
                  {op === "update" && record ? (
                    <>
                      <span className="text-muted-foreground line-through decoration-muted-foreground/50">{show(k, before)}</span>
                      <ArrowRight className="size-3 shrink-0 text-muted-foreground" aria-label="から" />
                    </>
                  ) : null}
                  <span className="font-medium text-foreground">{show(k, v)}</span>
                </dd>
              </div>
            );
          })}
        </dl>
      ) : null}
      {!table ? (
        <p className="text-[11px] text-muted-foreground">テーブルを読めなかったため、列は参照名のまま表示しています。</p>
      ) : op === "update" && !record ? (
        <p className="text-[11px] text-muted-foreground">対象のレコードを読めなかったため、今の値は表示していません。</p>
      ) : null}
    </div>
  );
}
