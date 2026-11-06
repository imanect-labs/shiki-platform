"use client";

/// 手動実行ダイアログ（対話トリガ・実行主体 = 自分）。
///
/// input_schema の properties があれば 1 階層の簡易フォーム、無ければ JSON 入力。
/// 項目の型に合わせて入力欄を変える（日付は日付の欄、選択肢と真偽は選ぶ欄・GAP-12）。表示名（title）があれば
/// それを見出しにし、必須の項目には印を付ける。
/// 実行後は実行履歴ページ（?run= deep-link）へ誘導する。
///
/// - 説明は付録 C の §3.6 の決まった文（使ってよい操作でさらに絞ることを言う・BUG-109）。
/// - 必須の欄が空なら送らずに欄の下で知らせる。サーバーの入力の誤りは、項目のキーを見出しに直して欄の下に出し、
///   ほかの理由（実行できない設定・見つからない・使えない）はダイアログの中の帯に出す（本文を捨てない・BUG-109）。
/// - きっかけがファイルやテーブルの変更のワークフローは、「詳しい設定」できっかけの内容（JSON）を入れて試せる
///   （D-93。空欄なら、きっかけの内容は入力と同じ）。
/// - スケジュールのきっかけがあるワークフローは、「詳しい設定」で予定の日時を変えて試せる（GAP-88）。欄は開いたときの
///   日時（実行情報の時間帯）で始め、変えたときだけ送る（変えなければ、予定の日時は実行した日時のまま）。
/// - 開いたときに、実行するのに自分に足りない権限を確かめ（`GET /workflows/{id}/access`・アプリのボタンと同じ決まり・
///   GAP-37）、足りなければ理由を出す。押したときに確かめ直し、まだ足りなければ送らない（共有してもらっていれば、
///   開き直さずに実行できる・BUG-217 と同じ考え方）。確かめられなかったときは止めない（サーバーが本人の権限で確かめる）。
///   頼む先は、確かめの答えに添えられた「持ち主か」で言い分ける（答えが届く前は理由を出さない・BUG-230）。

import * as React from "react";
import { useRouter } from "next/navigation";
import { AlertTriangle, ChevronDown, Loader2, Play } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { toast } from "@/components/ui/use-toast";
import { cn } from "@/lib/utils";
import { getWorkflow } from "@/lib/workflow-api";
import { RunStartError, startRun } from "@/lib/workflow-run-api";
import { getMyWorkflowAccess, type MyWorkflowAccess } from "@/lib/artifact-api";
import { myRunBlockedReason } from "@/lib/access-wording";
import { blockDisplayName } from "@/lib/workflow-block-name";
import { inputFieldsOf, type InputFieldInfo } from "@/lib/workflow-input-schema";
import { localDateTimeIn, zoneLabel } from "@/lib/time-zone";
import { EVENT_CATALOG, RUN_INFO_TZ } from "@/generated/workflow-catalog";
import type { WorkflowIr } from "@/generated/workflow-ir";

/// 付録 C の §3.6 の決まった文（手動の実行のダイアログ）。
const RUN_NOTE = "あなたの権限で 1 回実行します。このワークフローが使ってよい操作のうち、あなたにできることだけを行います。";
/// 実行履歴に残る値（付録 D の N4.7 の 3 文目）。
const HISTORY_NOTE = "実行履歴には、各ブロックが読んだ値と結果が残り、このワークフローを見られる人が読めます。";
const JSON_SYNTAX = "JSON の書き方が正しくありません。引用符、カンマ、括弧の対応を確かめてください。";

/// input_schema の型宣言に合わせて文字列入力を変換する（number/boolean）。
/// 変換できない値は文字列のまま渡す（実行側の検証・エラーに委ねる）。
function coerceValue(raw: string, kind: InputFieldInfo["kind"] | undefined): unknown {
  if (kind === "number" || kind === "integer") {
    const n = Number(raw);
    return Number.isFinite(n) && raw.trim() !== "" ? n : raw;
  }
  if (kind === "boolean") {
    if (raw === "true") return true;
    if (raw === "false") return false;
    return raw;
  }
  return raw;
}

