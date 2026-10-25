"use client";

/// スキーマエディタの列の設定（A-5）: 新しい列の追加フォーム（全型）と、既存の列の属性の編集。
///
/// - 直接入力の型（テキスト・数値・日付・日時・単一/複数選択・ユーザー・部署・ファイル）と、リンク（参照先・
///   カーディナリティ・逆向きの列）・ルックアップ・ロールアップ・数式を定義できる
/// - 必須・一意・検索・選択肢・数値の桁と範囲・説明を編集できる。可否と値の検査の正はサーバ（構造化 400 の
///   理由を出す）。ここは入力の形を整えるだけ

import { AlertTriangle, Plus, X } from "lucide-react";
import * as React from "react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { getTable, listTables, type DataTable, type FieldDef, type FieldType } from "@/lib/data-api";
import { FIELD_TYPE_LABELS, keyFromLabel, uniqueKey } from "@/lib/data-view";
import { FormulaSyntaxError, formatFormula, parseFormula } from "@/lib/formula-text";
import type { components } from "@/generated/api";
import { cn } from "@/lib/utils";

type S = components["schemas"];
type LinkCardinality = S["LinkCardinality"];
type RollupOp = S["RollupOp"];

/// 追加できる型（`computed` は旧形式なので数式を使う）。
export const ADDABLE_TYPES: FieldType[] = [
  "text",
  "number",
  "date",
  "date_time",
  "select",
  "multi_select",
  "user_ref",
  "role_ref",
  "file_ref",
  "record_ref",
  "lookup",
  "rollup",
  "formula",
];

const DERIVED: FieldType[] = ["lookup", "rollup", "formula", "computed"];
export const isDerived = (t: FieldType) => DERIVED.includes(t);

/// 一対多はこの列の側からは作れない（参照先の多対一の逆向きとして現れる）。
const CARDINALITIES: { value: LinkCardinality; label: string }[] = [
  { value: "many_to_one", label: "1 件を参照（多対一）" },
  { value: "many_to_many", label: "複数を参照（多対多）" },
  { value: "one_to_one", label: "1 件ずつ対応（一対一）" },
];
const CARDINALITY_LABEL: Record<LinkCardinality, string> = {
  many_to_one: "多対一",
  many_to_many: "多対多",
  one_to_one: "一対一",
  one_to_many: "一対多",
};

const ROLLUP_OPS: { value: RollupOp; label: string; needsTarget: boolean; numeric: boolean }[] = [
  { value: "count", label: "件数", needsTarget: false, numeric: false },
  { value: "sum", label: "合計", needsTarget: true, numeric: true },
  { value: "avg", label: "平均", needsTarget: true, numeric: true },
  { value: "min", label: "最小", needsTarget: true, numeric: true },
  { value: "max", label: "最大", needsTarget: true, numeric: true },
  { value: "concat", label: "連結", needsTarget: true, numeric: false },
];

/// テーブルの一覧（リンクの参照先の候補）と、参照先のスキーマ（ルックアップ・ロールアップの列の候補）。
function useTables(open: boolean) {
  const [tables, setTables] = React.useState<DataTable[] | null>(null);
  React.useEffect(() => {
    if (!open || tables) return;
    listTables()
      .then(setTables)
      .catch(() => setTables([]));
  }, [open, tables]);
  return tables;
}

/// 参照先のスキーマ（読み込み中・読めないときは `null`、読めなかったら `failed`）。
function useTableSchema(id: string | null | undefined): DataTable | null;
function useTableSchema(id: string | null | undefined, withState: true): { table: DataTable | null; failed: boolean };
function useTableSchema(id: string | null | undefined, withState?: true) {
  const [t, setT] = React.useState<DataTable | null>(null);
  const [failed, setFailed] = React.useState(false);
  React.useEffect(() => {
    let active = true;
    setT(null);
    setFailed(false);
    if (!id) return;
    getTable(id)
      .then((x) => active && setT(x))
      .catch(() => {
        if (!active) return;
        setT(null);
        setFailed(true);
      });
    return () => {
      active = false;
    };
  }, [id]);
  return withState ? { table: t, failed } : t;
}

