"use client";

/// 「AI で判定」（ai.decide）の質問の編集（FEAT-1・ir.md §7.11）。
///
/// 質問は作者が書くコード（リテラル）で、1〜20 件・順序を保つ。種類ごとに答えの値域を決める:
/// - 選択肢（choice）: 選択肢の名前（答えの名前・テーブルの選択肢の列に書き込む値・出口の名前）と、AI への説明
///   （AI への手がかり）と、キー（英小文字・答えの値・値で振り分けるときは出口の内部名）を分けて書く（D-87）。
/// - 段階（score）: 段階の名前を低い順に並べる。
/// - はい/いいえ（boolean）: それぞれに当たる場合の説明（省略可）。
/// キーの形と重なりはその場で知らせる。ほかの規則（予約名・件数・字数）はサーバの保存時の検証が決め、
/// 誤りは欄の下に出す（検証した時点の定義のキーで質問・選択肢を引くので、並べ替えの後も取り違えない）。
/// 並べ替え・追加・削除はボタンで行い、押したボタンの行へフォーカスを移す（キーボードだけで編集できる）。

import * as React from "react";
import { ChevronDown, ChevronUp, Plus, Trash2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { cn } from "@/lib/utils";
import type { DecideOption, DecideQuestion, DecideQuestionType } from "@/generated/workflow-ir";

/// 質問・選択肢のキーの形（エンジンの `key_ok` と同じ・その場で知らせるため）。
export const DECIDE_KEY_RE = /^[a-z][a-z0-9_]{0,63}$/;

export const QUESTION_KINDS: { value: DecideQuestionType; label: string }[] = [
  { value: "choice", label: "選択肢から選ぶ" },
  { value: "score", label: "段階で答える" },
  { value: "boolean", label: "はい／いいえ" },
];

export function kindLabel(kind: DecideQuestionType | undefined): string {
  return QUESTION_KINDS.find((k) => k.value === kind)?.label ?? String(kind ?? "");
}

/// 質問ごとの誤り（欄の位置 → 文言）。位置は `key`・`instructions`・`options`・`options/<i>/key`・
/// `options/<i>/label`・`options/<i>/description`・`levels`・`levels/<i>`・`yes`・`no`（i は今の並びの添字）。
export type QuestionErrors = Map<string, string>;

const KEY_HINT = "英小文字で始め、英小文字・数字・_ だけ（64 字まで）";

/// キーの手元の誤り（形・重なり）。
function keyError(key: string, others: string[], what: string): string | null {
  if (key === "") return `${what}を入れてください`;
  if (!DECIDE_KEY_RE.test(key)) return `${what}は${KEY_HINT}で書きます`;
  if (others.includes(key)) return `${what}「${key}」が重なっています`;
  return null;
}

/// 種類を変えたときの質問（種類に合わない項目を外し、要る項目の雛形を入れる）。
export function withKind(q: DecideQuestion, kind: DecideQuestionType): DecideQuestion {
  const base: DecideQuestion = { key: q.key, type: kind, instructions: q.instructions };
  if (kind === "choice") {
    return { ...base, options: q.options && q.options.length > 0 ? q.options : [{ key: "option_1" }, { key: "option_2" }] };
  }
  if (kind === "score") return { ...base, levels: q.levels && q.levels.length > 0 ? q.levels : ["低", "中", "高"] };
  return { ...base, ...(q.yes !== undefined ? { yes: q.yes } : {}), ...(q.no !== undefined ? { no: q.no } : {}) };
}

/// 空いている連番のキー（`prefix_1`・`prefix_2`…）。
export function freshKey(prefix: string, used: string[]): string {
  let n = used.length + 1;
  while (used.includes(`${prefix}${n}`)) n += 1;
  return `${prefix}${n}`;
}

/// 並べ替え・追加の後に、同じ行のボタン（または最初の入力）へフォーカスを移す。
function useFocusRow(listRef: React.RefObject<HTMLElement | null>, rowSelector: string) {
  const [target, setTarget] = React.useState<{ index: number; selector: string } | null>(null);
  React.useEffect(() => {
    if (!target) return;
    const rows = listRef.current?.querySelectorAll<HTMLElement>(rowSelector);
    const row = rows?.[target.index];
    const el =
      row?.querySelector<HTMLElement>(`${target.selector}:not(:disabled)`) ??
      // 端の行へ動いて同じ向きのボタンが押せなくなったら、その行の並べ替えのボタン（質問の行では見出しの組が先）。
      row?.querySelector<HTMLElement>("[data-move]:not(:disabled)") ??
      row?.querySelector<HTMLElement>("input, textarea");
    el?.focus();
    setTarget(null);
  }, [target, listRef, rowSelector]);
  return (index: number, selector: string) => setTarget({ index, selector });
}

function swap<T>(xs: T[], i: number, j: number): T[] {
  return xs.map((x, k) => (k === i ? xs[j] : k === j ? xs[i] : x));
}

/// 上へ・下へ・削除のボタン。`scope` は並べ替えの後にフォーカスを戻す先を見分ける印（質問と選択肢・段階）。
function MoveButtons({
  name,
  scope,
  canUp,
  canDown,
  onMove,
  onRemove,
  removeDisabled,
}: {
  name: string;
  scope: "question" | "item";
  canUp: boolean;
  canDown: boolean;
  onMove: (dir: "up" | "down") => void;
  onRemove: () => void;
  removeDisabled?: boolean;
}) {
  return (
    <>
      <Button
        variant="ghost"
        size="icon"
        className="size-8 shrink-0 pointer-coarse:size-11"
        aria-label={`${name}を上へ`}
        data-move="up"
        data-scope={scope}
        disabled={!canUp}
        onClick={() => onMove("up")}
      >
        <ChevronUp className="size-3.5" aria-hidden />
      </Button>
      <Button
        variant="ghost"
        size="icon"
        className="size-8 shrink-0 pointer-coarse:size-11"
        aria-label={`${name}を下へ`}
        data-move="down"
        data-scope={scope}
        disabled={!canDown}
        onClick={() => onMove("down")}
      >
        <ChevronDown className="size-3.5" aria-hidden />
      </Button>
      <Button
        variant="ghost"
        size="icon"
        // 指で押す端末では 44px にし、並べ替えの組から離して置く（「下へ」のつもりで消さない）。
        className="size-8 shrink-0 pointer-coarse:ml-2 pointer-coarse:size-11"
        aria-label={`${name}を削除`}
        disabled={removeDisabled}
        onClick={onRemove}
      >
        <Trash2 className="size-3.5" aria-hidden />
      </Button>
    </>
  );
}

function FieldError({ id, message }: { id: string; message: string | null | undefined }) {
  if (!message) return null;
  return (
    <p id={id} className="text-[11px] text-destructive">
      {message}
    </p>
  );
}

/// 見出しを名前にした 1 つの欄（見出し・入力・誤り）。入力には aria-labelledby / aria-describedby を付ける。
function LabeledInput({
  label,
  error,
  hint,
  children,
}: {
  label: string;
  error?: string | null;
  hint?: string;
  children: (a11y: { "aria-labelledby": string; "aria-describedby"?: string; "aria-invalid"?: true }) => React.ReactNode;
}) {
  const labelId = React.useId();
  const descId = React.useId();
  return (
    <div className="space-y-1">
      <label id={labelId} className="text-[11px] font-medium text-muted-foreground">
        {label}
      </label>
      {children({
        "aria-labelledby": labelId,
        ...(error || hint ? { "aria-describedby": descId } : {}),
        ...(error ? { "aria-invalid": true as const } : {}),
      })}
      {error ? (
        <FieldError id={descId} message={error} />
      ) : hint ? (
        <p id={descId} className="text-[11px] text-muted-foreground">
          {hint}
        </p>
      ) : null}
    </div>
  );
}

export function DecideQuestions({
  questions,
  onChange,
  onRenameKey,
  onRenameOption,
  errorsFor,
  generalError,
}: {
  questions: DecideQuestion[];
  /// 質問の一覧を書き戻す。
  onChange: (next: DecideQuestion[]) => void;
  /// 質問のキーを変えた（振り分けの質問を同じ 1 手で付け替える）。
  onRenameKey: (index: number, from: string, to: string, next: DecideQuestion[]) => void;
  /// 選択肢のキーを変えた（値で振り分けている質問なら、その出口のエッジも同じ 1 手で付け替える）。
  onRenameOption: (index: number, from: string, to: string, next: DecideQuestion[]) => void;
  /// 質問ごとの保存時の検証の誤り（今の並びの添字）。
  errorsFor: (index: number) => QuestionErrors;
  /// 質問の一覧全体の誤り（`/params/questions`）。
  generalError?: string | null;
}) {
  const headingId = React.useId();
  const listRef = React.useRef<HTMLDivElement>(null);
  const focusRow = useFocusRow(listRef, '[data-testid="decide-question"]');
  const keys = questions.map((q) => q.key);
  const replace = (i: number, q: DecideQuestion) => questions.map((x, j) => (j === i ? q : x));

  return (
    <div className="space-y-2.5" role="group" aria-labelledby={headingId} data-testid="decide-questions">
      <p id={headingId} className="text-xs font-medium text-foreground">
        質問
      </p>
      <p className="text-[11px] leading-relaxed text-muted-foreground">
        判定の材料について AI に答えさせることです。答えは後ろのブロックの「差し込む」から「〜の 〈キー〉 の答え」で使えます。
      </p>
      <div ref={listRef} className="space-y-3">
        {questions.map((q, i) => (
          <QuestionCard
            key={i}
            index={i}
            question={q}
            otherKeys={keys.filter((_, j) => j !== i)}
            errors={errorsFor(i)}
            canMove={{ up: i > 0, down: i < questions.length - 1 }}
            canRemove={questions.length > 1}
            onChange={(next) => onChange(replace(i, next))}
            onRenameKey={(from, to, next) => onRenameKey(i, from, to, replace(i, next))}
            onRenameOption={(from, to, next) => onRenameOption(i, from, to, replace(i, next))}
            onMove={(dir) => {
              const j = dir === "up" ? i - 1 : i + 1;
              if (j < 0 || j >= questions.length) return;
              onChange(swap(questions, i, j));
              focusRow(j, `[data-scope="question"][data-move="${dir}"]`);
            }}
            onRemove={() => onChange(questions.filter((_, j) => j !== i))}
          />
        ))}
      </div>
      {generalError ? <FieldError id={`${headingId}-e`} message={generalError} /> : null}
      <Button
        variant="outline"
        size="sm"
        className="h-7 text-xs"
        disabled={questions.length >= 20}
        onClick={() => {
          onChange([
            ...questions,
            {
              key: freshKey("q", keys),
              type: "choice",
              instructions: "",
              options: [{ key: "option_1" }, { key: "option_2" }],
            },
          ]);
          focusRow(questions.length, "input");
        }}
      >
        <Plus className="size-3.5" aria-hidden />
        質問を追加
      </Button>
    </div>
  );
}

function QuestionCard({
  index,
  question: q,
  otherKeys,
  errors,
  canMove,
  canRemove,
  onChange,
  onRenameKey,
  onRenameOption,
  onMove,
  onRemove,
}: {
  index: number;
  question: DecideQuestion;
  otherKeys: string[];
  errors: QuestionErrors;
  canMove: { up: boolean; down: boolean };
  canRemove: boolean;
  onChange: (next: DecideQuestion) => void;
  onRenameKey: (from: string, to: string, next: DecideQuestion) => void;
  onRenameOption: (from: string, to: string, next: DecideQuestion) => void;
  onMove: (dir: "up" | "down") => void;
  onRemove: () => void;
}) {
  const name = `質問 ${index + 1}（${q.key || "キーなし"}）`;
  const keyErr = keyError(q.key, otherKeys, "質問のキー") ?? errors.get("key");
  return (
    <div
      role="group"
      aria-label={name}
      className="space-y-2.5 rounded-lg border bg-muted/30 p-2.5"
      data-testid="decide-question"
    >
      <div className="flex items-center gap-1">
        <span className="min-w-0 flex-1 truncate text-xs font-medium">質問 {index + 1}</span>
        <MoveButtons
          name={name}
          scope="question"
          canUp={canMove.up}
          canDown={canMove.down}
          onMove={onMove}
          onRemove={onRemove}
          removeDisabled={!canRemove}
        />
      </div>
      <LabeledInput label="種類">
        {(a11y) => (
          <Select value={q.type} onValueChange={(v) => onChange(withKind(q, v as DecideQuestionType))}>
            <SelectTrigger {...a11y} className="h-8 w-full text-xs">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {QUESTION_KINDS.map((k) => (
                <SelectItem key={k.value} value={k.value}>
                  {k.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        )}
      </LabeledInput>
      <LabeledInput label="キー" error={keyErr} hint="後ろのブロックは /answers/〈キー〉 で読みます">
        {(a11y) => (
          <Input
            {...a11y}
            value={q.key}
            onChange={(e) => {
              const to = e.target.value.trim();
              onRenameKey(q.key, to, { ...q, key: to });
            }}
            placeholder="例: category"
            spellCheck={false}
            autoCapitalize="off"
            className="h-8 font-mono text-xs max-md:text-base"
          />
        )}
      </LabeledInput>
      <LabeledInput label="問い（何を判定するか）" error={errors.get("instructions")}>
        {(a11y) => (
          <Textarea
            {...a11y}
            value={q.instructions}
            rows={2}
            onChange={(e) => onChange({ ...q, instructions: e.target.value })}
            placeholder="例: 問い合わせの種類はどれか"
            className="text-sm"
          />
        )}
      </LabeledInput>
      {q.type === "choice" ? (
        <OptionsEditor
          name={name}
          options={q.options ?? []}
          errors={errors}
          onChange={(options) => onChange({ ...q, options })}
          onRename={(from, to, options) => onRenameOption(from, to, { ...q, options })}
        />
      ) : q.type === "score" ? (
        <LevelsEditor name={name} levels={q.levels ?? []} errors={errors} onChange={(levels) => onChange({ ...q, levels })} />
      ) : (
        <div className="grid gap-2">
          <LabeledInput label="「はい」に当たる場合（省略可）" error={errors.get("yes")}>
            {(a11y) => (
              <Input
                {...a11y}
                value={q.yes ?? ""}
                onChange={(e) => onChange({ ...q, yes: e.target.value || undefined })}
                placeholder="例: 苦情・強い不満がある"
                className="h-8 text-xs max-md:text-base"
              />
            )}
          </LabeledInput>
          <LabeledInput label="「いいえ」に当たる場合（省略可）" error={errors.get("no")}>
            {(a11y) => (
              <Input
                {...a11y}
                value={q.no ?? ""}
                onChange={(e) => onChange({ ...q, no: e.target.value || undefined })}
                placeholder="例: 通常の問い合わせ"
                className="h-8 text-xs max-md:text-base"
              />
            )}
          </LabeledInput>
        </div>
      )}
    </div>
  );
}

function OptionsEditor({
  name,
  options,
  errors,
  onChange,
  onRename,
}: {
  name: string;
  options: DecideOption[];
  errors: QuestionErrors;
  onChange: (next: DecideOption[]) => void;
  onRename: (from: string, to: string, next: DecideOption[]) => void;
}) {
  const headingId = React.useId();
  const hintId = React.useId();
  const listRef = React.useRef<HTMLDivElement>(null);
  const focusRow = useFocusRow(listRef, '[data-testid="decide-option"]');
  const keys = options.map((o) => o.key);
  const labels = options.map((o) => (o.label ?? "").trim());
  const replace = (i: number, o: DecideOption) => options.map((x, j) => (j === i ? o : x));
  return (
    <div className="space-y-1.5" role="group" aria-labelledby={headingId} aria-describedby={hintId}>
      <p id={headingId} className="text-[11px] font-medium text-muted-foreground">
        選択肢
      </p>
      <p id={hintId} className="text-[11px] leading-relaxed text-muted-foreground">
        AI はこの中から 1 つを選びます。名前は答えの名前になります。テーブルの選択肢の列に書き込むなら、列の選択肢と同じ文字にします。
      </p>
      <div ref={listRef} className="space-y-1.5">
        {options.map((o, i) => {
          const label = labels[i];
          const optName = `${name}の選択肢「${label || o.key || i + 1}」`;
          const keyErr = keyError(o.key, keys.filter((_, j) => j !== i), "選択肢のキー") ?? errors.get(`options/${i}/key`);
          const labelErr =
            (label !== "" && labels.some((l, j) => j !== i && l === label)
              ? `選択肢の名前「${label}」はほかの選択肢でも使っています。別の名前にしてください。`
              : null) ?? errors.get(`options/${i}/label`);
          const descErr = errors.get(`options/${i}/description`);
          const rowErr = keyErr ?? labelErr ?? descErr;
          const errId = `${headingId}-o${i}`;
          // 名前の無い選択肢（名前を足す前の定義）は、今までどおり説明かキーが答えの名前になる（D-87）。
          const fallback = (o.description ?? "").trim() || o.key;
          const noteId = `${headingId}-n${i}`;
          const describedBy = [rowErr ? errId : null, label === "" && fallback ? noteId : null].filter(Boolean).join(" ");
          return (
            <div key={i} className="space-y-1 rounded-md border bg-background p-1.5" data-testid="decide-option">
              <div className="flex items-center gap-0.5">
                <Input
                  value={o.label ?? ""}
                  onChange={(e) => onChange(replace(i, { ...o, label: e.target.value === "" ? undefined : e.target.value }))}
                  onBlur={(e) => {
                    // 前後の空白は外して保存する（出口とテーブルの列に書く値）。
                    const trimmed = e.target.value.trim();
                    if (trimmed !== (o.label ?? "")) onChange(replace(i, { ...o, label: trimmed === "" ? undefined : trimmed }));
                  }}
                  placeholder="例: 請求"
                  aria-label={`選択肢 ${i + 1} の名前`}
                  aria-invalid={labelErr ? true : undefined}
                  aria-describedby={describedBy || undefined}
                  className="mr-1 h-8 min-w-0 flex-1 text-xs max-md:text-base"
                />
                <MoveButtons
                  name={optName}
                  scope="item"
                  canUp={i > 0}
                  canDown={i < options.length - 1}
                  onMove={(dir) => {
                    const j = dir === "up" ? i - 1 : i + 1;
                    onChange(swap(options, i, j));
                    focusRow(j, `[data-scope="item"][data-move="${dir}"]`);
                  }}
                  onRemove={() => onChange(options.filter((_, j) => j !== i))}
                />
              </div>
              <div className="flex items-center gap-1">
                <Input
                  value={o.description ?? ""}
                  onChange={(e) => onChange(replace(i, { ...o, description: e.target.value || undefined }))}
                  placeholder="AI への説明（省略可）"
                  aria-label={`選択肢 ${i + 1} の AI への説明`}
                  aria-invalid={descErr ? true : undefined}
                  aria-describedby={descErr ? errId : undefined}
                  className="h-8 min-w-0 flex-1 text-xs max-md:text-base"
                />
                {/* キーは答えの値と出口の内部名。ふだんは触らないので、行の右端に小さく出す。 */}
                <Input
                  value={o.key}
                  onChange={(e) => {
                    const to = e.target.value.trim();
                    onRename(o.key, to, replace(i, { ...o, key: to }));
                  }}
                  placeholder="例: billing"
                  aria-label={`選択肢 ${i + 1} のキー`}
                  aria-invalid={keyErr ? true : undefined}
                  aria-describedby={keyErr ? errId : undefined}
                  title="キー（英数字の名前）"
                  spellCheck={false}
                  autoCapitalize="off"
                  className="h-8 w-24 shrink-0 font-mono text-[11px] text-muted-foreground max-md:w-28 max-md:text-sm"
                />
              </div>
              {label === "" && fallback && !rowErr ? (
                <p id={noteId} className="text-[11px] text-muted-foreground">
                  名前を入れないと、「{fallback}」が答えの名前になります。
                </p>
              ) : null}
              <FieldError id={errId} message={rowErr} />
            </div>
          );
        })}
      </div>
      {errors.get("options") ? <FieldError id={`${headingId}-e`} message={errors.get("options")} /> : null}
      <Button
        variant="outline"
        size="sm"
        className="h-7 text-xs"
        disabled={options.length >= 100}
        onClick={() => {
          onChange([...options, { key: freshKey("option_", keys) }]);
          focusRow(options.length, "input");
        }}
      >
        <Plus className="size-3.5" aria-hidden />
        選択肢を追加
      </Button>
    </div>
  );
}

function LevelsEditor({
  name,
  levels,
  errors,
  onChange,
}: {
  name: string;
  levels: string[];
  errors: QuestionErrors;
  onChange: (next: string[]) => void;
}) {
  const headingId = React.useId();
  const listRef = React.useRef<HTMLDivElement>(null);
  const focusRow = useFocusRow(listRef, '[data-testid="decide-level"]');
  return (
    <div className="space-y-1.5" role="group" aria-labelledby={headingId}>
      <p id={headingId} className="text-[11px] font-medium text-muted-foreground">
        段階（低い順に並べる）
      </p>
      <div ref={listRef} className="space-y-1.5">
        {levels.map((l, i) => {
          const err = errors.get(`levels/${i}`);
          const errId = `${headingId}-l${i}`;
          return (
            <div key={i} data-testid="decide-level">
              <div className="flex items-center gap-1.5">
                <span className="w-5 shrink-0 text-right text-[11px] tabular-nums text-muted-foreground">{i}</span>
                <Input
                  value={l}
                  onChange={(e) => onChange(levels.map((x, j) => (j === i ? e.target.value : x)))}
                  placeholder="例: 中"
                  aria-label={`段階 ${i + 1}（低い方から）`}
                  aria-invalid={err ? true : undefined}
                  aria-describedby={err ? errId : undefined}
                  className={cn("h-8 min-w-0 flex-1 text-xs max-md:text-base")}
                />
                <MoveButtons
                  name={`${name}の段階「${l || i + 1}」`}
                  scope="item"
                  canUp={i > 0}
                  canDown={i < levels.length - 1}
                  onMove={(dir) => {
                    const j = dir === "up" ? i - 1 : i + 1;
                    onChange(swap(levels, i, j));
                    focusRow(j, `[data-scope="item"][data-move="${dir}"]`);
                  }}
                  onRemove={() => onChange(levels.filter((_, j) => j !== i))}
                />
              </div>
              <FieldError id={errId} message={err} />
            </div>
          );
        })}
      </div>
      {errors.get("levels") ? <FieldError id={`${headingId}-e`} message={errors.get("levels")} /> : null}
      <p className="text-[11px] text-muted-foreground">左の番号は答えの段階の番号（/answers/〈キー〉/level_index・0 から）です</p>
      <Button
        variant="outline"
        size="sm"
        className="h-7 text-xs"
        disabled={levels.length >= 10}
        onClick={() => {
          onChange([...levels, ""]);
          focusRow(levels.length, "input");
        }}
      >
        <Plus className="size-3.5" aria-hidden />
        段階を追加
      </Button>
    </div>
  );
}
