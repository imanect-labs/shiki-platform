"use client";

/// 値の入力ウィジェット（IR の ValueExpr = 固定値 / $from 参照 / $template）。
///
/// IT に詳しくない人向けに「固定値」「前の結果から」「文章を組み立て」の 3 モードで見せる。
/// $from の source は閉集合（input / trigger / each / nodes.<id>.output）で、ブロックの結果は
/// 祖先候補（呼び出し側が渡す）、入力項目は input_schema（`InputFieldsContext`）から選ぶ。
///
/// 文字を求める欄（`text`）には「差し込む」を置く（BUG-1）。選ぶとカーソル位置に `{名前}` を入れて vars に
/// 参照を足し、固定値ならテンプレート（文章を組み立て）へ切り替える。差し込み名は path から付ける（/title → title）。
/// 文字でない欄（リスト・数・オブジェクト・比較の値・ID）には出さない: 差し込むと値が文字列に変わり、
/// 保存は通っても実行で型が合わなくなる。参照は「前の結果から」で `$from` のまま入れる。
/// 固定値に `{{x}}`・`{x}`・`${x}` と書くと差し込みにならず文字のまま送られるので、その場で知らせ、
/// ワンクリックでテンプレートに変換できるようにする（テンプレートの記法は `{name}`・`{{` は波括弧そのもの）。

import * as React from "react";
import { Trash2, TriangleAlert } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import type { TemplateExpr, ValueExpr } from "@/generated/workflow-ir";
import type { InputFieldInfo } from "@/lib/workflow-input-schema";
import { InsertMenu, RefPicker } from "./ref-picker";
import {
  escapeTemplateText,
  findSuspects,
  fixEscapedVars,
  guessRef,
  insertIntoLiteral,
  insertVar,
  isFromRef,
  joinPieces,
  literalToTemplate,
  renameVar,
  templateIssues,
  templatePieces,
  templateToText,
  varNameFor,
  type RefCandidate,
  type RefContext,
  type RefValue,
} from "./value-refs";

export type { RefCandidate };

/// この実行の入力項目（ワークフローの input_schema の properties）。設定パネルが渡す。
export const InputFieldsContext = React.createContext<InputFieldInfo[]>([]);

/// ワークフローのイベントトリガの source（きっかけの内容の項目を候補に出す・BUG-36）。設定パネルが渡す。
export const TriggerSourcesContext = React.createContext<string[]>([]);

type Mode = "literal" | "from" | "template";

function modeOf(value: ValueExpr | undefined): Mode {
  if (value && typeof value === "object" && value !== null) {
    if ("$from" in value) return "from";
    if ("$template" in value) return "template";
  }
  return "literal";
}

/// 固定値の数字を数として保存する欄の種類（[`numericLiteral`]）。
/// - `auto`: 等値（eq / neq）で、左辺の型が分からない。数字だけ（`-12`・`3.5`・全角の数字も）なら数にするが、
///   頭に 0 を重ねた数字（コードの「001」「08」）と「-0」は文字のまま（数にすると文字の項目との等値が黙って偽になる）
/// - `order`: 大小比較（gt / gte / lt / lte）で、左辺の型が分からない。頭に 0 を重ねた数字も数にする（数字を
///   文字で書いた値との大小比較は、数との比較なら実行で expr_type_error になり、保存時にもサーバが
///   NumericString で警告する。文字との大小比較を数字の文字で書く理由は無い）
/// - `always`: 数と比べると分かっている欄（左辺が数値の入力項目）。桁区切りの `,` も外して読む
export type NumericMode = "auto" | "order" | "always";

/// 固定値の文字を数として保存するか（条件の比べる値・BUG-31）。数として読めなければ文字のまま返す。
/// 16 桁以上は数にしない（JavaScript の数で桁が落ち、別の値を保存してしまう・ID などは文字のまま）。
/// 入力中の文字（「0.」「1.0」）は欄が下書きとして持つので、ここで数の正規形に潰しても打てなくならない。
export function numericLiteral(text: string, mode: NumericMode): string | number {
  const half = text.trim().replace(/[０-９．－]/g, (c) => String.fromCharCode(c.charCodeAt(0) - 0xfee0));
  const t = mode === "always" ? half.replace(/,/g, "") : half;
  // 末尾の「.」（打っている途中の「0.」）と、整数部を省いた「.5」も数として読む（文字の「0.」「.5」で保存しない）。
  if (!/^-?(\d+(\.\d*)?|\.\d+)$/.test(t) || t.replace(/[-.]/g, "").length > 15) return text;
  const n = Number(t);
  if (mode === "auto" && (/^-?0\d/.test(t) || Object.is(n, -0))) return text;
  return n;
}

