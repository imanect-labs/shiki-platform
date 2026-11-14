"use client";

/// テーブルを作る・直す道具（`data_table_create` / `data_table_update`・D-10 の T05）の承認カードの中身。
///
/// 生の JSON の代わりに、作るテーブルの列（表示名・型・必須・選択肢）・見られる人の決まり・列の見せ方・状態の進め方
/// （状態と、だれがどこへ進められるか）を読める形で出す。直すときは、承認する人本人の権限で今のテーブルを読み、
/// 足す列・変える列（今 → 新しい）と入れ替える決まりを出す。表示のための読み取りで、実行の権限はサーバが別に検査する
/// （テーブルを直せるのは持ち主だけ）。

import { ArrowRight, Loader2 } from "lucide-react";
import * as React from "react";

import { getTable, type DataTable, type FieldDef } from "@/lib/data-api";
import { FIELD_TYPE_LABELS } from "@/lib/data-view";
import { cachedRoleName, resolveRoleNames } from "@/lib/role-names";

type Obj = Record<string, unknown>;
type FsmInput = { status_field?: unknown; states?: unknown; transitions?: unknown; applicant_write_lock?: unknown };
type FieldPolicyInput = { field?: unknown; readable_by?: unknown };

const isObj = (v: unknown): v is Obj => Boolean(v) && typeof v === "object" && !Array.isArray(v);
const asStr = (v: unknown): string | null => (typeof v === "string" && v.trim() ? v : null);
const strs = (v: unknown): string[] => (Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : []);

/// 入力の列（オブジェクトだけ・参照名のあるもの）。
function fieldsOf(v: unknown): FieldDef[] {
  return Array.isArray(v) ? (v.filter((f) => isObj(f) && typeof f.key === "string") as FieldDef[]) : [];
}

const labelOf = (key: string, fields: FieldDef[]) => {
  const f = fields.find((x) => x.key === key);
  return f?.display_name || key;
};

/// 述語を読める文に（`who` は進められる人の文脈か、見られる人の文脈か）。読めない形は null。
export function describePolicy(e: unknown, fields: FieldDef[], who: "read" | "actor"): string | null {
  if (e === "public") return who === "read" ? "テーブルを見られる人全員" : "テーブルを編集できる人";
  if (e === "is_owner") return "レコードを追加した人";
  if (!isObj(e)) return null;
  if (isObj(e.has_role)) {
    const role = asStr(e.has_role.role) ?? "";
    const name = cachedRoleName(role) || role;
    return `部署・グループ「${name}」${e.has_role.subtree ? "（配下を含む）" : ""}の人`;
  }
  if (isObj(e.field_cmp)) {
    const c = e.field_cmp;
    const col = `「${labelOf(String(c.field ?? ""), fields)}」`;
    if (c.value === "user_id") return c.op === "ne" ? `${col}が自分でない人` : `${col}が自分の人`;
    if (isObj(c.value) && "user_roles" in c.value) return `${col}の部署・グループの人`;
    if (isObj(c.value) && "lit" in c.value) {
      const lit = c.value.lit;
      const vals = Array.isArray(lit) ? lit.map(String).join("・") : String(lit);
      return c.op === "ne" ? `${col}が「${vals}」でないとき` : `${col}が「${vals}」のとき`;
    }
    return null;
  }
  for (const [k, sep] of [
    ["any", "、または"],
    ["all", "、かつ"],
  ] as const) {
    const xs = e[k];
    if (Array.isArray(xs)) {
      const parts = xs.map((x) => describePolicy(x, fields, who));
      return parts.length && parts.every((p) => p) ? parts.join(sep) : null;
    }
  }
  return null;
}

/// 述語に出てくる部署・グループの id（表示名を引く）。
function rolesIn(e: unknown, out: string[] = []): string[] {
  if (Array.isArray(e)) e.forEach((x) => rolesIn(x, out));
  else if (isObj(e)) {
    if (isObj(e.has_role) && typeof e.has_role.role === "string") out.push(e.has_role.role);
    Object.values(e).forEach((x) => rolesIn(x, out));
  }
  return out;
}

function PolicyText({ expr, fields, who }: { expr: unknown; fields: FieldDef[]; who: "read" | "actor" }) {
  const text = describePolicy(expr, fields, who);
  return text ? <>{text}</> : <span className="text-muted-foreground">細かい条件（送る内容の JSON で確かめてください）</span>;
}

function fieldTraits(f: FieldDef): string {
  const t = [FIELD_TYPE_LABELS[f.type] ?? f.type];
  if (f.required) t.push("必須");
  if (f.unique) t.push("重複不可");
  return t.join("・");
}