function Field({ label, htmlFor, children, hint }: { label: string; htmlFor?: string; children: React.ReactNode; hint?: string }) {
  return (
    <div className="space-y-1">
      <label htmlFor={htmlFor} className="text-[11px] font-medium text-muted-foreground">
        {label}
      </label>
      {children}
      {hint ? <p className="text-[11px] text-muted-foreground">{hint}</p> : null}
    </div>
  );
}

function Toggle({
  label,
  checked,
  onChange,
  disabled,
  locked,
  hint,
}: {
  label: string;
  checked: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
  /// 変えられないが薄くしない（必ずオンになる設定で、理由の説明を読ませる）。
  locked?: boolean;
  hint?: string;
}) {
  return (
    <label className={cn("flex items-start justify-between gap-3 text-xs", disabled && !locked && "opacity-50")}>
      <span>
        {label}
        {hint ? <span className="block text-[11px] text-muted-foreground">{hint}</span> : null}
      </span>
      <Switch aria-label={label} checked={checked} disabled={disabled || locked} onCheckedChange={onChange} />
    </label>
  );
}

function SimpleSelect<T extends string>({
  value,
  onChange,
  options,
  label,
  placeholder,
  id,
}: {
  value: T | "";
  onChange: (v: T) => void;
  options: { value: T; label: string }[];
  label: string;
  placeholder?: string;
  id?: string;
}) {
  return (
    // 未選択は "" で渡す（undefined にすると非制御 → 制御に切り替わる警告になる。"" はプレースホルダを出す）。
    <Select value={value} onValueChange={(v) => onChange(v as T)}>
      <SelectTrigger id={id} aria-label={label} className="h-8 text-[13px]">
        <SelectValue placeholder={placeholder ?? "選ぶ"} />
      </SelectTrigger>
      <SelectContent>
        {options.map((o) => (
          <SelectItem key={o.value} value={o.value}>
            {o.label}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}

/// 選択肢の編集（追加・削除・並び）。既存の値を持つ選択肢を消すとサーバが拒否することがある。
export function OptionsEditor({
  options,
  onChange,
  label = "選択肢",
}: {
  options: string[];
  onChange: (o: string[]) => void;
  label?: string;
}) {
  const [draft, setDraft] = React.useState("");
  const add = () => {
    const parts = draft
      .split(/[,、\n]/)
      .map((s) => s.trim())
      .filter(Boolean)
      .filter((s) => !options.includes(s));
    if (parts.length) onChange([...options, ...new Set(parts)]);
    setDraft("");
  };
  return (
    <div className="space-y-1.5">
      <span className="text-[11px] font-medium text-muted-foreground">{label}</span>
      {options.length ? (
        <ul className="flex flex-wrap gap-1.5" aria-label={label}>
          {options.map((o) => (
            <li key={o} className="inline-flex items-center gap-1 rounded-full border bg-card py-0.5 pl-2.5 pr-1 text-xs">
              {o}
              <button
                type="button"
                aria-label={`選択肢「${o}」を外す`}
                onClick={() => onChange(options.filter((x) => x !== o))}
                className="rounded-full p-0.5 text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                <X className="size-3" aria-hidden />
              </button>
            </li>
          ))}
        </ul>
      ) : (
        <p className="text-[11px] text-muted-foreground">まだ選択肢がありません。</p>
      )}
      <div className="flex gap-1.5">
        <Input
          aria-label={`${label}を追加`}
          value={draft}
          placeholder="選択肢を追加（カンマ区切りで複数）"
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              add();
            }
          }}
          className="h-8 text-[13px]"
        />
        <Button type="button" variant="outline" size="sm" className="h-8 shrink-0" onClick={add} disabled={!draft.trim()}>
          <Plus className="size-3.5" aria-hidden />
          追加
        </Button>
      </div>
    </div>
  );
}

/// 数値の桁と範囲（numeric_spec）。空欄は宣言しない。
function NumericSpecEditor({ field, onChange }: { field: FieldDef; onChange: (f: FieldDef) => void }) {
  const spec = field.numeric_spec ?? null;
  const set = (patch: Partial<NonNullable<FieldDef["numeric_spec"]>> | null) => {
    if (patch === null) return onChange({ ...field, numeric_spec: null });
    const next = { scale: spec?.scale ?? 0, ...spec, ...patch };
    onChange({ ...field, numeric_spec: next });
  };
  const num = (v: string): number | null => (v.trim() === "" || !Number.isFinite(Number(v)) ? null : Number(v));
  return (
    <div className="grid grid-cols-3 gap-2">
      <Field label="小数の桁数" hint="0〜9。超える桁は拒否">
        <Input
          aria-label="小数の桁数"
          inputMode="numeric"
          value={spec ? String(spec.scale) : ""}
          placeholder="指定なし"
          onChange={(e) => {
            const v = e.target.value.trim();
            if (v === "") return set(null);
            const n = Math.max(0, Math.min(9, Math.trunc(Number(v)) || 0));
            set({ scale: n });
          }}
          className="h-8 text-[13px]"
        />
      </Field>
      <Field label="最小">
        <Input
          aria-label="最小"
          inputMode="decimal"
          value={spec?.min ?? ""}
          placeholder="なし"
          onChange={(e) => set({ min: num(e.target.value) })}
          className="h-8 text-[13px]"
        />
      </Field>
      <Field label="最大">
        <Input
          aria-label="最大"
          inputMode="decimal"
          value={spec?.max ?? ""}
          placeholder="なし"
          onChange={(e) => set({ max: num(e.target.value) })}
          className="h-8 text-[13px]"
        />
      </Field>
    </div>
  );
}

/// テーブルの性質のうち、列の属性の警告に使うもの（スキーマ管理者だけが知る行ポリシー・マスク）。
export type TableTraits = {
  /// 行ポリシー（行ごとの閲覧の絞り込み）がある。
  rowPolicy: boolean;
  /// フィールドマスク（一部の人に隠す列）の参照名。
  masked: ReadonlySet<string>;
};

export const NO_TRAITS: TableTraits = { rowPolicy: false, masked: new Set() };

function Warning({ children, testId }: { children: React.ReactNode; testId?: string }) {
  return (
    <p
      className="flex items-start gap-1.5 rounded-md border px-2 py-1.5 text-[11px] leading-relaxed text-foreground"
      // 「待ち・注意」の帯と同じ秋色（data-problem と揃える）。
      style={{
        borderColor: "color-mix(in oklab, var(--season-autumn) 40%, transparent)",
        backgroundColor: "color-mix(in oklab, var(--season-autumn) 8%, transparent)",
      }}
      role="note"
      data-testid={testId}
    >
      <AlertTriangle className="mt-px size-3.5 shrink-0 text-[var(--season-autumn)]" aria-hidden />
      <span>{children}</span>
    </p>
  );
}

/// 直接入力の列の属性（必須・一意・検索・選択肢・数値）。新しい列と既存の列で共通。
function BasicAttributes({
  field,
  onChange,
  isNew,
  traits = NO_TRAITS,
}: {
  field: FieldDef;
  onChange: (f: FieldDef) => void;
  isNew: boolean;
  traits?: TableTraits;
}) {
  const t = field.type;
  return (
    <div className="space-y-2.5">
      {t === "select" || t === "multi_select" ? (
        <OptionsEditor options={field.options ?? []} onChange={(options) => onChange({ ...field, options })} />
      ) : null}
      {t === "number" && isNew ? <NumericSpecEditor field={field} onChange={onChange} /> : null}
      {t === "number" && !isNew && field.numeric_spec ? (
        <p className="text-[11px] text-muted-foreground">
          小数 {field.numeric_spec.scale} 桁
          {field.numeric_spec.min != null ? `・最小 ${field.numeric_spec.min}` : ""}
          {field.numeric_spec.max != null ? `・最大 ${field.numeric_spec.max}` : ""}（保存済みの値の規則なので変えられません）
        </p>
      ) : null}
      <Toggle
        label="必須"
        hint="作成時に値が要り、空にできません"
        checked={Boolean(field.required)}
        onChange={(required) => onChange({ ...field, required })}
      />
      {field.required && traits.masked.has(field.key) ? (
        // 必須かつマスク: 読めない editor は作成・取込で値を入れられず、常に拒否される（2-B-P3-5）。
        <Warning testId="schema-warning-required-masked">
          この列は一部の人には隠しています。必須にすると、この列を読めない編集者はレコードの追加・取込ができなくなります。
        </Warning>
      ) : null}
      {t === "text" || t === "number" || t === "date" ? (
        <Toggle
          label="重複を許さない（一意）"
          hint={isNew ? undefined : "既存の値に重複があると保存できません"}
          checked={Boolean(field.unique)}
          onChange={(unique) => onChange({ ...field, unique })}
        />
      ) : null}
      {field.unique && traits.rowPolicy && (t === "text" || t === "number" || t === "date") ? (
        // 行ポリシーのあるテーブルの一意: 重複の拒否で、見えない行に同じ値があることが分かる（2-B-P2-3）。
        <Warning testId="schema-warning-unique-row-policy">
          このテーブルは行ごとに見られる人を絞っています。一意にすると、見られない行に同じ値があることが、重複の拒否から他の編集者に分かります。
        </Warning>
      ) : null}
      {t === "text" || t === "multi_select" ? (
        <Toggle
          label="部分一致で検索できる"
          hint="「含む」「で終わる」の絞り込みに使えます"
          checked={Boolean(field.searchable)}
          onChange={(searchable) => onChange({ ...field, searchable })}
        />
      ) : null}
    </div>
  );
}

/// 派生列・リンクの定義の要約（既存の列・定義の変更は列を作り直す）。
export function DefinitionSummary({ field, fields, tables }: { field: FieldDef; fields: FieldDef[]; tables: DataTable[] | null }) {
  const labelOf = (k: string) => fields.find((f) => f.key === k)?.display_name || k;
  const tableName = (id: string | null | undefined) => tables?.find((t) => t.id === id)?.name ?? "（見られないテーブル）";
  // 参照先の列は参照先テーブルの表示名で出す（日本語名の列の参照名 col_N をそのまま見せない）。
  const targetLabel = (via: string, k: string) => {
    const ref = fields.find((f) => f.key === via)?.ref_table;
    return tables?.find((t) => t.id === ref)?.schema.fields.find((f) => f.key === k)?.display_name || k;
  };
  let text: string | null = null;
  if (field.type === "record_ref") {
    text = `参照先: ${tableName(field.ref_table)}${field.link ? `・${CARDINALITY_LABEL[field.link.cardinality]}` : ""}${
      field.link?.reverse ? "・逆向きの列（読み取り専用）" : ""
    }`;
  } else if (field.type === "lookup" && field.lookup) {
    text = `「${labelOf(field.lookup.via_field)}」の先の「${targetLabel(field.lookup.via_field, field.lookup.target_field)}」`;
  } else if (field.type === "rollup" && field.rollup) {
    const op = ROLLUP_OPS.find((o) => o.value === field.rollup?.op)?.label ?? field.rollup.op;
    text = `「${labelOf(field.rollup.link)}」の先の${field.rollup.target ? `「${targetLabel(field.rollup.link, field.rollup.target)}」の` : ""}${op}`;
  } else if (field.type === "formula" && field.formula) {
    text = `= ${formatFormula(field.formula, (k) => fields.find((f) => f.key === k)?.display_name || null)}`;
  }
  if (!text) return null;
  return (
    <p className="rounded-md bg-muted/60 px-2.5 py-1.5 text-[11px] text-muted-foreground" data-testid="schema-definition">
      {text}
    </p>
  );
}

/// 既存の列の属性の編集（説明・必須・一意・検索・選択肢・数値）。定義（型・リンク・派生）は変えない。
export function FieldSettings({
  field,
  fields,
  onChange,
  tables,
  traits,
}: {
  field: FieldDef;
  fields: FieldDef[];
  onChange: (f: FieldDef) => void;
  tables: DataTable[] | null;
  traits?: TableTraits;
}) {
  return (
    <div className="space-y-2.5 border-t bg-muted/20 px-3 py-3" data-testid={`schema-settings-${field.key}`}>
      <DefinitionSummary field={field} fields={fields} tables={tables} />
      <Field label="説明（見出しのヒント）" htmlFor={`desc-${field.key}`}>
        <Input
          id={`desc-${field.key}`}
          value={field.description ?? ""}
          maxLength={500}
          placeholder="例: 税込みの金額"
          onChange={(e) => onChange({ ...field, description: e.target.value || null })}
          className="h-8 text-[13px]"
        />
      </Field>
      {isDerived(field.type) || field.type === "record_ref" ? (
        <p className="text-[11px] text-muted-foreground">
          参照先・集計・式は変えられません。変えたいときは新しい列を作ってください（この列はビューの「列」で隠せます）。
        </p>
      ) : (
        <BasicAttributes field={field} onChange={onChange} isNew={false} traits={traits} />
      )}
    </div>
  );
}

export type NewField = { label: string; def: FieldDef };

/// 一対一のリンクは参照先の側の一意をサーバが逆向きの列で強制するので、逆向きの列が要る。
const needsSymmetric = (c: LinkCardinality | undefined) => c === "one_to_one" || c === "one_to_many";

/// 新しい列の追加フォーム。`onAdd` に参照名を決めた列の定義を渡す。
export function AddFieldForm({
  fields,
  tableId,
  tableName,
  traits,
  onAdd,
  onCancel,
}: {
  fields: FieldDef[];
  tableId: string;
  /// このテーブルの名前（逆向きの列の表示名「<テーブル名>（<列名>）」に使う）。
  tableName: string;
  traits?: TableTraits;
  onAdd: (f: FieldDef) => void;
  onCancel: () => void;
}) {
  const [label, setLabel] = React.useState("");
  const [type, setType] = React.useState<FieldType>("text");
  const [def, setDef] = React.useState<FieldDef>({ key: "", type: "text" });
  const [symmetric, setSymmetric] = React.useState(false);
  const [formula, setFormula] = React.useState("");
  const tables = useTables(type === "record_ref" || type === "lookup" || type === "rollup");
  const links = fields.filter((f) => f.type === "record_ref" && !f.link?.reverse);
  const via = type === "lookup" ? def.lookup?.via_field : type === "rollup" ? def.rollup?.link : null;
  const viaField = links.find((f) => f.key === via);
  const target = useTableSchema(viaField?.ref_table);
  // リンクの参照先（逆向きの列の参照名が参照先の既存の列と重ならないよう、参照先の参照名を見る）。
  const { table: refTable, failed: refFailed } = useTableSchema(type === "record_ref" ? def.ref_table : null, true);
  const selfRef = type === "record_ref" && def.ref_table === tableId;
  const cardinality = def.link?.cardinality ?? "many_to_one";
  // 一対一は逆向きの列が必須（自己参照では作れないので選べない）。
  const symmetricOn = !selfRef && (symmetric || needsSymmetric(cardinality));
  const taken = new Set(fields.map((f) => f.key));

  const changeType = (t: FieldType) => {
    setType(t);
    setDef({ key: "", type: t, options: t === "select" || t === "multi_select" ? [] : undefined });
    setSymmetric(false);
    setFormula("");
  };

  // 数式は書くたびに組み立て、誤りはその場で出す。
  const formulaResult = React.useMemo(() => {
    if (type !== "formula" || !formula.trim()) return null;
    try {
      return {
        ok: true as const,
        expr: parseFormula(formula, (l) => fields.find((f) => (f.display_name || f.key) === l)?.key ?? null),
      };
    } catch (e) {
      return { ok: false as const, error: e instanceof FormulaSyntaxError ? e.message : String(e) };
    }
  }, [type, formula, fields]);

  const missing: string | null = (() => {
    if (!label.trim()) return "列の名前を入れてください";
    if ((type === "select" || type === "multi_select") && !(def.options ?? []).length) return "選択肢を 1 つ以上入れてください";
    if (type === "record_ref" && !def.ref_table) return "参照先のテーブルを選んでください";
    if (type === "record_ref" && selfRef && needsSymmetric(cardinality)) {
      return "同じテーブルを参照する一対一のリンクは作れません（多対一か多対多を選んでください）";
    }
    if (type === "record_ref" && symmetricOn && !refTable) {
      return refFailed ? "参照先のテーブルを読めないため、逆向きの列を作れません" : "参照先のテーブルを読み込んでいます…";
    }
    if (type === "lookup" && (!def.lookup?.via_field || !def.lookup?.target_field)) return "リンクの列と、参照先の列を選んでください";
    if (type === "rollup") {
      const op = ROLLUP_OPS.find((o) => o.value === def.rollup?.op);
      if (!def.rollup?.link || !op) return "リンクの列と集計の方法を選んでください";
      if (op.needsTarget && !def.rollup.target) return "集計する参照先の列を選んでください";
    }
    if (type === "formula" && !formulaResult?.ok) return formulaResult ? formulaResult.error : "式を入れてください";
    return null;
  })();

  const add = () => {
    if (missing) return;
    const key = keyFromLabel(label, taken);
    const out: FieldDef = { ...def, key, display_name: label.trim(), type };
    if (type === "record_ref") {
      // 逆向きの列の参照名は、この列の参照名から作り、参照先の既存の参照名と重ねない（日本語名の列で
      // 常に `rev` になり 2 本目が作れなかった・2-A-P2-1）。表示名「<テーブル名>（<列名>）」は作成時に
      // サーバへ渡す（`symmetric_display_name`・保存の後に参照先のスキーマを書き換えない）。
      const symKey = symmetricOn
        ? uniqueKey(`${key}_rev`, new Set((refTable?.schema.fields ?? []).map((f) => f.key)))
        : null;
      out.link = {
        cardinality,
        ...(symKey ? { symmetric_key: symKey, symmetric_display_name: `${tableName}（${label.trim()}）` } : {}),
      };
    }
    if (type === "formula" && formulaResult?.ok) out.formula = formulaResult.expr;
    onAdd(out);
  };

  const targetFields = (target?.schema.fields ?? []).filter((f) => !f.link?.reverse);
  const rollupOp = ROLLUP_OPS.find((o) => o.value === def.rollup?.op);

  return (
    <div className="space-y-3 rounded-lg border border-primary/30 bg-primary/[0.03] p-3" data-testid="schema-add-form">
      <div className="grid grid-cols-[1fr_9rem] gap-2">
        <Input
          autoFocus
          aria-label="新しい列の表示名"
          placeholder="列の名前（例: 金額）"
          value={label}
          maxLength={100}
          onChange={(e) => setLabel(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !missing) {
              e.preventDefault();
              add();
            }
          }}
          className="h-8 text-[13px]"
        />
        <Select value={type} onValueChange={(t) => changeType(t as FieldType)}>
          <SelectTrigger aria-label="新しい列の型" className="h-8 text-[13px]">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {ADDABLE_TYPES.map((t) => (
              <SelectItem key={t} value={t}>
                {FIELD_TYPE_LABELS[t]}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      {type === "record_ref" ? (
        <div className="space-y-2.5">
          <Field label="参照先のテーブル">
            <SimpleSelect
              label="参照先のテーブル"
              value={def.ref_table ?? ""}
              onChange={(ref_table) => setDef({ ...def, ref_table })}
              placeholder={tables ? "テーブルを選ぶ" : "読み込み中…"}
              options={(tables ?? []).map((t) => ({ value: t.id, label: t.id === tableId ? `${t.name}（このテーブル）` : t.name }))}
            />
          </Field>
          <Field label="つながり方">
            <SimpleSelect
              label="つながり方"
              value={cardinality}
              onChange={(c) => setDef({ ...def, link: { ...def.link, cardinality: c } })}
              options={CARDINALITIES}
            />
          </Field>
          <Toggle
            label="参照先にも逆向きの列を作る"
            hint={
              selfRef
                ? "同じテーブルを参照するリンクには逆向きの列を作れません"
                : needsSymmetric(cardinality)
                  ? "一対一のリンクは、参照先の行が重ならないよう逆向きの列で確かめるため必ず作ります（参照先の管理者である必要があります）"
                  : `参照先のテーブルから、こちらの行を一覧できます。列名は「${tableName}（${label.trim() || "この列"}）」になります（参照先の管理者である必要があります）`
            }
            checked={symmetricOn}
            disabled={selfRef}
            locked={!selfRef && needsSymmetric(cardinality)}
            onChange={setSymmetric}
          />
        </div>
      ) : null}

      {type === "lookup" || type === "rollup" ? (
        links.length === 0 ? (
          <p className="text-[11px] text-muted-foreground">先にリンクの列を作ってください（参照先の値を引く列です）。</p>
        ) : (
          <div className="space-y-2.5">
            <Field label="リンクの列">
              <SimpleSelect
                label="リンクの列"
                value={via ?? ""}
                onChange={(k) =>
                  setDef(
                    type === "lookup"
                      ? { ...def, lookup: { via_field: k, target_field: "" } }
                      : { ...def, rollup: { link: k, op: def.rollup?.op ?? "count", target: null } },
                  )
                }
                options={links.map((f) => ({ value: f.key, label: f.display_name || f.key }))}
              />
            </Field>
            {type === "rollup" ? (
              <Field label="集計の方法">
                <SimpleSelect
                  label="集計の方法"
                  value={def.rollup?.op ?? "count"}
                  onChange={(op) => setDef({ ...def, rollup: { link: def.rollup?.link ?? "", op, target: null } })}
                  options={ROLLUP_OPS.map((o) => ({ value: o.value, label: o.label }))}
                />
              </Field>
            ) : null}
            {type === "lookup" || rollupOp?.needsTarget ? (
              <Field label="参照先の列">
                <SimpleSelect
                  label="参照先の列"
                  value={(type === "lookup" ? def.lookup?.target_field : def.rollup?.target) ?? ""}
                  placeholder={via ? (target ? "列を選ぶ" : "読み込み中…") : "先にリンクの列を選ぶ"}
                  onChange={(k) =>
                    setDef(
                      type === "lookup"
                        ? { ...def, lookup: { via_field: def.lookup?.via_field ?? "", target_field: k } }
                        : { ...def, rollup: { link: def.rollup?.link ?? "", op: def.rollup?.op ?? "sum", target: k } },
                    )
                  }
                  options={targetFields
                    .filter((f) => !(type === "rollup" && rollupOp?.numeric) || f.type === "number" || f.result_type === "number")
                    .map((f) => ({ value: f.key, label: f.display_name || f.key }))}
                />
              </Field>
            ) : null}
          </div>
        )
      ) : null}

      {type === "formula" ? (
        <Field
          label="式"
          htmlFor="schema-formula"
          hint="列は「表示名」か参照名で書きます。例: 「単価」 * 「数量」、round(「金額」 * 1.1, 0)、if(「数量」 >= 10, &quot;大口&quot;, &quot;通常&quot;)"
        >
          <Input
            id="schema-formula"
            aria-label="数式の式"
            aria-invalid={formulaResult && !formulaResult.ok ? true : undefined}
            value={formula}
            onChange={(e) => setFormula(e.target.value)}
            placeholder="「単価」 * 「数量」"
            className={cn("h-8 font-mono text-[13px]", formulaResult && !formulaResult.ok && "border-destructive")}
          />
        </Field>
      ) : null}

      {!isDerived(type) && type !== "record_ref" ? (
        <BasicAttributes field={{ ...def, type }} onChange={setDef} isNew traits={traits} />
      ) : null}

      {missing && label.trim() ? (
        <p
          className={cn("text-[11px]", formulaResult && !formulaResult.ok ? "text-destructive" : "text-muted-foreground")}
          role="status"
        >
          {missing}
        </p>
      ) : null}
      <div className="flex justify-end gap-2">
        <Button variant="ghost" size="sm" className="h-8 text-xs" onClick={onCancel} data-testid="schema-add-cancel">
          やめる
        </Button>
        <Button size="sm" className="h-8 text-xs" disabled={missing !== null} onClick={add} data-testid="schema-add-confirm">
          追加
        </Button>
      </div>
    </div>
  );
}