function schemaFields(ir: WorkflowIr): InputFieldInfo[] | null {
  const fields = inputFieldsOf(ir.input_schema);
  return fields.length > 0 ? fields : null;
}

/// きっかけの呼び名（スケジュールか、ファイルやテーブルの変更の具体名・カタログの単一定義から）。
function triggerLabel(t: WorkflowIr["triggers"][number]): string | null {
  if (t.kind === "schedule") return "スケジュール";
  if (t.kind === "event") {
    return EVENT_CATALOG.find((e) => e.source === t.source)?.label_ja ?? "ファイルやテーブルの変更";
  }
  return null;
}

/// 手動でないきっかけ（説明の 2 文目）。手動のきっかけがあれば null。
function nonManualTrigger(ir: WorkflowIr): string | null {
  if (ir.triggers.some((t) => t.kind === "interactive")) return null;
  const t = ir.triggers[0];
  return t ? triggerLabel(t) : null;
}

/// ファイルやテーブルの変更のきっかけ（きっかけの内容の欄を出す・D-93）。スケジュールの内容は空なので出さない。
function eventTrigger(ir: WorkflowIr): { label: string; source: string } | null {
  const t = ir.triggers.find((x) => x.kind === "event");
  return t && t.kind === "event" ? { label: triggerLabel(t) ?? "", source: String(t.source) } : null;
}

/// スケジュールのきっかけがあるか（予定の日時の欄を出す・GAP-88）。
function hasSchedule(ir: WorkflowIr): boolean {
  return ir.triggers.some((t) => t.kind === "schedule");
}

/// ファイルやテーブルの変更のきっかけの内容の項目（カタログの単一定義から・D-47）。
function payloadSkeleton(source: string): string {
  const entry = EVENT_CATALOG.find((e) => e.source === source);
  const value = (type: string): unknown =>
    type.startsWith("number")
      ? 0
      : type.startsWith("boolean")
        ? false
        : type.startsWith("Array")
          ? []
          : type.startsWith("{")
            ? {}
            : "";
  return JSON.stringify(
    Object.fromEntries((entry?.payload_fields ?? []).map((f) => [f.name, value(f.type)])),
    null,
    2,
  );
}

/// JSON の欄の値を読む（空なら `undefined`・項目のまとまりでなければ誤り）。
function parseJsonObject(text: string): { ok: true; value: Record<string, unknown> | undefined } | { ok: false; error: string } {
  if (!text.trim()) return { ok: true, value: undefined };
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    return { ok: false, error: JSON_SYNTAX };
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return { ok: false, error: "項目のまとまり（{ } で囲んだ JSON）で書いてください。" };
  }
  return { ok: true, value: value as Record<string, unknown> };
}

