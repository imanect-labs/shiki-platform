"use client";

/// 制御ノードのフォーム（条件分岐・振り分け・繰り返し・待機・合流）。
///
/// 条件は「比較の並び（すべて/いずれか）」の 1 段編集を基本にし、深いネストは
/// 保存済み JSON をそのまま維持する（高度な条件は AI 編集/JSON で作る想定）。

import * as React from "react";
import { Plus, Trash2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { EVENT_CATALOG, NODE_CATALOG, type NodeParamsByType } from "@/generated/workflow-catalog";
import type {
  CmpOp,
  Comparison,
  Condition,
  ValueExpr,
} from "@/generated/workflow-ir";
import { Field, FieldInput, FieldSelectTrigger, NumberInput } from "./common";
import { paramsOf, patchParams } from "./params";
import { InputFieldsContext, numericLiteral, ValueExprInput, type NumericMode } from "./value-expr-input";
import { isFromRef } from "./value-refs";
import type { FormProps } from "./forms-basic";
import { LIST_SHAPE } from "./value-shape";
import type { InputFieldInfo } from "@/lib/workflow-input-schema";

const OP_LABELS: [CmpOp, string][] = [
  ["eq", "と等しい"],
  ["neq", "と等しくない"],
  ["gt", "より大きい"],
  ["gte", "以上"],
  ["lt", "より小さい"],
  ["lte", "以下"],
  ["contains", "を含む"],
  ["starts_with", "で始まる"],
  ["ends_with", "で終わる"],
  ["in", "のいずれか"],
  ["exists", "が存在する"],
  ["is_null", "が空である"],
  ["matches", "がパターンに一致"],
];

const NO_RIGHT: CmpOp[] = ["exists", "is_null"];

/// 右辺の数字を数として保存する比較（等値と大小比較）。文字の判定（含む・始まる・終わる・パターン）は
/// 文字のまま、`in` は候補のリストを JSON で書く。
const NUMERIC_OPS: CmpOp[] = ["eq", "neq", "gt", "gte", "lt", "lte"];

/// 比べる値（右辺）の数字を数として保存するか（BUG-31）。固定値の数字を文字の「50000」で保存すると、
/// 数との大小比較は run の失敗（`expr_type_error`）に、等値は黙って偽になる。
/// - 左辺が数値の入力項目なら、数として読めるものは数（`always`・桁区切りも外す）
/// - 左辺が文字・選択肢・日付の入力項目なら、文字のまま（数字のコード「001」などを数にしない）
/// - それ以外（前のブロックの結果・きっかけの内容など）は、数字だけを打ったときに数。頭に 0 を重ねた数字を
///   文字のまま残すのは等値だけ（`auto`）で、大小比較では数にする（`order`）
function numericModeFor(cmp: Comparison, inputFields: InputFieldInfo[]): NumericMode | undefined {
  if (!NUMERIC_OPS.includes(cmp.op)) return undefined;
  const left = cmp.left as unknown;
  if (isFromRef(left) && left.$from === "input" && typeof left.path === "string" && /^\/[^/]+$/.test(left.path)) {
    const key = left.path.slice(1).replace(/~1/g, "/").replace(/~0/g, "~");
    const field = inputFields.find((f) => f.key === key);
    if (field?.kind === "number" || field?.kind === "integer") return "always";
    if (field && field.kind !== "other" && field.kind !== "boolean") return undefined;
  }
  return cmp.op === "eq" || cmp.op === "neq" ? "auto" : "order";
}

type FlatCondition = { mode: "all" | "any"; comparisons: Comparison[] } | null;

/// 1 段の all/any＋比較列に平坦化できる条件だけ編集対象にする（それ以外は不透明維持）。
function flatten(cond: Condition | undefined): FlatCondition {
  if (!cond || typeof cond !== "object") return { mode: "all", comparisons: [] };
  if ("cmp" in cond) return { mode: "all", comparisons: [cond.cmp as Comparison] };
  if ("all" in cond || "any" in cond) {
    const mode = "all" in cond ? "all" : "any";
    const list = (cond as { all?: Condition[]; any?: Condition[] })[mode] ?? [];
    const comparisons: Comparison[] = [];
    for (const c of list) {
      if (c && typeof c === "object" && "cmp" in c) {
        comparisons.push((c as { cmp: Comparison }).cmp);
      } else {
        return null; // ネスト条件は簡易エディタ対象外。
      }
    }
    return { mode, comparisons };
  }
  return null;
}

function build(flat: NonNullable<FlatCondition>): Condition {
  const cmps = flat.comparisons.map((cmp) => ({ cmp }) as Condition);
  if (cmps.length === 1) return cmps[0];
  return (flat.mode === "all" ? { all: cmps } : { any: cmps }) as Condition;
}

/// switch case のリテラルをテキストから型付きで解釈する（完全一致比較のため型が正）。
/// 数値・true/false・JSON（[ / { / " 始まり）は解釈し、それ以外は文字列として扱う。
///
/// 数にするのは、打った文字が数の書き方そのもの（数に直して書き戻すと同じ文字）のときだけ。頭に 0 のある番号
/// （001・郵便番号・社員番号）、1.0、桁が落ちる長い数字は文字のまま比べる（BUG-100。001 を 1 にすると
/// テキストの「001」と一致しない）。前後に空白のある値も整えずに文字のまま（そのまま比べる）。
function parseCaseLiteral(text: string): unknown {
  if (/^-?\d+(\.\d+)?$/.test(text) && String(Number(text)) === text) return Number(text);
  if (text === "true") return true;
  if (text === "false") return false;
  if (/^[\[{"]/.test(text)) {
    try {
      return JSON.parse(text);
    } catch {
      // 入力途中は文字列のまま。
    }
  }
  return text;
}

export function ConditionEditor({
  value,
  onChange,
  refCandidates,
  inMapRegion,
}: {
  value: Condition | undefined;
  onChange: (next: Condition) => void;
} & Pick<FormProps, "refCandidates" | "inMapRegion">) {
  const inputFields = React.useContext(InputFieldsContext);
  const flat = flatten(value);
  if (flat === null) {
    return (
      <p className="rounded-md border bg-muted/40 p-2 text-[11px] text-muted-foreground">
        入れ子になった高度な条件が設定されています（AI 編集で変更できます）。ここで編集すると単純な条件に置き換わります。
        <Button
          variant="outline"
          size="sm"
          className="mt-2 h-7 w-full text-xs"
          onClick={() => onChange(build({ mode: "all", comparisons: [] }))}
        >
          単純な条件で作り直す
        </Button>
      </p>
    );
  }
  const update = (next: NonNullable<FlatCondition>) => onChange(build(next));
  return (
    <div className="space-y-2">
      {flat.comparisons.length > 1 ? (
        <Select
          value={flat.mode}
          onValueChange={(v) => update({ ...flat, mode: v as "all" | "any" })}
        >
          <SelectTrigger className="h-8" aria-label="条件の組み合わせ方">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">すべて満たすとき</SelectItem>
            <SelectItem value="any">どれか 1 つ満たすとき</SelectItem>
          </SelectContent>
        </Select>
      ) : null}
      {flat.comparisons.map((cmp, i) => (
        <div key={i} className="space-y-1.5 rounded-md border p-2">
          <ValueExprInput
            label="調べる値"
            preferFrom
            value={cmp.left as ValueExpr | undefined}
            onChange={(v) => {
              const comparisons = [...flat.comparisons];
              comparisons[i] = { ...cmp, left: v };
              update({ ...flat, comparisons });
            }}
            refCandidates={refCandidates}
            inMapRegion={inMapRegion}
          />
          <div className="flex items-center gap-1.5">
            <Select
              value={cmp.op}
              onValueChange={(v) => {
                const comparisons = [...flat.comparisons];
                const next = { ...cmp, op: v as CmpOp };
                // 文字の判定から大小比較へ変えたときも、打ってあった数字を数に直す。
                const mode = numericModeFor(next, inputFields);
                next.right = NO_RIGHT.includes(next.op)
                  ? undefined
                  : mode && typeof cmp.right === "string"
                    ? (numericLiteral(cmp.right, mode) as unknown as ValueExpr)
                    : cmp.right;
                comparisons[i] = next;
                update({ ...flat, comparisons });
              }}
            >
              <SelectTrigger className="h-8 flex-1" aria-label="比べ方">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {OP_LABELS.map(([op, label]) => (
                  <SelectItem key={op} value={op}>
                    {label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Button
              variant="ghost"
              size="icon"
              className="size-8"
              aria-label="条件を削除"
              onClick={() => {
                const comparisons = flat.comparisons.filter((_, j) => j !== i);
                update({ ...flat, comparisons });
              }}
            >
              <Trash2 className="size-3.5" aria-hidden />
            </Button>
          </div>
          {!NO_RIGHT.includes(cmp.op) ? (
            <ValueExprInput
              label="比べる値"
              numeric={numericModeFor(cmp, inputFields)}
              value={cmp.right as ValueExpr | undefined}
              onChange={(v) => {
                const comparisons = [...flat.comparisons];
                comparisons[i] = { ...cmp, right: v };
                update({ ...flat, comparisons });
              }}
              refCandidates={refCandidates}
              inMapRegion={inMapRegion}
            />
          ) : null}
        </div>
      ))}
      <Button
        variant="outline"
        size="sm"
        className="h-7 w-full text-xs"
        onClick={() =>
          update({
            ...flat,
            comparisons: [
              ...flat.comparisons,
              {
                // 調べる値は選ばせる（入力全体を黙って入れない・空のまま保存はできるが、実行と自動実行のオンの
                // 前に止まる・D-83）。
                left: "" as unknown as ValueExpr,
                op: "eq",
                right: "" as unknown as ValueExpr,
              } as Comparison,
            ],
          })
        }
      >
        <Plus className="size-3.5" aria-hidden />
        条件を追加
      </Button>
    </div>
  );
}

export function BranchForm({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
  const p = paramsOf<NodeParamsByType["control.branch"]>(node);
  return (
    <Field label="分ける条件" hint="満たすと「はい」・満たさないと「いいえ」へ" error={fieldErrors.get("condition")}>
      <ConditionEditor
        value={p.condition as Condition | undefined}
        onChange={(condition) => patchParams(dispatch, node, { condition })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
      />
    </Field>
  );
}

export function SwitchForm({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
  const p = paramsOf<NodeParamsByType["control.switch"]>(node);
  const cases = (p.cases ?? []) as { port: string; equals: unknown }[];
  return (
    <div className="space-y-3">
      <ValueExprInput
        label="振り分ける値"
        preferFrom
        value={p.value as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { value: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        error={fieldErrors.get("value")}
        hint="この値と同じ値の行き先へ進みます。値がないと、このブロックは失敗します。"
      />
      <Field
        label="行き先"
        hint="上から順に比べて、最初に一致した行き先へ進みます。どれにも一致しないときは「その他」へ進みます。"
        error={fieldErrors.get("cases")}
      >
        <div className="space-y-1.5">
          {cases.map((c, i) => (
            <div key={i} className="flex items-center gap-1.5">
              <Input
                value={
                  typeof c.equals === "string" ? c.equals : JSON.stringify(c.equals)
                }
                onChange={(e) => {
                  const next = [...cases];
                  // switch はリテラルの完全一致で比較するため、数値/真偽/JSON は
                  // 型を保持して保存する（"1" と 1 は一致しない）。
                  next[i] = { ...c, equals: parseCaseLiteral(e.target.value) };
                  patchParams(dispatch, node, { cases: next });
                }}
                placeholder="この値のとき"
                className="h-8 flex-1"
                aria-label="一致する値"
              />
              <span className="text-xs text-muted-foreground">→</span>
              <Input
                value={c.port}
                onChange={(e) => {
                  const next = [...cases];
                  next[i] = { ...c, port: e.target.value };
                  // 配線済みエッジの from_port も同時に付け替える（古い出口名の
                  // エッジが残ると実行時にその行き先へ流れなくなる）。
                  dispatch({
                    type: "rename_out_port",
                    id: node.id,
                    fromPort: c.port,
                    toPort: e.target.value,
                    params: { ...(node.params as Record<string, unknown>), cases: next },
                  });
                }}
                placeholder="出口名"
                className="h-8 w-24 font-mono text-xs max-md:text-base"
                aria-label="出口名"
              />
              <Button
                variant="ghost"
                size="icon"
                className="size-8"
                aria-label="行き先を削除"
                onClick={() =>
                  patchParams(dispatch, node, {
                    cases: cases.filter((_, j) => j !== i),
                  })
                }
              >
                <Trash2 className="size-3.5" aria-hidden />
              </Button>
            </div>
          ))}
          <Button
            variant="outline"
            size="sm"
            className="h-7 w-full text-xs"
            onClick={() =>
              patchParams(dispatch, node, {
                cases: [...cases, { port: `case_${cases.length + 1}`, equals: "" }],
              })
            }
          >
            <Plus className="size-3.5" aria-hidden />
            行き先を追加
          </Button>
        </div>
      </Field>
    </div>
  );
}

/// きっかけ「テーブルの行の状態が変わったとき」の名前（イベントのカタログの単一定義）。
const TRANSITION_TRIGGER = EVENT_CATALOG.find((e) => e.source === "data.transition")?.label_ja ?? "";

/// 失敗した件があるときの扱い（付録 D の N3.1.4）。
const MAP_ITEM_ERROR: Record<string, { label: string; note: string }> = {
  fail_map: {
    label: "繰り返しを失敗にする（おすすめ）",
    note: "ほかの件は最後まで動きます。そのあと「失敗したとき」の設定に従います。",
  },
  collect: {
    label: "失敗を記録して続ける",
    note: "失敗した件の番号と内容を結果に残し、次へ進みます。実行は「完了」になります。",
  },
};

/// AI を呼ぶブロックか（カタログの分類が AI のもの・利用量がかかる）。
const isAiType = (type: string) => NODE_CATALOG.find((c) => c.type === type)?.category === "ai";

export function MapForm({ node, dispatch, refCandidates, inMapRegion, fieldErrors, insideTypes }: FormProps) {
  const p = paramsOf<NodeParamsByType["control.map"]>(node);
  const onItemError = (p.on_item_error as string | undefined) ?? "fail_map";
  // 中に AI のブロックがあると、件数の分だけ AI を呼ぶ（付録 D の N3.1.4・P8）。
  const aiInside = (insideTypes ?? []).some(isAiType);
  return (
    <div className="space-y-3">
      <ValueExprInput
        label="繰り返すリスト"
        preferFrom
        shapes={LIST_SHAPE}
        value={p.items as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { items: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        hint={`このリストの 1 件ずつに、中のブロックを実行します。最大 1,000 件で、空なら何もせずに次へ進みます。${
          aiInside ? "中に AI のブロックがあると、1 件ごとに AI を呼ぶので、件数の分だけ利用量がかかります。" : ""
        }`}
        error={fieldErrors.get("items")}
      />
      <Field
        label="失敗した件があるとき"
        hint="中で分岐して、最後のブロックまで進まなかった件も失敗と数えます。"
        error={fieldErrors.get("on_item_error")}
      >
        <Select value={onItemError} onValueChange={(v) => patchParams(dispatch, node, { on_item_error: v })}>
          <FieldSelectTrigger className="h-8">
            <SelectValue />
          </FieldSelectTrigger>
          <SelectContent>
            {Object.entries(MAP_ITEM_ERROR).map(([value, o]) => (
              <SelectItem key={value} value={value} note={o.note}>
                {o.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </Field>
      {/* 中のブロックの案内（D-85: キャンバスは枠の中、スマホは字下げした一覧の「中にブロックを追加」から足す）。 */}
      <div className="space-y-1 rounded-md border bg-muted/40 p-2 text-[11px] leading-relaxed text-muted-foreground" data-testid="map-inside-note">
        <p>
          1 件ごとに動かすブロックは、「中にブロックを追加」から、このブロックの中に追加します。中のブロックでは「繰り返し中の 1 件」を値に使えます。
        </p>
        <p>
          テーブルのレコードを 1 件ずつ処理するなら、きっかけ「{TRANSITION_TRIGGER}」などを使うと、繰り返しは要りません。
        </p>
      </div>
    </div>
  );
}

export function WaitForm({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
  const p = paramsOf<NodeParamsByType["control.wait"]>(node);
  const kind = (p.kind as string | undefined) ?? "duration";
  return (
    <div className="space-y-3">
      <Field label="待ち方" error={fieldErrors.get("kind")}>
        <Select
          value={kind}
          onValueChange={(v) => {
            if (v === "duration") {
              patchParams(dispatch, node, {
                kind: v,
                duration_sec: 60,
                until: undefined,
                source: undefined,
                scope: undefined,
                filter: undefined,
                timeout_sec: undefined,
                on_timeout: undefined,
              });
            } else if (v === "until") {
              patchParams(dispatch, node, {
                kind: v,
                until: "",
                duration_sec: undefined,
                source: undefined,
                scope: undefined,
                filter: undefined,
                timeout_sec: undefined,
                on_timeout: undefined,
              });
            } else {
              patchParams(dispatch, node, {
                kind: v,
                source: "storage.write",
                duration_sec: undefined,
                until: undefined,
              });
            }
          }}
        >
          <FieldSelectTrigger className="h-8">
            <SelectValue />
          </FieldSelectTrigger>
          <SelectContent>
            <SelectItem value="duration">決めた時間だけ待つ</SelectItem>
            <SelectItem value="until">決めた日時まで待つ</SelectItem>
            <SelectItem value="event">できごとが起きるまで待つ</SelectItem>
          </SelectContent>
        </Select>
      </Field>
      {kind === "duration" ? (
        <ValueExprInput
          label="待つ秒数"
          value={p.duration_sec as ValueExpr | undefined}
          onChange={(v) =>
            patchParams(dispatch, node, {
              // 実行側は数値として解決する（as_i64）ため、数字テキストは number で保存する。
              duration_sec:
                typeof v === "string" && /^\d+$/.test(v.trim()) ? Number(v.trim()) : v,
            })
          }
          refCandidates={refCandidates}
          inMapRegion={inMapRegion}
          placeholder="例: 3600"
          error={fieldErrors.get("duration_sec")}
        />
      ) : null}
      {kind === "until" ? (
        <ValueExprInput
          label="待つ日時（RFC3339）"
          value={p.until as ValueExpr | undefined}
          onChange={(v) => patchParams(dispatch, node, { until: v })}
          refCandidates={refCandidates}
          inMapRegion={inMapRegion}
          placeholder="例: 2026-08-01T09:00:00+09:00"
          error={fieldErrors.get("until")}
        />
      ) : null}
      {kind === "event" ? (
        <div className="space-y-3">
          <Field label="待つできごと" error={fieldErrors.get("source")}>
            <Select
              value={(p.source as string | undefined) ?? "storage.write"}
              onValueChange={(v) => patchParams(dispatch, node, { source: v })}
            >
              <FieldSelectTrigger className="h-8">
                <SelectValue />
              </FieldSelectTrigger>
              <SelectContent>
                <SelectItem value="storage.write">ファイルが保存されたとき</SelectItem>
              </SelectContent>
            </Select>
          </Field>
          <Field
            label="対象フォルダ ID"
            hint="このフォルダ配下への保存を待ちます。必須です"
            error={fieldErrors.get("scope")}
          >
            <FieldInput
              value={
                ((p.scope as { folder?: string } | undefined)?.folder as string | undefined) ?? ""
              }
              onChange={(e) =>
                patchParams(dispatch, node, {
                  scope: e.target.value ? { folder: e.target.value } : undefined,
                })
              }
              placeholder="フォルダ ID"
              className="h-8 font-mono text-xs max-md:text-base"
            />
          </Field>
          <Field label="待ちの上限（秒・省略可）" hint="空欄で無期限" error={fieldErrors.get("timeout_sec")}>
            <NumberInput
              value={typeof p.timeout_sec === "number" ? p.timeout_sec : null}
              min={1}
              onChange={(n) =>
                patchParams(dispatch, node, { timeout_sec: n ?? undefined })
              }
            />
          </Field>
          <Field label="時間切れのとき">
            <Select
              value={(p.on_timeout as string | undefined) ?? "fail"}
              onValueChange={(v) => patchParams(dispatch, node, { on_timeout: v })}
            >
              <FieldSelectTrigger className="h-8">
                <SelectValue />
              </FieldSelectTrigger>
              <SelectContent>
                <SelectItem value="fail">失敗にする</SelectItem>
                <SelectItem value="continue">「時間切れ」の出口に流す</SelectItem>
              </SelectContent>
            </Select>
          </Field>
        </div>
      ) : null}
    </div>
  );
}

export function JoinForm({ node, dispatch }: FormProps) {
  const p = paramsOf<NodeParamsByType["control.join"]>(node);
  return (
    <Field label="待ち合わせ" hint="分かれた流れをどう待つか">
      <Select
        value={(p.mode as string | undefined) ?? "all"}
        onValueChange={(v) => patchParams(dispatch, node, { mode: v })}
      >
        <FieldSelectTrigger className="h-8">
          <SelectValue />
        </FieldSelectTrigger>
        <SelectContent>
          <SelectItem value="all">全部そろってから進む</SelectItem>
          <SelectItem value="any">最初の 1 つで進む</SelectItem>
        </SelectContent>
      </Select>
    </Field>
  );
}
