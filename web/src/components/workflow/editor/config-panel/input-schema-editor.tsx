"use client";

/// 手動で実行するときの入力項目（IR の `input_schema`）の編集（GAP-12）。
///
/// 項目ごとにキー（英数字・_）・表示名・型（文字列・数値・整数・真偽・日付・選択肢）・必須を並べ、
/// JSON Schema のサブセット（lib/workflow-input-schema.ts）として保存する。表で書けない形（範囲・
/// パターン・入れ子など）の定義は壊さず「JSON で編集」で開く。
/// キーはブロックの「差し込む」の候補になり、ミニアプリの画面の部品の `as` の名前と対応する。
/// 誤り（キーの重なりなど）がある間は定義に書き戻さず、行の下に理由を出す。
/// キーを変えると、ブロックの参照（`$from: "input"` の path）も同じ 1 手で新しいキーへ書き換える
/// （呼び出し側へ旧キー → 新キーを渡す）。項目を消しても参照は残るので、定義に無いキーを使っている
/// ブロックを一覧で知らせる。

import * as React from "react";
import { Plus, Trash2, TriangleAlert } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { cn } from "@/lib/utils";
import {
  buildInputSchema,
  INPUT_FIELD_KINDS,
  INPUT_KEY_RE,
  inputRowErrors,
  parseInputSchema,
  type InputFieldKind,
  type InputFieldRow,
} from "@/lib/workflow-input-schema";

/// ブロックが参照している入力項目のキーと、そのブロックの表示名。
export type InputRefUsage = { key: string; blocks: string[] };

/// 行ごとの、今の定義でのキー（足したばかりの行は null）。キーの変更をブロックの参照へ写すのに使う。
type Origins = (string | null)[];

export function InputSchemaEditor({
  schema,
  onChange,
  usages = [],
}: {
  schema: unknown;
  /// `renames` は表でキーを変えた項目（旧キー → 新キー）。
  onChange: (next: unknown, renames?: Record<string, string>) => void;
  usages?: InputRefUsage[];
}) {
  const parsed = React.useMemo(() => parseInputSchema(schema), [schema]);
  // 表で書けない形は必ず JSON で見せる（Undo などで外から変わったときも、表で上書きして壊さない）。
  // 一度 JSON で見せたら、書き直して表で書ける形になっても勝手に表へ切り替えない（「表で編集」で戻る）。
  const [preferJson, setPreferJson] = React.useState(parsed === null);
  if (parsed === null && !preferJson) setPreferJson(true);
  const jsonMode = preferJson || parsed === null;
  // 誤りのある間の行（定義には書き戻していない）。定義が外から変わったら（Undo など）捨てる。
  const [draft, setDraft] = React.useState<{ rows: InputFieldRow[]; origins: Origins } | null>(null);
  const [draftFor, setDraftFor] = React.useState<unknown>(schema);
  if (draftFor !== schema) {
    setDraftFor(schema);
    setDraft(null);
  }
  const committed = parsed?.fields ?? [];
  const rows = draft?.rows ?? committed;
  const origins: Origins = draft?.origins ?? committed.map((f) => f.key);
  const errors = inputRowErrors(rows);

  const update = (next: InputFieldRow[], nextOrigins: Origins) => {
    if (inputRowErrors(next).size > 0) {
      setDraft({ rows: next, origins: nextOrigins });
      return;
    }
    setDraft(null);
    const renames: Record<string, string> = {};
    next.forEach((r, i) => {
      const from = nextOrigins[i];
      if (from !== null && from !== r.key) renames[from] = r.key;
    });
    onChange(buildInputSchema({ fields: next, rest: parsed?.rest ?? {} }), renames);
  };
  const patchRow = (i: number, patch: Partial<InputFieldRow>) =>
    update(
      rows.map((r, j) => (j === i ? { ...r, ...patch } : r)),
      origins,
    );
  const addRow = (key?: string) => {
    const keys = new Set(rows.map((r) => r.key));
    let n = rows.length + 1;
    while (keys.has(`field_${n}`)) n += 1;
    update([...rows, { key: key ?? `field_${n}`, title: "", kind: "string", required: false, options: [] }], [...origins, null]);
  };
  // 定義に無いキーを使っているブロック（項目を消した・キーを JSON で書き換えた・AI が作った定義など）。
  // 誤りのある間の行ではなく、保存される定義と突き合わせる。
  const declared = new Set(committed.map((f) => f.key));
  const dangling = jsonMode ? [] : usages.filter((u) => !declared.has(u.key));

  return (
    <div className="space-y-2.5" data-testid="input-schema-editor">
      <div className="flex items-center justify-between gap-2">
        <p className="text-xs font-medium text-foreground">入力項目</p>
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
        実行するときに入れてもらう値です。ブロックの「差し込む」から使えます。ミニアプリの画面の部品とは、
        部品の「as キー」の名前で対応します（例: <code className="font-mono">件名 = text as title</code>）。
      </p>
      {jsonMode ? (
        <SchemaJson schema={schema} tableable={parsed !== null} onChange={onChange} />
      ) : (
        <>
          {rows.map((row, i) => (
            <FieldRow
              key={i}
              row={row}
              error={errors.get(i) ?? null}
              onChange={(patch) => patchRow(i, patch)}
              onRemove={() =>
                update(
                  rows.filter((_, j) => j !== i),
                  origins.filter((_, j) => j !== i),
                )
              }
            />
          ))}
          {draft !== null ? (
            <p className="text-[11px] text-destructive" role="alert">
              直すまで入力項目の変更は保存されません
            </p>
          ) : null}
          <Button variant="outline" size="sm" className="h-7 text-xs" onClick={() => addRow()}>
            <Plus className="size-3.5" aria-hidden />
            項目を追加
          </Button>
          {dangling.length > 0 ? (
            <DanglingRefs
              usages={dangling}
              // 誤りのある行を直している間は足さない（直しかけの行と重なるかもしれない）。
              onAdd={draft === null ? addRow : undefined}
            />
          ) : null}
        </>
      )}
    </div>
  );
}