function FieldRow({ f, before, tag }: { f: FieldDef; before?: FieldDef; tag?: string }) {
  const opts = strs(f.options);
  const prevOpts = before ? strs(before.options) : [];
  const added = opts.filter((o) => !prevOpts.includes(o));
  return (
    <div className="flex items-start gap-2 px-2.5 py-1.5">
      <dt className="w-28 shrink-0 text-foreground" title={f.key}>
        <span className="block truncate">{f.display_name || before?.display_name || f.key}</span>
        {tag ? <span className="block text-[10.5px] text-muted-foreground">{tag}</span> : null}
      </dt>
      <dd className="min-w-0 flex-1 space-y-0.5 text-muted-foreground [overflow-wrap:anywhere]">
        <span className="flex flex-wrap items-center gap-1">
          {before && fieldTraits(before) !== fieldTraits(f) ? (
            <>
              <span className="line-through decoration-muted-foreground/50">{fieldTraits(before)}</span>
              <ArrowRight className="size-3 shrink-0" aria-label="から" />
            </>
          ) : null}
          <span className="text-foreground">{fieldTraits(f)}</span>
        </span>
        {opts.length ? (
          <span className="flex flex-wrap gap-1">
            {opts.map((o) => (
              <span
                key={o}
                className={
                  before && added.includes(o)
                    ? "rounded bg-emerald-500/15 px-1.5 text-[11px] text-emerald-700 dark:text-emerald-300"
                    : "rounded bg-muted px-1.5 text-[11px] text-foreground/80"
                }
              >
                {before && added.includes(o) ? `＋${o}` : o}
              </span>
            ))}
          </span>
        ) : null}
      </dd>
    </div>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="space-y-1">
      <h4 className="text-[11px] font-medium text-muted-foreground">{title}</h4>
      {children}
    </section>
  );
}

function FsmView({ fsm, fields }: { fsm: FsmInput; fields: FieldDef[] }) {
  const states = strs(fsm.states);
  const moves = Array.isArray(fsm.transitions) ? fsm.transitions.filter(isObj) : [];
  const status = asStr(fsm.status_field);
  const lock = asStr(fsm.applicant_write_lock);
  return (
    <Section title={`状態の進め方${status ? `（列「${labelOf(status, fields)}」）` : ""}`}>
      <p className="flex flex-wrap items-center gap-1 text-[12px]">
        {states.map((s, i) => (
          <span key={s} className="rounded bg-muted px-1.5 text-foreground/80">
            {s}
            {i === 0 ? <span className="ml-1 text-[10.5px] text-muted-foreground">最初</span> : null}
          </span>
        ))}
      </p>
      <ul className="space-y-0.5 text-[12px]">
        {moves.map((m, i) => (
          <li key={i} className="flex flex-wrap items-center gap-1 [overflow-wrap:anywhere]">
            <span className="text-foreground">{String(m.from ?? "")}</span>
            <ArrowRight className="size-3 shrink-0 text-muted-foreground" aria-label="から" />
            <span className="text-foreground">{String(m.to ?? "")}</span>
            <span className="text-muted-foreground">
              ：<PolicyText expr={m.actor} fields={fields} who="actor" />
            </span>
          </li>
        ))}
      </ul>
      {lock ? (
        <p className="text-[11px] text-muted-foreground">
          最初の状態を過ぎたら、「{labelOf(lock, fields)}」の人（申請者）はレコードを書き換えられません。
        </p>
      ) : null}
    </Section>
  );
}

function PoliciesView({ input, fields, replacing }: { input: Obj; fields: FieldDef[]; replacing: boolean }) {
  const rp = isObj(input.row_policy) ? input.row_policy : null;
  const fp = Array.isArray(input.field_policy) ? (input.field_policy.filter(isObj) as FieldPolicyInput[]) : null;
  return (
    <>
      {rp ? (
        <Section title={replacing ? "見られる人の決まり（入れ替え）" : "見られる人の決まり"}>
          <p className="text-[12px] text-foreground [overflow-wrap:anywhere]">
            見られる: <PolicyText expr={rp.read} fields={fields} who="read" />
          </p>
          {rp.write !== undefined && rp.write !== null ? (
            <p className="text-[12px] text-foreground [overflow-wrap:anywhere]">
              書き換え・削除できる: <PolicyText expr={rp.write} fields={fields} who="read" />
            </p>
          ) : null}
        </Section>
      ) : !replacing ? (
        <Section title="見られる人の決まり">
          <p className="text-[12px] text-foreground">決まりなし（テーブルを見られる人は全部の行を見られます）</p>
        </Section>
      ) : null}
      {fp && fp.length ? (
        <Section title={replacing ? "列の見せ方（入れ替え）" : "列の見せ方"}>
          <ul className="space-y-0.5 text-[12px]">
            {fp.map((p, i) => (
              <li key={i} className="[overflow-wrap:anywhere]">
                「{labelOf(String(p.field ?? ""), fields)}」を見られるのは <PolicyText expr={p.readable_by} fields={fields} who="read" />
              </li>
            ))}
          </ul>
        </Section>
      ) : null}
    </>
  );
}