/// 2 つの固定値が同じ値か（入力中の下書きが、いまの値から作ったものかを見る）。
function sameLiteral(a: unknown, b: unknown): boolean {
  return a === b || JSON.stringify(a) === JSON.stringify(b);
}

function literalText(value: unknown): { text: string; isJson: boolean } {
  if (typeof value === "string") return { text: value, isJson: false };
  if (value === undefined) return { text: "", isJson: false };
  return { text: JSON.stringify(value), isJson: true };
}

/// 注意の色（設定パネルの「保存はできますが、確認してください」と揃える）。
const NOTE_STYLE: React.CSSProperties = {
  borderColor: "color-mix(in oklab, var(--season-autumn) 45%, transparent)",
  backgroundColor: "color-mix(in oklab, var(--season-autumn) 8%, transparent)",
};

type TextEl = HTMLInputElement | HTMLTextAreaElement;
type Range = { start: number; end: number };

type Props = {
  label: string;
  value: ValueExpr | undefined;
  onChange: (next: ValueExpr) => void;
  /// `nodes.<id>.output` の参照候補（祖先ブロック）。
  refCandidates: RefCandidate[];
  /// map 領域内なら each を出す。
  inMapRegion?: boolean;
  placeholder?: string;
  /// 複数行の固定値（プロンプト等）。
  multiline?: boolean;
  error?: string | null;
  /// 固定値を JSON のオブジェクト（または配列）に限る（業務データの値など）。文字列としては保存しない:
  /// 読めない間は IR に値を入れず（保存時の検証で「未入力」として止まる）、その場で理由を出す（C-P2-1）。
  json?: "object" | "array";
  /// 文字を求める欄（指示・検索語・名前・内容など）。「差し込む」と固定値の差し込みの注意を出す。
  text?: boolean;
  /// 固定値の数字を数として保存する（条件の比べる値・[`numericLiteral`]・BUG-31）。数になった値は
  /// 「数値やリストとして扱う」をオンにしなくても数のまま見せ、その旨を欄の下に出す。
  numeric?: NumericMode;
  /// 欄の下の注記（値の決め方によらず出す）。
  hint?: string;
};