/// 1 項目の入力欄（型ごと）。値は文字で持ち、送るときに型へ変換する。
function FieldInput({
  field,
  value,
  onChange,
  invalid,
  describedBy,
}: {
  field: InputFieldInfo;
  value: string;
  onChange: (next: string) => void;
  invalid: boolean;
  describedBy: string | undefined;
}) {
  const label = field.title ?? field.key;
  if (field.kind === "enum" || field.kind === "boolean") {
    const options =
      field.kind === "boolean"
        ? [
            { value: "true", label: "はい" },
            { value: "false", label: "いいえ" },
          ]
        : field.options.map((o) => ({ value: o, label: o }));
    return (
      <Select value={value} onValueChange={onChange}>
        <SelectTrigger
          className={cn("h-8", invalid && "border-destructive")}
          aria-label={label}
          aria-invalid={invalid || undefined}
          aria-describedby={describedBy}
        >
          <SelectValue placeholder="選んでください" />
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
  return (
    <Input
      type={field.kind === "date" ? "date" : field.kind === "number" || field.kind === "integer" ? "number" : "text"}
      inputMode={field.kind === "integer" ? "numeric" : field.kind === "number" ? "decimal" : undefined}
      value={value}
      onChange={(e) => onChange(e.target.value)}
      aria-label={label}
      aria-invalid={invalid || undefined}
      aria-describedby={describedBy}
      className={cn("h-8", invalid && "border-destructive")}
    />
  );
}

/// サーバーの入力の誤りのキー（`addr.zip`・`tags[2]`）から、表の項目のキー（最上位）を取り出す。
function topKey(key: string): string {
  return key.split(/[.[]/)[0] ?? "";
}

export function RunNowDialog({
  open,
  onOpenChange,
  workflowId,
  ir,
  version,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  workflowId: string;
  ir: WorkflowIr;
  /// フォームの元になった保存済みバージョン（実行前に最新と一致することを確認する）。
  version: number;
}) {
  const router = useRouter();
  const props = schemaFields(ir);
  const manualOnlyFor = nonManualTrigger(ir);
  const trigger = eventTrigger(ir);
  const schedule = hasSchedule(ir);
  const idBase = React.useId();
  const [fields, setFields] = React.useState<Record<string, string>>({});
  const [jsonText, setJsonText] = React.useState("{}");
  const [triggerText, setTriggerText] = React.useState("");
  /// 予定の日時の欄（`YYYY-MM-DDTHH:mm`・実行情報の時間帯）と、開いたときの値（変えたときだけ送る）。
  const [scheduledText, setScheduledText] = React.useState("");
  const [scheduledInitial, setScheduledInitial] = React.useState("");
  const [advanced, setAdvanced] = React.useState(false);
  const [busy, setBusy] = React.useState(false);
  /// 欄の下の誤り（項目のキー → 文・`$input` は入力の JSON・`$trigger` はきっかけの内容・`$scheduled` は予定の日時）。
  const [fieldErrors, setFieldErrors] = React.useState<Record<string, string>>({});
  /// 欄に写せない誤り（ダイアログの中の帯）。
  const [failure, setFailure] = React.useState<{ message: string; lines: string[] } | null>(null);
  /// ブロックの名前（名前を付けていなければカタログのブロック名。ブロックの ID は出さない・付録 C の P1）。
  const labels = React.useMemo(() => new Map(ir.nodes.map((n) => [n.id, blockDisplayName(n)])), [ir.nodes]);
  /// 実行するのに自分に足りない権限（開いたときに確かめる・読めなかったときは null で止めない）。頼む先は、答えに
  /// 添えられた持ち主かで言い分ける（別の問い合わせの答えを待たない・BUG-230）。
  const [access, setAccess] = React.useState<MyWorkflowAccess | null>(null);
  const blockedReason = access ? myRunBlockedReason(access.items, access.owner) : null;

  React.useEffect(() => {
    if (!open) return;
    setFieldErrors({});
    setFailure(null);
    setAccess(null);
    const now = localDateTimeIn(RUN_INFO_TZ);
    setScheduledText(now);
    setScheduledInitial(now);
    let live = true;
    getMyWorkflowAccess(workflowId)
      .then((a) => live && setAccess(a))
      .catch(() => {
        /* 確かめられなくても実行はできる（サーバーが本人の権限で確かめる） */
      });
    return () => {
      live = false;
    };
  }, [open, workflowId]);

  const run = async () => {
    const errors: Record<string, string> = {};
    let input: unknown = {};
    if (props) {
      for (const f of props) {
        if (f.required && !(fields[f.key] ?? "").trim()) {
          errors[f.key] = `「${f.title ?? f.key}」を${f.kind === "enum" || f.kind === "boolean" ? "選んで" : "入れて"}ください。`;
        }
      }
      input = Object.fromEntries(
        Object.entries(fields)
          .filter(([, v]) => v !== "")
          .map(([k, v]) => [k, coerceValue(v, props.find((f) => f.key === k)?.kind)]),
      );
    } else {
      try {
        input = jsonText.trim() ? JSON.parse(jsonText) : {};
      } catch {
        errors.$input = JSON_SYNTAX;
      }
    }
    let triggerValue: Record<string, unknown> | undefined;
    if (trigger) {
      const parsed = parseJsonObject(triggerText);
      if (parsed.ok) triggerValue = parsed.value;
      else {
        errors.$trigger = parsed.error;
        setAdvanced(true);
      }
    }
    // 予定の日時は変えたときだけ送る（空にしたときも送らない＝実行した日時）。
    const scheduledAt = schedule && scheduledText && scheduledText !== scheduledInitial ? scheduledText : undefined;
    setFieldErrors(errors);
    setFailure(null);
    if (Object.keys(errors).length > 0) return;
    setBusy(true);
    try {
      // 足りない権限があると分かっていれば、確かめ直してから送る（共有してもらっていれば、そのまま実行する）。
      if (blockedReason) {
        const fresh = await getMyWorkflowAccess(workflowId).catch(() => null);
        if (fresh) setAccess(fresh);
        const still = fresh ? myRunBlockedReason(fresh.items, fresh.owner) : null;
        if (still) {
          // 理由はすぐ下の注意に出ているので、ここではまだ足りないことだけを言う。
          setFailure({ message: "足りない権限が、まだ共有されていません。", lines: [] });
          return;
        }
      }
      // 実行は常に最新保存版で走る（backend 仕様）。別の編集者が保存していた場合、
      // このフォームは古いスキーマなので実行せず再読込を促す。
      const latest = await getWorkflow(workflowId);
      if (latest.version !== version) {
        toast({
          variant: "destructive",
          title: "ほかの人が先にワークフローを保存しました",
          description:
            "実行は最新のバージョンで動くため、入力項目が変わっていることがあります。最新の内容を読み込んでから、もう一度実行してください。",
        });
        return;
      }
      const runId = await startRun(workflowId, input, { trigger: triggerValue, scheduledAt });
      if (runId) {
        onOpenChange(false);
        toast({ title: "実行を始めました", description: "実行履歴で進み具合を見られます。" });
        router.push(`/workflows/${workflowId}/runs?run=${runId}`);
      } else {
        setFailure({
          message:
            "同時に動かせる実行の数に達していて、このワークフローは上限を超えた実行を見送る設定です。いまの実行が終わってから、もう一度実行してください。",
          lines: [],
        });
      }
    } catch (e) {
      if (!(e instanceof RunStartError)) {
        setFailure({ message: e instanceof Error ? e.message : String(e), lines: [] });
        return;
      }
      // 入力の誤りは欄の下へ（同じ項目の誤りはまとめて同じ欄の下に。表に無い項目・入力全体の誤りは帯へ）。帯では
      // 項目のキー（`addr.zip`・`tags[2]`）を出さず、入力項目の見出しで言う（付録 C の P1）。
      const shown: Record<string, string> = {};
      const rest: string[] = [];
      for (const issue of e.issues) {
        const key = topKey(issue.key);
        const field = props?.find((f) => f.key === key);
        if (field) {
          shown[key] = `${shown[key] ?? ""}${issue.message}。`;
        } else if (!props && !shown.$input) {
          shown.$input = `${issue.key || "入力"}: ${issue.message}。`;
        } else if (!props) {
          rest.push(`${issue.key || "入力"}: ${issue.message}`);
        } else {
          rest.push(`入力: ${issue.message}`);
        }
      }
      if (e.field === "trigger") {
        shown.$trigger = e.message;
        setAdvanced(true);
      }
      if (e.field === "scheduled_at") {
        shown.$scheduled = e.message;
        setAdvanced(true);
      }
      setFieldErrors(shown);
      const lines = [
        ...rest,
        ...e.reasons.map((r) => (r.nodeId ? `「${labels.get(r.nodeId) ?? r.nodeId}」: ${r.message}` : r.message)),
      ];
      const fieldShown = e.field === "trigger" || e.field === "scheduled_at";
      if (lines.length > 0 || (e.issues.length === 0 && !fieldShown)) {
        setFailure({ message: e.message, lines });
      }
    } finally {
      setBusy(false);
    }
  };

  const errorId = (key: string) => `${idBase}-${key}-error`;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Play className="size-5 text-primary" aria-hidden />
            いま実行する
          </DialogTitle>
          <DialogDescription className="space-y-1">
            <span className="block">{RUN_NOTE}</span>
            {manualOnlyFor ? (
              <span className="block">{`このワークフローのきっかけは「${manualOnlyFor}」ですが、ここでは試しに 1 回だけ実行します。`}</span>
            ) : null}
            <span className="block">{HISTORY_NOTE}</span>
          </DialogDescription>
        </DialogHeader>
        {failure ? (
          <div
            role="alert"
            className="space-y-1 rounded-md border border-destructive/40 bg-destructive/5 p-2.5 text-xs"
            data-testid="run-now-failure"
          >
            <p className="flex items-center gap-1.5 font-medium text-destructive">
              <AlertTriangle className="size-4 shrink-0" aria-hidden />
              実行できませんでした
            </p>
            <p className="text-foreground/80">{failure.message}</p>
            {failure.lines.length > 0 ? (
              <ul className="list-disc space-y-0.5 pl-5 text-foreground/80">
                {failure.lines.map((l, i) => (
                  <li key={i}>{l}</li>
                ))}
              </ul>
            ) : null}
          </div>
        ) : null}
        {blockedReason ? (
          <p
            role="note"
            className="flex items-start gap-1.5 rounded-md border border-amber-500/30 bg-amber-500/5 p-2.5 text-xs text-amber-700 dark:text-amber-400"
            data-testid="run-now-access"
          >
            <AlertTriangle className="mt-0.5 size-4 shrink-0" aria-hidden />
            <span className="min-w-0 [overflow-wrap:anywhere]">
              {blockedReason}共有してもらった後は、そのまま実行できます。
            </span>
          </p>
        ) : access?.truncated ? (
          <p className="text-[11px] text-muted-foreground" role="note" data-testid="run-now-access-truncated">
            このワークフローは使うテーブルやファイルが多いため、一部は前もって権限を確かめていません。
          </p>
        ) : null}
        {props ? (
          <div className="space-y-3">
            {props.map((field) => {
              const error = fieldErrors[field.key];
              const noteId = field.description ? `${idBase}-${field.key}-note` : undefined;
              return (
                <div key={field.key} className="space-y-1">
                  <label className="text-xs font-medium">
                    {field.title ?? field.key}
                    {field.required ? (
                      <>
                        <span className="ml-1 text-destructive" aria-hidden>
                          *
                        </span>
                        <span className="sr-only">（必須）</span>
                      </>
                    ) : null}
                  </label>
                  <FieldInput
                    field={field}
                    value={fields[field.key] ?? ""}
                    onChange={(v) => setFields({ ...fields, [field.key]: v })}
                    invalid={Boolean(error)}
                    describedBy={[error ? errorId(field.key) : null, noteId].filter(Boolean).join(" ") || undefined}
                  />
                  {error ? (
                    <p id={errorId(field.key)} className="text-[11px] text-destructive">
                      {error}
                    </p>
                  ) : null}
                  {field.description ? (
                    <p id={noteId} className="text-[11px] text-muted-foreground">
                      {field.description}
                    </p>
                  ) : null}
                </div>
              );
            })}
          </div>
        ) : (
          <div className="space-y-1">
            <label className="text-xs font-medium" htmlFor={`${idBase}-input`}>
              入力（JSON・省略可）
            </label>
            <Textarea
              id={`${idBase}-input`}
              value={jsonText}
              rows={4}
              onChange={(e) => setJsonText(e.target.value)}
              aria-invalid={fieldErrors.$input ? true : undefined}
              aria-describedby={fieldErrors.$input ? errorId("input") : undefined}
              className={cn("font-mono text-xs", fieldErrors.$input && "border-destructive")}
            />
            {fieldErrors.$input ? (
              <p id={errorId("input")} className="text-[11px] text-destructive">
                {fieldErrors.$input}
              </p>
            ) : null}
          </div>
        )}
        {trigger || schedule ? (
          <div className="space-y-2">
            <button
              type="button"
              aria-expanded={advanced}
              aria-controls={`${idBase}-advanced`}
              onClick={() => setAdvanced((v) => !v)}
              className="flex items-center gap-1 rounded-sm text-xs font-medium text-muted-foreground hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              <ChevronDown className={cn("size-3.5 transition-transform", advanced && "rotate-180")} aria-hidden />
              詳しい設定
            </button>
            {advanced ? (
              <div id={`${idBase}-advanced`} className="space-y-3">
                {schedule ? (
                  <div className="space-y-1" data-testid="run-now-scheduled">
                    <label className="text-xs font-medium" htmlFor={`${idBase}-scheduled`}>
                      {`予定の日時（${zoneLabel(RUN_INFO_TZ)}）`}
                    </label>
                    <Input
                      id={`${idBase}-scheduled`}
                      type="datetime-local"
                      step={60}
                      value={scheduledText}
                      onChange={(e) => setScheduledText(e.target.value)}
                      aria-invalid={fieldErrors.$scheduled ? true : undefined}
                      aria-describedby={`${fieldErrors.$scheduled ? `${errorId("scheduled")} ` : ""}${idBase}-scheduled-note`}
                      className={cn("h-8 w-full min-w-0 tabular-nums", fieldErrors.$scheduled && "border-destructive")}
                    />
                    {fieldErrors.$scheduled ? (
                      <p id={errorId("scheduled")} className="text-[11px] text-destructive">
                        {fieldErrors.$scheduled}
                      </p>
                    ) : null}
                    <p id={`${idBase}-scheduled-note`} className="text-[11px] leading-relaxed text-muted-foreground">
                      スケジュールがこの日時に動いたものとして実行し、「日付を計算」はこの日時から数えます。変えなければ、実行した日時を使います。
                    </p>
                  </div>
                ) : null}
                {trigger ? (
                  <div className="space-y-1" data-testid="run-now-trigger">
                    <div className="flex items-end justify-between gap-2">
                      <label className="text-xs font-medium" htmlFor={`${idBase}-trigger`}>
                        きっかけの内容（JSON・省略可）
                      </label>
                      <Button
                        type="button"
                        variant="outline"
                        size="sm"
                        className="h-7 text-xs"
                        onClick={() => setTriggerText(payloadSkeleton(trigger.source))}
                      >
                        項目を並べる
                      </Button>
                    </div>
                    <Textarea
                      id={`${idBase}-trigger`}
                      value={triggerText}
                      rows={5}
                      placeholder={'例: {"record_id": "…"}'}
                      onChange={(e) => setTriggerText(e.target.value)}
                      aria-invalid={fieldErrors.$trigger ? true : undefined}
                      aria-describedby={`${fieldErrors.$trigger ? `${errorId("trigger")} ` : ""}${idBase}-trigger-note`}
                      className={cn("font-mono text-xs", fieldErrors.$trigger && "border-destructive")}
                    />
                    {fieldErrors.$trigger ? (
                      <p id={errorId("trigger")} className="text-[11px] text-destructive">
                        {fieldErrors.$trigger}
                      </p>
                    ) : null}
                    <p id={`${idBase}-trigger-note`} className="text-[11px] leading-relaxed text-muted-foreground">
                      {`ここに書いた値を、「${trigger.label}」のきっかけの内容として使います。空欄なら、入力の値をきっかけの内容として使います。`}
                    </p>
                  </div>
                ) : null}
              </div>
            ) : null}
          </div>
        ) : null}
        <div className="flex justify-end">
          <Button onClick={run} disabled={busy}>
            {busy ? (
              <Loader2 className="size-4 animate-spin" aria-hidden />
            ) : (
              <Play className="size-4" aria-hidden />
            )}
            実行する
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
