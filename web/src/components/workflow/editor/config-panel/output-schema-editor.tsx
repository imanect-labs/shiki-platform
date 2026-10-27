"use client";

/// 「AI に聞く」（llm.invoke）の出力の形（`output_schema`・GAP-13）の編集。
///
/// 項目ごとに項目名（英数字・_）・種類（文字列・数値・整数・はい/いいえ・選択肢・文字の一覧）・必須・説明を並べ、
/// JSON Schema のサブセット（lib/workflow-output-schema.ts）として保存する。表で書けない形（入れ子・null を許す型・
/// 範囲など）の定義は壊さず「JSON で編集」で開く。入力項目の表（input-schema-editor.tsx・GAP-12）と同じ作り:
/// - 手元の誤り（項目名の重なりなど）がある間は定義に書き戻さず、行の下に理由を出す。
/// - 並びは上下のボタンで変え、書き戻すたびに `x-order` に書く（保存して開き直しても同じ順・D-60）。
/// - 保存時の検証の誤り（`/params/output_schema/...`）は、項目を指すものはその行の下に、ほかは表の下に出す。
/// 形を決めると、出力に検証済みの `data` が付き、後ろのブロックの「差し込む」に項目ごとの候補が出る。

import * as React from "react";
import { ChevronDown, ChevronUp, Plus, Trash2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { cn } from "@/lib/utils";
import type { Node as IrNode, ValidationError } from "@/generated/workflow-ir";
import {
  buildOutputSchema,
  OUTPUT_FIELD_KINDS,
  outputRowErrors,
  parseOutputSchema,
  type OutputFieldKind,
  type OutputFieldRow,
} from "@/lib/workflow-output-schema";

const PREFIX = "/params/output_schema";

type MoveDir = "up" | "down";

/// JSON Pointer の 1 段を戻す（`~1` → `/`・`~0` → `~`）。
const unpointer = (s: string) => s.replace(/~1/g, "/").replace(/~0/g, "~");

/// 保存時の検証の誤りを、項目名ごとと全体に分ける。`required/<i>` は検証した時点の定義で項目名を引く
/// （検証の後に並びが変わっても取り違えない）。
function splitServerErrors(
  errors: ValidationError[],
  checkedNode: IrNode | null,
): { byKey: Map<string, string>; general: string[] } {
  const byKey = new Map<string, string>();
  const general: string[] = [];
  const checked = (checkedNode?.params as { output_schema?: { required?: unknown } } | undefined)?.output_schema;
  for (const e of errors) {
    if (!e.path || (e.path !== PREFIX && !e.path.startsWith(`${PREFIX}/`))) continue;
    const rest = e.path.slice(PREFIX.length).split("/").filter(Boolean);
    let key: string | null = null;
    if (rest[0] === "properties" && rest[1] !== undefined) key = unpointer(rest[1]);
    if (rest[0] === "required" && rest[1] !== undefined && Array.isArray(checked?.required)) {
      const name = checked.required[Number(rest[1])];
      if (typeof name === "string") key = name;
    }
    const message = e.message.replace(/^params が不正です: /, "");
    if (key !== null && !byKey.has(key)) byKey.set(key, message);
    else if (key === null) general.push(message);
  }
  return { byKey, general };
}

export function OutputSchemaEditor({
  schema,
  onChange,
  serverErrors,
  checkedNode,
}: {
  schema: unknown;
  /// 書き戻す値（undefined は出力の形を外す）。
  onChange: (next: unknown) => void;
  /// このノードへの保存時の検証の誤り（path 付き）。
  serverErrors: ValidationError[];
  /// その検証の対象だったこのノード（添字の付いた誤りの項目名を引く）。
  checkedNode: IrNode | null;
}) {
  const headingId = React.useId();
  const parsed = React.useMemo(() => parseOutputSchema(schema), [schema]);
  // 表で書けない形は必ず JSON で見せる（Undo などで外から変わったときも、表で上書きして壊さない）。
  const [preferJson, setPreferJson] = React.useState(parsed === null);
  if (parsed === null && !preferJson) setPreferJson(true);
  const jsonMode = preferJson || parsed === null;
  // 誤りのある間の行（定義には書き戻していない）。定義が外から変わったら（Undo など）捨てる。
  const [draft, setDraft] = React.useState<OutputFieldRow[] | null>(null);
  const [draftFor, setDraftFor] = React.useState<unknown>(schema);
  if (draftFor !== schema) {
    setDraftFor(schema);
    setDraft(null);
  }
  const rows = draft ?? parsed?.fields ?? [];
  const localErrors = outputRowErrors(rows);
  const server = React.useMemo(() => splitServerErrors(serverErrors, checkedNode), [serverErrors, checkedNode]);

  // 表の下に出す誤り。JSON で編集している間（行が無い）と、今の行に無い項目名を指す誤りは、項目名を添えてここに出す。
  const generalMessages = [
    ...server.general,
    ...[...server.byKey]
      .filter(([key]) => jsonMode || !rows.some((r) => r.key === key))
      .map(([key, m]) => `「${key}」: ${m}`),
  ];

  const update = (next: OutputFieldRow[]) => {
    if (outputRowErrors(next).size > 0) {
      setDraft(next);
      return;
    }
    setDraft(null);
    onChange(buildOutputSchema({ fields: next, rest: parsed?.rest ?? {} }));
  };
  const patchRow = (i: number, patch: Partial<OutputFieldRow>) => update(rows.map((r, j) => (j === i ? { ...r, ...patch } : r)));
  // 並べ替えた行の同じボタンへフォーカスを移す（行は位置で描くので、押したボタンは別の項目の行に残る）。
  const listRef = React.useRef<HTMLDivElement>(null);
  const [focusMoved, setFocusMoved] = React.useState<{ index: number; dir: MoveDir } | null>(null);
  React.useEffect(() => {
    if (!focusMoved) return;
    const row = listRef.current?.querySelectorAll<HTMLElement>('[data-testid="output-field-row"]')[focusMoved.index];
    const button =
      row?.querySelector<HTMLButtonElement>(`[data-move="${focusMoved.dir}"]:not(:disabled)`) ??
      row?.querySelector<HTMLButtonElement>("[data-move]:not(:disabled)");
    button?.focus();
    setFocusMoved(null);
  }, [focusMoved]);
  // 足した行の項目名の欄へフォーカスを移す（キーボードだけで続けて書ける）。
  const [focusAdded, setFocusAdded] = React.useState<number | null>(null);
  React.useEffect(() => {
    if (focusAdded === null) return;
    listRef.current
      ?.querySelectorAll<HTMLElement>('[data-testid="output-field-row"]')
      [focusAdded]?.querySelector<HTMLInputElement>("input")
      ?.focus();
    setFocusAdded(null);
  }, [focusAdded]);
  const moveRow = (i: number, dir: MoveDir) => {
    const j = dir === "up" ? i - 1 : i + 1;
    if (j < 0 || j >= rows.length) return;
    update(rows.map((x, k) => (k === i ? rows[j] : k === j ? rows[i] : x)));
    setFocusMoved({ index: j, dir });
  };
  const addRow = () => {
    const keys = new Set(rows.map((r) => r.key));
    let n = rows.length + 1;
    while (keys.has(`field_${n}`)) n += 1;
    update([...rows, { key: `field_${n}`, kind: "string", required: true, options: [], description: "" }]);
    setFocusAdded(rows.length);
  };

  return (
    <div className="space-y-2.5" role="group" aria-labelledby={headingId} data-testid="output-schema-editor">
      <div className="flex items-center justify-between gap-2">
        <p id={headingId} className="text-xs font-medium text-foreground">
          出力の形（省略可）
        </p>
        <button
          type="button"
          className="text-[11px] text-muted-foreground underline-offset-2 hover:text-foreground hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-50"
          disabled={!jsonMode ? draft !== null : parsed === null}
          onClick={() => setPreferJson(!jsonMode)}
        >
          {jsonMode ? "表で編集" : "JSON で編集"}
        </button>
      </div>
      <p className="text-[11px] leading-relaxed text-muted-foreground">
        {"決めると、AI の答えをこの形で受け取ります（合わなければ 1 回だけ直させ、直らなければ失敗）。後ろのブロックの「差し込む」から「〜の data」の項目を使えます。"}
      </p>
      {jsonMode ? (
        <SchemaJson schema={schema} tableable={parsed !== null} onChange={onChange} />
      ) : (
        <>
          {rows.length > 0 ? (
            <div ref={listRef} className="space-y-2.5">
              {rows.map((row, i) => (
                <FieldRow
                  key={i}
                  index={i}
                  row={row}
                  error={localErrors.get(i) ?? server.byKey.get(row.key) ?? null}
                  onChange={(patch) => patchRow(i, patch)}
                  onMove={(dir) => moveRow(i, dir)}
                  canMove={{ up: i > 0, down: i < rows.length - 1 }}
                  onRemove={() => update(rows.filter((_, j) => j !== i))}
                />
              ))}
            </div>
          ) : null}
          {draft !== null ? (
            <p className="text-[11px] text-destructive" role="alert">
              直すまで出力の形の変更は保存されません
            </p>
          ) : null}
          <Button variant="outline" size="sm" className="h-7 text-xs" onClick={addRow}>
            <Plus className="size-3.5" aria-hidden />
            出力の項目を追加
          </Button>
        </>
      )}
      {generalMessages.length > 0 ? (
        <ul className="space-y-0.5 text-[11px] text-destructive" role="alert" data-testid="output-schema-errors">
          {generalMessages.map((m, i) => (
            <li key={i}>{m}</li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

function FieldRow({
  index,
  row,
  error,
  onChange,
  onMove,
  canMove,
  onRemove,
}: {
  index: number;
  row: OutputFieldRow;
  error: string | null;
  onChange: (patch: Partial<OutputFieldRow>) => void;
  onMove: (dir: MoveDir) => void;
  canMove: Record<MoveDir, boolean>;
  onRemove: () => void;
}) {
  // 選択肢は 1 行 1 つで書く（入力中の空行は消さずに残す）。
  const [optionsText, setOptionsText] = React.useState(row.options.join("\n"));
  const [optionsFor, setOptionsFor] = React.useState(row.options);
  if (optionsFor !== row.options && optionsFor.join("\n") !== row.options.join("\n")) {
    setOptionsFor(row.options);
    setOptionsText(row.options.join("\n"));
  }
  const errorId = React.useId();
  const name = row.key || `項目 ${index + 1}`;
  return (
    <div
      role="group"
      aria-label={`出力の項目 ${index + 1}`}
      className="space-y-2 rounded-md border bg-muted/30 p-2"
      data-testid="output-field-row"
    >
      <div className="flex items-center gap-1.5">
        <Input
          value={row.key}
          onChange={(e) => onChange({ key: e.target.value.trim() })}
          placeholder="項目名（例: category）"
          aria-label="項目名"
          aria-invalid={error ? true : undefined}
          aria-describedby={error ? errorId : undefined}
          spellCheck={false}
          autoCapitalize="off"
          className="h-8 min-w-0 flex-1 font-mono text-xs max-md:text-base"
        />
        <Button
          variant="ghost"
          size="icon"
          className="size-8 shrink-0 pointer-coarse:size-11"
          aria-label={`出力の項目「${name}」を上へ`}
          data-move="up"
          disabled={!canMove.up}
          onClick={() => onMove("up")}
        >
          <ChevronUp className="size-3.5" aria-hidden />
        </Button>
        <Button
          variant="ghost"
          size="icon"
          className="size-8 shrink-0 pointer-coarse:size-11"
          aria-label={`出力の項目「${name}」を下へ`}
          data-move="down"
          disabled={!canMove.down}
          onClick={() => onMove("down")}
        >
          <ChevronDown className="size-3.5" aria-hidden />
        </Button>
        <Button
          variant="ghost"
          size="icon"
          // 指で押す端末では 44px にし、並べ替えの組から離して置く（「下へ」のつもりで消さない）。
          className="size-8 shrink-0 pointer-coarse:ml-2 pointer-coarse:size-11"
          aria-label={`出力の項目「${name}」を削除`}
          onClick={onRemove}
        >
          <Trash2 className="size-3.5" aria-hidden />
        </Button>
      </div>
      <div className="flex items-center gap-1.5">
        <Select value={row.kind} onValueChange={(v) => onChange({ kind: v as OutputFieldKind })}>
          <SelectTrigger className="h-8 w-36 shrink-0 text-xs" aria-label="種類">
            <SelectValue />
          </SelectTrigger>
          <SelectContent align="start">
            {OUTPUT_FIELD_KINDS.map((k) => (
              <SelectItem key={k.value} value={k.value}>
                {k.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <label className="ml-auto flex shrink-0 items-center gap-1.5 text-[11px] text-muted-foreground">
          <Switch
            checked={row.required}
            onCheckedChange={(on) => onChange({ required: on })}
            className="scale-75"
            aria-label="必須"
          />
          必須
        </label>
      </div>
      {row.kind === "enum" ? (
        <Textarea
          value={optionsText}
          rows={3}
          onChange={(e) => {
            const text = e.target.value;
            setOptionsText(text);
            const options = text
              .split("\n")
              .map((s) => s.trim())
              .filter(Boolean);
            setOptionsFor(options);
            onChange({ options });
          }}
          placeholder={"選択肢を 1 行に 1 つ\n例: billing\nbug"}
          aria-label="選択肢"
          className="font-mono text-xs"
        />
      ) : null}
      <Input
        value={row.description}
        onChange={(e) => onChange({ description: e.target.value })}
        placeholder="説明（AI への手がかり・省略可）"
        aria-label="説明"
        className="h-8 text-xs max-md:text-base"
      />
      {error ? (
        <p id={errorId} className="text-[11px] text-destructive" role="alert">
          {error}
        </p>
      ) : null}
    </div>
  );
}

/// JSON で書く（表で書けない形・上級者向け）。読めない間は定義に書き戻さない。空にすると形を外す。
function SchemaJson({
  schema,
  tableable,
  onChange,
}: {
  schema: unknown;
  tableable: boolean;
  onChange: (next: unknown) => void;
}) {
  const format = (v: unknown) => (v === null || v === undefined ? "" : JSON.stringify(v, null, 2));
  const [text, setText] = React.useState(() => format(schema));
  const [textFor, setTextFor] = React.useState<unknown>(schema);
  const [error, setError] = React.useState<string | null>(null);
  // 外からの変更（Undo など）だけ書き直す（自分の書き戻しでは入力中の文字・カーソルを保つ）。
  const [emitted, setEmitted] = React.useState<unknown>(schema);
  if (textFor !== schema) {
    setTextFor(schema);
    if (schema !== emitted) {
      setText(format(schema));
      setError(null);
    }
  }
  const emit = (v: unknown) => {
    setEmitted(v);
    onChange(v);
  };
  return (
    <div className="space-y-1">
      <Textarea
        value={text}
        rows={8}
        spellCheck={false}
        aria-label="出力の形（JSON Schema）"
        aria-invalid={error ? true : undefined}
        placeholder={'{\n  "type": "object",\n  "required": ["category"],\n  "properties": { "category": { "type": "string" } }\n}'}
        onChange={(e) => {
          const next = e.target.value;
          setText(next);
          if (next.trim() === "") {
            setError(null);
            emit(undefined);
            return;
          }
          try {
            const v = JSON.parse(next) as unknown;
            if (v === null || typeof v !== "object" || Array.isArray(v)) {
              setError("{ … } のオブジェクトで書いてください");
              return;
            }
            setError(null);
            emit(v);
          } catch {
            setError("JSON として読めません（引用符・カンマ・括弧を確かめてください）");
          }
        }}
        className={cn("font-mono text-xs", error && "border-destructive")}
      />
      {error ? (
        <p className="text-[11px] text-destructive" role="alert">
          {error}
        </p>
      ) : !tableable ? (
        <p className="text-[11px] text-muted-foreground">
          入れ子・null を許す型・範囲などを使った形は、表では編集できません（JSON のまま保存します）
        </p>
      ) : null}
    </div>
  );
}