export function ValueExprInput({
  label,
  value,
  onChange,
  refCandidates,
  inMapRegion,
  placeholder,
  multiline,
  error,
  json,
  text,
  numeric,
  hint,
}: Props) {
  const inputFields = React.useContext(InputFieldsContext);
  const triggerSources = React.useContext(TriggerSourcesContext);
  const ctx = React.useMemo<RefContext>(
    () => ({ inputFields, ancestors: refCandidates, inMapRegion: Boolean(inMapRegion), triggerSources }),
    [inputFields, refCandidates, inMapRegion, triggerSources],
  );
  // 「前の結果から」を選んだが、まだ参照先を選んでいない間（IR は変えずに候補を見せる）。
  const [pendingFrom, setPendingFrom] = React.useState(false);
  const actual = modeOf(value);
  const mode: Mode = pendingFrom && actual !== "from" ? "from" : actual;
  const from = actual === "from" ? (value as RefValue) : null;
  const template = actual === "template" ? (value as TemplateExpr) : null;
  const literal = actual === "literal" ? literalText(value) : { text: "", isJson: false };
  // 数の欄（`numeric`）では、数は数字を打てば数になるので「数値やリストとして扱う」をオンにして見せない
  // （リスト・真偽・null を書くときだけオンにする）。
  const autoNumber = Boolean(numeric) && typeof value === "number";
  const [jsonMode, setJsonMode] = React.useState(literal.isJson && !autoNumber);
  const toText = (text: string): ValueExpr => (numeric ? numericLiteral(text, numeric) : text) as ValueExpr;
  // 固定値の欄に打った文字から作る値（「数値やリストとして扱う」なら JSON として読み、読めなければ文字）。
  const literalOf = (text: string): ValueExpr => {
    if (jsonMode) {
      try {
        return JSON.parse(text) as ValueExpr;
      } catch {
        // 不完全な JSON 入力中は文字列のまま保持。
      }
    }
    return toText(text);
  };
  // 固定値の欄の入力中の文字。値を数・JSON に直すと「0.」「1.0」「-0」が正規形に潰れ、打ち続けると別の数
  // （「0.08」が 8）になる・IME の変換が壊れるので、打った文字のまま見せる（BUG-31）。いまの値がこの文字から
  // 作った値と違う（元に戻した・ほかの操作で書き換わった）ときは使わない。
  const [literalDraft, setLiteralDraft] = React.useState<string | null>(null);
  const shownLiteral =
    mode === "literal" && literalDraft !== null && sameLiteral(literalOf(literalDraft), value)
      ? literalDraft
      : literal.text;
  // JSON 限定の欄の入力中の文字（読めない間も消さずに見せる）。
  const [jsonDraft, setJsonDraft] = React.useState<string | null>(null);
  const jsonError = React.useMemo(() => {
    if (!json || mode !== "literal") return null;
    const text = jsonDraft ?? literal.text;
    if (text.trim() === "") return null;
    try {
      const v = JSON.parse(text) as unknown;
      const ok = json === "array" ? Array.isArray(v) : v !== null && typeof v === "object" && !Array.isArray(v);
      return ok ? null : json === "array" ? "[ … ] の配列で書いてください" : "{ … } のオブジェクトで書いてください";
    } catch {
      return "JSON として読めません（引用符・カンマ・括弧を確かめてください）";
    }
  }, [json, mode, jsonDraft, literal.text]);

  // 差し込む位置（最後にカーソルがあった所）と、差し込んだ後にカーソルを置く位置。
  const textRef = React.useRef<TextEl | null>(null);
  const selection = React.useRef<Range | null>(null);
  const caretAfter = React.useRef<number | null>(null);
  const remember = (e: React.SyntheticEvent<TextEl>) => {
    const el = e.currentTarget;
    selection.current = { start: el.selectionStart ?? el.value.length, end: el.selectionEnd ?? el.value.length };
  };
  React.useLayoutEffect(() => {
    const at = caretAfter.current;
    const el = textRef.current;
    if (at === null || !el) return;
    caretAfter.current = null;
    el.focus();
    el.setSelectionRange(at, at);
    selection.current = { start: at, end: at };
  });

  const emit = (next: ValueExpr) => {
    setPendingFrom(false);
    // 入力方法が変わったら前のカーソル位置は使わない（固定値とテンプレートでは文字の位置が違う。
    // 差し込んだ直後の位置は描画の後に置き直す）。
    if (modeOf(next) !== actual) selection.current = null;
    onChange(next);
  };

  // 文字として差し込める欄か（文字を求める欄だけ。JSON として扱う固定値・JSON 限定の欄には差し込まない）。
  const textual =
    Boolean(text) &&
    ((mode === "literal" && !json && !jsonMode && (value === undefined || typeof value === "string")) ||
      mode === "template");

  const insert = (ref: RefValue) => {
    if (template) {
      const range = selection.current ?? { start: template.$template.length, end: template.$template.length };
      const r = insertVar(template, range, ref);
      caretAfter.current = r.caret;
      emit(r.template as ValueExpr);
      return;
    }
    const text = typeof value === "string" ? value : "";
    // カーソルの位置は固定値の文字の位置。テンプレートでは波括弧が二重になるので位置を写して差し込む。
    const range = selection.current ?? { start: text.length, end: text.length };
    const r = insertIntoLiteral(text, range, ref);
    caretAfter.current = r.caret;
    emit(r.template as ValueExpr);
  };

  const setMode = (next: Mode) => {
    if (next === mode) return;
    setJsonDraft(null);
    // 前の欄のカーソル位置は持ち越さない（固定値とテンプレートでは文字の位置が違う）。
    selection.current = null;
    // 参照先を選ぶ前に元の入力方法へ戻した（IR は変えていない）。
    if (pendingFrom && next === actual) {
      setPendingFrom(false);
      return;
    }
    if (next !== "literal") setJsonMode(false);
    if (next === "from") {
      // 参照先の既定は入れない（入力全体を黙って入れない）。候補から選ばせる。1 つだけ差し込んだ
      // テンプレートはその参照にする。
      const [only, ...others] = template ? Object.entries(template.vars ?? {}) : [];
      if (template && only && others.length === 0 && isFromRef(only[1]) && template.$template.trim() === `{${only[0]}}`) {
        emit(only[1] as ValueExpr);
        return;
      }
      setPendingFrom(true);
      return;
    }
    if (next === "literal") {
      if (template) emit((json ? undefined : templateToText(template.$template)) as ValueExpr);
      else emit((json ? undefined : "") as ValueExpr);
      return;
    }
    // テンプレートへ: 参照は 1 つの差し込みにする。固定値は差し込みのつもりの書き方（`{{x}}` など）を
    // 差し込みに直し、ほかの文字はそのまま出るように残す（「差し込みに変換する」と同じ）。
    if (from) {
      const { name } = varNameFor(from, {});
      emit({ $template: `{${name}}`, vars: { [name]: from } } as ValueExpr);
      return;
    }
    emit(
      (typeof value === "string" ? literalToTemplate(value, ctx) : { $template: escapeTemplateText(literal.text), vars: {} }) as ValueExpr,
    );
  };

  const suspects = mode === "literal" && textual && typeof value === "string" ? findSuspects(value) : [];

  return (
    <div className="space-y-1.5" data-value-expr={label}>
      <div className="flex flex-wrap items-center justify-between gap-x-2 gap-y-0.5">
        <label className="min-w-0 text-xs font-medium text-foreground">{label}</label>
        <div className="ml-auto flex items-center gap-0.5">
          {textual ? (
            <InsertMenu ctx={ctx} onPick={insert} onClosed={() => textRef.current?.focus()} />
          ) : null}
          <Select value={mode} onValueChange={(v) => setMode(v as Mode)}>
            <SelectTrigger
              // 欄の名前（label）を含めない（getByLabel(label) が入力の欄だけに当たるように）。
              aria-label="値の決め方"
              className="h-6 w-auto gap-1 border-none bg-transparent px-1.5 text-[11px] text-muted-foreground shadow-none"
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent align="end">
              <SelectItem value="literal">固定値</SelectItem>
              <SelectItem value="from">前の結果から</SelectItem>
              <SelectItem value="template">文章を組み立て</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>

      {mode === "literal" && json ? (
        <div className="space-y-1">
          <Textarea
            aria-label={label}
            value={jsonDraft ?? literal.text}
            rows={4}
            placeholder={placeholder}
            spellCheck={false}
            aria-invalid={jsonError ? true : undefined}
            onChange={(e) => {
              const text = e.target.value;
              setJsonDraft(text);
              try {
                const v = JSON.parse(text) as unknown;
                const ok = json === "array" ? Array.isArray(v) : v !== null && typeof v === "object" && !Array.isArray(v);
                emit((ok ? v : undefined) as ValueExpr);
              } catch {
                emit(undefined as unknown as ValueExpr);
              }
            }}
            className={cn("font-mono text-xs", (error || jsonError) && "border-destructive")}
          />
          {jsonError ? (
            <p className="text-[11px] text-destructive" role="alert">
              {jsonError}
            </p>
          ) : null}
        </div>
      ) : mode === "literal" ? (
        <div className="space-y-1">
          <TextField
            multiline={multiline}
            label={label}
            value={shownLiteral}
            placeholder={placeholder}
            invalid={Boolean(error)}
            textRef={textRef}
            onSelect={remember}
            onChange={(text) => {
              setLiteralDraft(text);
              emit(literalOf(text));
            }}
          />
          {autoNumber && !jsonMode ? (
            <p className="text-[11px] text-muted-foreground" data-testid="numeric-literal-note">
              数値として比べます（文字として比べるなら「文章を組み立て」で書きます）
            </p>
          ) : null}
          {suspects.length > 0 ? (
            <div role="note" className="space-y-1.5 rounded-md border p-2 text-[11px] leading-relaxed" style={NOTE_STYLE} data-testid="literal-var-warning">
              <p className="flex items-start gap-1.5">
                <TriangleAlert className="mt-0.5 size-3.5 shrink-0 text-[var(--season-autumn)]" aria-hidden />
                <span>
                  {suspects
                    .slice(0, 3)
                    .map((s) => `「${s.text}」`)
                    .join("")}
                  {suspects.length > 3 ? ` ほか ${suspects.length - 3} 件` : ""}
                  は差し込みになりません。固定値のままでは文字のまま送られます。
                </span>
              </p>
              <Button
                variant="outline"
                size="sm"
                className="h-7 text-xs"
                onClick={() => emit(literalToTemplate(literal.text, ctx) as ValueExpr)}
              >
                差し込みに変換する
              </Button>
            </div>
          ) : null}
          <label className="flex items-center gap-1.5 text-[11px] text-muted-foreground">
            <Switch
              checked={jsonMode}
              onCheckedChange={(on) => {
                setJsonMode(on);
                // 切替時に IR 内の値も追随させる（ON: 今のテキストを JSON として解釈、
                // OFF: 文字列に戻す）。トグルだけで型が変わらない齟齬を防ぐ。
                if (on) {
                  try {
                    emit(JSON.parse(shownLiteral) as ValueExpr);
                  } catch {
                    // 解釈できないテキストは文字列のまま（入力継続で解釈される）。
                  }
                } else if (literal.isJson) {
                  emit(toText(shownLiteral));
                }
              }}
              className="scale-75"
            />
            数値やリストとして扱う（JSON）
          </label>
        </div>
      ) : null}

      {mode === "from" ? (
        <div className="rounded-md border bg-muted/40 p-2">
          <RefPicker value={from} ctx={ctx} onChange={(r) => emit(r as ValueExpr)} />
        </div>
      ) : null}

      {mode === "template" && template ? (
        <TemplateEditor
          template={template}
          ctx={ctx}
          label={label}
          multiline={multiline}
          invalid={Boolean(error)}
          textRef={textRef}
          onSelect={remember}
          onChange={(next) => emit(next as ValueExpr)}
          onReshape={() => {
            selection.current = null;
          }}
        />
      ) : null}

      {hint ? <p className="text-[11px] text-muted-foreground">{hint}</p> : null}
      {/* 手元で JSON として読めない理由を出しているときは、サーバの同じ欄の誤りを重ねて出さない。 */}
      {error && !jsonError ? <p className="text-[11px] text-destructive">{error}</p> : null}
    </div>
  );
}