function FieldRow({
  row,
  error,
  onChange,
  onRemove,
}: {
  row: InputFieldRow;
  error: string | null;
  onChange: (patch: Partial<InputFieldRow>) => void;
  onRemove: () => void;
}) {
  // 選択肢は 1 行 1 つで書く（入力中の空行は消さずに残す）。
  const [optionsText, setOptionsText] = React.useState(row.options.join("\n"));
  const [optionsFor, setOptionsFor] = React.useState(row.options);
  if (optionsFor !== row.options && optionsFor.join("\n") !== row.options.join("\n")) {
    setOptionsFor(row.options);
    setOptionsText(row.options.join("\n"));
  }
  const name = row.title || row.key || "項目";
  return (
    <div className="space-y-2 rounded-md border bg-muted/30 p-2" data-testid="input-field-row">
      <div className="flex items-center gap-1.5">
        <Input
          value={row.title}
          onChange={(e) => onChange({ title: e.target.value })}
          placeholder="表示名（例: 件名）"
          aria-label="表示名"
          className="h-8 min-w-0 flex-1 text-xs max-md:text-base"
        />
        <Button
          variant="ghost"
          size="icon"
          className="size-8 shrink-0"
          aria-label={`入力項目「${name}」を削除`}
          onClick={onRemove}
        >
          <Trash2 className="size-3.5" aria-hidden />
        </Button>
      </div>
      <div className="flex items-center gap-1.5">
        <Input
          value={row.key}
          onChange={(e) => onChange({ key: e.target.value.trim() })}
          placeholder="キー（例: title）"
          aria-label="キー"
          aria-invalid={error?.startsWith("キー") ? true : undefined}
          spellCheck={false}
          autoCapitalize="off"
          className="h-8 min-w-0 flex-1 font-mono text-xs max-md:text-base"
        />
        <Select value={row.kind} onValueChange={(v) => onChange({ kind: v as InputFieldKind })}>
          <SelectTrigger className="h-8 w-32 shrink-0 text-xs" aria-label="型">
            <SelectValue />
          </SelectTrigger>
          <SelectContent align="end">
            {INPUT_FIELD_KINDS.map((k) => (
              <SelectItem key={k.value} value={k.value}>
                {k.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
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
          placeholder={"選択肢を 1 行に 1 つ\n例: 交通費\n出張費"}
          aria-label="選択肢"
          className="text-xs"
        />
      ) : null}
      <label className="flex items-center gap-1.5 text-[11px] text-muted-foreground">
        <Switch
          checked={row.required}
          onCheckedChange={(on) => onChange({ required: on })}
          className="scale-75"
          aria-label="必須"
        />
        必須
      </label>
      {error ? (
        <p className="text-[11px] text-destructive" role="alert">
          {error}
        </p>
      ) : null}
    </div>
  );
}

/// 定義に無いキーを使っているブロックの一覧（実行すると、その参照は「解決できません」で失敗する）。
function DanglingRefs({ usages, onAdd }: { usages: InputRefUsage[]; onAdd?: (key: string) => void }) {
  return (
    <div
      role="note"
      className="space-y-1.5 rounded-md border p-2 text-[11px] leading-relaxed"
      style={{
        borderColor: "color-mix(in oklab, var(--season-autumn) 45%, transparent)",
        backgroundColor: "color-mix(in oklab, var(--season-autumn) 8%, transparent)",
      }}
      data-testid="input-dangling-refs"
    >
      <p className="flex items-start gap-1.5">
        <TriangleAlert className="mt-0.5 size-3.5 shrink-0 text-[var(--season-autumn)]" aria-hidden />
        <span>
          入力項目に無いキーを使っているブロックがあります。このまま実行すると、そのブロックは失敗します。
          項目を足すか、ブロックの参照先を選び直してください。
        </span>
      </p>
      <ul className="space-y-1">
        {usages.map((u) => (
          <li key={u.key} className="flex items-center justify-between gap-2">
            <span className="min-w-0 break-words">
              <code className="font-mono">{u.key}</code>
              {"（"}
              {u.blocks.map((b) => `「${b}」`).join("")}
              {"）"}
            </span>
            {onAdd && INPUT_KEY_RE.test(u.key) ? (
              <Button variant="outline" size="sm" className="h-7 shrink-0 text-xs" onClick={() => onAdd(u.key)}>
                項目に足す
              </Button>
            ) : null}
          </li>
        ))}
      </ul>
    </div>
  );
}

/// JSON で書く（表で書けない形・上級者向け）。読めない間は定義に書き戻さない。
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
        aria-label="入力項目（JSON Schema）"
        aria-invalid={error ? true : undefined}
        placeholder={'{\n  "type": "object",\n  "properties": { "title": { "type": "string" } }\n}'}
        onChange={(e) => {
          const next = e.target.value;
          setText(next);
          if (next.trim() === "") {
            setError(null);
            emit(null);
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
          範囲・パターン・入れ子などを使った定義は、表では編集できません（JSON のまま保存します）
        </p>
      ) : null}
    </div>
  );
}