function useRoleNames(input: unknown) {
  const [, rerender] = React.useReducer((n: number) => n + 1, 0);
  const key = JSON.stringify(rolesIn(input));
  React.useEffect(() => {
    const roles = JSON.parse(key) as string[];
    if (!roles.length) return;
    let active = true;
    void resolveRoleNames(roles).then(() => active && rerender());
    return () => {
      active = false;
    };
  }, [key]);
}

const OWNER_NOTE = "作ったテーブルの持ち主はあなただけです。ほかの人には共有されません（共有はテーブルの画面で行います）。";

/// テーブルを作る（`data_table_create`）承認カードの中身。入力だけで描ける（読み込みは部署の表示名だけ）。
export function TableCreateSummary({ input }: { input: unknown }) {
  const p = isObj(input) ? input : {};
  const fields = fieldsOf(p.fields);
  useRoleNames(p);
  const fsm = isObj(p.fsm) ? (p.fsm as FsmInput) : null;
  return (
    <div className="mb-2 space-y-2 text-[13px]" data-testid="table-create-summary">
      <p className="text-foreground [overflow-wrap:anywhere]">
        テーブル「<b>{asStr(p.name) ?? "（名前なし）"}</b>」を作ります。
      </p>
      <Section title={`列（${fields.length}）`}>
        <dl className="divide-y rounded-lg border bg-card/60 text-[12px]">
          {fields.map((f) => (
            <FieldRow key={f.key} f={fsm && f.key === fsm.status_field ? { ...f, options: strs(fsm.states) } : f} />
          ))}
        </dl>
      </Section>
      <PoliciesView input={p} fields={fields} replacing={false} />
      {fsm ? <FsmView fsm={fsm} fields={fields} /> : null}
      <p className="text-[11px] text-muted-foreground">{OWNER_NOTE}</p>
    </div>
  );
}

type Loaded = { loading: true } | { loading: false; table: DataTable | null };

/// テーブルを直す（`data_table_update`）承認カードの中身。今のテーブルを承認する人の権限で読み、差を出す。
export function TableUpdateSummary({ input }: { input: unknown }) {
  const p = isObj(input) ? input : {};
  const tableId = asStr(p.table_id);
  const [state, setState] = React.useState<Loaded>({ loading: true });
  useRoleNames(p);
  React.useEffect(() => {
    let active = true;
    void (async () => {
      const table = tableId ? await getTable(tableId).catch(() => null) : null;
      if (active) setState({ loading: false, table });
    })();
    return () => {
      active = false;
    };
  }, [tableId]);
  if (state.loading) {
    return (
      <p className="mb-2 flex items-center gap-1.5 text-[12px] text-muted-foreground" aria-busy="true">
        <Loader2 className="size-3.5 animate-spin" aria-hidden />
        今のテーブルを読み込んでいます…
      </p>
    );
  }
  const table = state.table;
  const current = table?.schema.fields ?? [];
  const written = fieldsOf(p.fields);
  const all = [...current, ...written.filter((f) => !current.some((c) => c.key === f.key))];
  const fsm = isObj(p.fsm) ? (p.fsm as FsmInput) : null;
  const version = typeof p.expected_schema_version === "number" ? p.expected_schema_version : null;
  return (
    <div className="mb-2 space-y-2 text-[13px]" data-testid="table-update-summary">
      <p className="text-foreground [overflow-wrap:anywhere]">
        {table ? (
          <>
            テーブル「<b>{table.name}</b>」を直します。
          </>
        ) : (
          "テーブルを直します。"
        )}
      </p>
      {!table ? (
        <p className="text-[11px] text-muted-foreground">
          テーブルを読めなかったため、今の定義との差は表示していません（テーブルを直せるのは持ち主だけです）。
        </p>
      ) : version !== null && version !== table.schema_version ? (
        <p className="text-[11px] text-amber-700 dark:text-amber-300">
          AI が読んだ後にテーブルが変わっています（今は v{table.schema_version}）。承認しても保存されず、読み直しになります。
        </p>
      ) : null}
      {written.length ? (
        <Section title="列">
          <dl className="divide-y rounded-lg border bg-card/60 text-[12px]">
            {written.map((w) => {
              const before = current.find((c) => c.key === w.key);
              const merged = before ? ({ ...before, ...w } as FieldDef) : w;
              return <FieldRow key={w.key} f={merged} before={before} tag={before ? "変える" : table ? "足す" : undefined} />;
            })}
          </dl>
        </Section>
      ) : null}
      <PoliciesView input={p} fields={all} replacing />
      {fsm ? <FsmView fsm={fsm} fields={all} /> : null}
    </div>
  );
}