/// 1 行／複数行の文字の欄（差し込む位置を覚えるため ref と選択の通知を受ける）。
function TextField({
  multiline,
  label,
  value,
  placeholder,
  invalid,
  textRef,
  onSelect,
  onChange,
  rows = 3,
}: {
  multiline?: boolean;
  label: string;
  value: string;
  placeholder?: string;
  invalid: boolean;
  textRef: React.RefObject<TextEl | null>;
  onSelect: (e: React.SyntheticEvent<TextEl>) => void;
  onChange: (text: string) => void;
  rows?: number;
}) {
  if (multiline) {
    return (
      <Textarea
        ref={textRef as React.Ref<HTMLTextAreaElement>}
        aria-label={label}
        value={value}
        rows={rows}
        placeholder={placeholder}
        onSelect={onSelect}
        onChange={(e) => {
          onSelect(e);
          onChange(e.target.value);
        }}
        className={cn(invalid && "border-destructive")}
      />
    );
  }
  return (
    <Input
      ref={textRef as React.Ref<HTMLInputElement>}
      aria-label={label}
      value={value}
      placeholder={placeholder}
      onSelect={onSelect}
      onChange={(e) => {
        onSelect(e);
        onChange(e.target.value);
      }}
      className={cn("h-8", invalid && "border-destructive")}
    />
  );
}

function TemplateEditor({
  template,
  ctx,
  label,
  multiline,
  invalid,
  textRef,
  onSelect,
  onChange,
  onReshape,
}: {
  template: TemplateExpr;
  ctx: RefContext;
  label: string;
  multiline?: boolean;
  invalid: boolean;
  textRef: React.RefObject<TextEl | null>;
  onSelect: (e: React.SyntheticEvent<TextEl>) => void;
  onChange: (next: TemplateExpr) => void;
  /// 本文を入力欄の外で書き換える前に呼ぶ（覚えているカーソル位置がずれるので捨てる）。
  onReshape: () => void;
}) {
  const vars = Object.entries(template.vars ?? {});
  const issues = templateIssues(template);

  const removeVar = (name: string) => {
    onReshape();
    const next = { ...(template.vars ?? {}) };
    delete next[name];
    // 本文の `{name}` も外す（残すと実行で「vars にありません」になる）。
    const pieces = templatePieces(template.$template).filter((p) => !(p.kind === "var" && p.name === name));
    onChange({ $template: joinPieces(pieces), vars: next });
  };

  const addMissing = () => {
    const next = { ...(template.vars ?? {}) };
    for (const name of issues.missing) next[name] = guessRef(name, ctx) as ValueExpr;
    onChange({ ...template, vars: next });
  };

  return (
    <div className="space-y-1.5 rounded-md border bg-muted/40 p-2">
      <TextField
        multiline={multiline}
        label={label}
        value={template.$template}
        placeholder={"例: {name} さんの申請が届きました"}
        invalid={invalid}
        textRef={textRef}
        onSelect={onSelect}
        onChange={(text) => onChange({ ...template, $template: text })}
      />
      <p className="text-[11px] leading-relaxed text-muted-foreground">
        <code className="whitespace-nowrap font-mono">{"{名前}"}</code> の所に下の値が入ります。波括弧そのものは{" "}
        <code className="whitespace-nowrap font-mono">{"{{ }}"}</code> と書きます
      </p>
      {issues.missing.length > 0 ? (
        <TemplateNote
          text={`${issues.missing.map((n) => `「{${n}}」`).join("")}に入る値がありません（このままでは実行で失敗します）`}
          action="差し込む値を足す"
          onAction={addMissing}
        />
      ) : null}
      {issues.escaped.length > 0 ? (
        <TemplateNote
          text={`${issues.escaped.map((n) => `「{{${n}}}」`).join("")}は波括弧そのものとして出ます（差し込まれません）`}
          action={`${issues.escaped.map((n) => `{${n}}`).join("・")} に直す`}
          onAction={() => {
            onReshape();
            onChange(fixEscapedVars(template));
          }}
        />
      ) : null}
      {vars.map(([name, expr], i) => (
        <div key={i} className="space-y-1 rounded-md border bg-background p-1.5" data-testid="template-var">
          <div className="flex items-center gap-1">
            <span className="font-mono text-[11px] text-muted-foreground" aria-hidden>
              {"{"}
            </span>
            <Input
              value={name}
              onChange={(e) => {
                const next = renameVar(template, name, e.target.value);
                if (!next) return;
                onReshape();
                onChange(next);
              }}
              placeholder="名前"
              className="h-7 w-24 font-mono text-xs max-md:text-base"
              aria-label="差し込み名"
            />
            <span className="font-mono text-[11px] text-muted-foreground" aria-hidden>
              {"}"}
            </span>
            <span className="min-w-0 flex-1 truncate text-[11px] text-muted-foreground">に入る値</span>
            <Button
              variant="ghost"
              size="icon"
              className="size-7"
              aria-label={`差し込み「${name}」を削除`}
              onClick={() => removeVar(name)}
            >
              <Trash2 className="size-3.5" aria-hidden />
            </Button>
          </div>
          {isFromRef(expr) ? null : (
            <p className="px-0.5 text-[11px] text-muted-foreground">
              固定の値: <code className="break-all font-mono">{JSON.stringify(expr)}</code>
            </p>
          )}
          <RefPicker
            value={isFromRef(expr) ? expr : null}
            ctx={ctx}
            ariaLabel={`「${name}」に入る値`}
            onChange={(r) => onChange({ ...template, vars: { ...(template.vars ?? {}), [name]: r as ValueExpr } })}
          />
        </div>
      ))}
    </div>
  );
}

function TemplateNote({ text, action, onAction }: { text: string; action: string; onAction: () => void }) {
  return (
    <div role="note" className="space-y-1.5 rounded-md border p-2 text-[11px] leading-relaxed" style={NOTE_STYLE}>
      <p className="flex items-start gap-1.5">
        <TriangleAlert className="mt-0.5 size-3.5 shrink-0 text-[var(--season-autumn)]" aria-hidden />
        <span>{text}</span>
      </p>
      <Button variant="outline" size="sm" className="h-7 text-xs" onClick={onAction}>
        {action}
      </Button>
    </div>
  );
}
