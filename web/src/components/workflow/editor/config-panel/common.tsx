"use client";

/// ノード共通の設定セクション（ブロック名・ID・リトライ・タイムアウト・エラー時の動作）と
/// フォーム部品（フィールド枠・数値入力・見出しを名前にする入力の欄）。

import * as React from "react";
import { TriangleAlert } from "lucide-react";

import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";
import type { Node as IrNode } from "@/generated/workflow-ir";
import { NODE_CATALOG } from "@/generated/workflow-catalog";
import type { EditorAction } from "../ir-state";

/// 欄の見出しと注記の id。見出しは入力の兄弟なので、そのままでは入力に名前が付かず、読み上げでも getByLabel でも
/// 引けない。[`Field`] の中の入力（[`FieldInput`]・[`FieldTextarea`]・[`FieldSelectTrigger`]・[`NumberInput`]・
/// [`useFieldControl`] を使う欄）は aria-labelledby で見出しを名前に、aria-describedby で注記・誤りを説明にする
/// （BUG-64）。
type FieldIds = { labelId: string; descId: string | undefined };
const FieldContext = React.createContext<FieldIds | undefined>(undefined);

/// `warning` は保存を止めない警告（サーバの保存時の警告のうち、欄に写すもの）。誤りが無ければ注記の代わりに出す。
export function Field({
  label,
  hint,
  error,
  warning,
  children,
}: {
  label: string;
  hint?: string;
  error?: string | null;
  warning?: string | null;
  children: React.ReactNode;
}) {
  const labelId = React.useId();
  const descId = React.useId();
  const described = Boolean(error || warning || hint);
  const ids = React.useMemo(
    () => ({ labelId, descId: described ? descId : undefined }),
    [labelId, descId, described],
  );
  return (
    <div className="space-y-1.5">
      <label id={labelId} className="text-xs font-medium text-foreground">
        {label}
      </label>
      <FieldContext.Provider value={ids}>{children}</FieldContext.Provider>
      {error ? (
        <p id={descId} className="text-[11px] text-destructive">
          {error}
        </p>
      ) : warning ? (
        <p id={descId} className="flex items-start gap-1 text-[11px] leading-relaxed text-foreground" data-field-warning>
          <TriangleAlert className="mt-px size-3.5 shrink-0 text-[var(--season-autumn)]" aria-hidden />
          <span className="min-w-0 [overflow-wrap:anywhere]">{warning}</span>
        </p>
      ) : hint ? (
        <p id={descId} className="text-[11px] text-muted-foreground">
          {hint}
        </p>
      ) : null}
    </div>
  );
}

/// [`Field`] の中の入力に付ける名前（見出し）と説明（注記・誤り）の属性。`named` は呼び出し側が aria-label /
/// aria-labelledby で名付けた欄（見出しで上書きしない・aria-labelledby は aria-label より優先されるため）。
/// [`Field`] の外では何も付けない。
export function useFieldControl(named = false): { "aria-labelledby"?: string; "aria-describedby"?: string } {
  const ids = React.useContext(FieldContext);
  if (!ids) return {};
  return {
    ...(named ? {} : { "aria-labelledby": ids.labelId }),
    ...(ids.descId ? { "aria-describedby": ids.descId } : {}),
  };
}

type Named = { "aria-label"?: string; "aria-labelledby"?: string };
const isNamed = (p: Named) => p["aria-label"] !== undefined || p["aria-labelledby"] !== undefined;

/// [`Field`] の見出しを名前にする 1 行の入力。
export function FieldInput(props: React.ComponentProps<typeof Input>) {
  const a11y = useFieldControl(isNamed(props));
  return <Input {...a11y} {...props} />;
}

/// [`Field`] の見出しを名前にする複数行の入力。
export function FieldTextarea(props: React.ComponentProps<typeof Textarea>) {
  const a11y = useFieldControl(isNamed(props));
  return <Textarea {...a11y} {...props} />;
}

/// [`Field`] の見出しを名前にするブラウザの選択の欄（`<select>`）。
export function FieldNativeSelect(props: React.ComponentProps<"select">) {
  const a11y = useFieldControl(isNamed(props));
  return <select {...a11y} {...props} />;
}

/// [`Field`] の見出しを名前にする選択の欄（選んだ値はボタンの中の文字として読める）。
export function FieldSelectTrigger(props: React.ComponentProps<typeof SelectTrigger>) {
  const a11y = useFieldControl(isNamed(props));
  return <SelectTrigger {...a11y} {...props} />;
}

export function NumberInput({
  value,
  onChange,
  min,
  max,
  placeholder,
  error,
}: {
  value: number | null | undefined;
  onChange: (next: number | null) => void;
  min?: number;
  max?: number;
  placeholder?: string;
  error?: boolean;
}) {
  const a11y = useFieldControl();
  return (
    <Input
      type="number"
      inputMode="numeric"
      {...a11y}
      value={value ?? ""}
      min={min}
      max={max}
      placeholder={placeholder}
      onChange={(e) => {
        const raw = e.target.value;
        if (raw === "") return onChange(null);
        const n = Number(raw);
        if (Number.isFinite(n)) onChange(n);
      }}
      className={cn("h-8", error && "border-destructive")}
    />
  );
}

/// 秒を「N 秒」か「N 分」で言う（1 分未満だけ秒・付録 C の P11）。
function period(sec: number): string {
  const m = Math.floor(sec / 60);
  const s = sec % 60;
  if (m === 0) return `${s} 秒`;
  return s === 0 ? `${m.toLocaleString("ja-JP")} 分` : `${m.toLocaleString("ja-JP")} 分 ${s} 秒`;
}

/// 時間の上限の注記（数はカタログの timeout_default_sec と timeout_max_sec から・付録 D の N2.6）。
function timeoutHint(defaultSec: number, maxSec: number, agent: boolean): string {
  const def = defaultSec >= 60 ? `${defaultSec.toLocaleString("ja-JP")} 秒（${period(defaultSec)}）` : `${defaultSec} 秒`;
  const agentNote = agent ? "「AI の作業時間の上限」より短くはできません。" : "";
  return `空欄なら ${def}で、最大 ${maxSec.toLocaleString("ja-JP")} 秒です。${agentNote}過ぎると、このブロックは失敗します。`;
}

/// 共通セクション（全ノード種）。
export function CommonSection({
  node,
  dispatch,
  attrErrors,
}: {
  node: IrNode;
  dispatch: React.Dispatch<EditorAction>;
  /// step 属性の欄の誤り（`timeout_sec` → 文言・保存時の照合・BUG-96）。
  attrErrors?: Map<string, string>;
}) {
  const entry = NODE_CATALOG.find((c) => c.type === node.type);
  const [idDraft, setIdDraft] = React.useState(node.id);
  React.useEffect(() => setIdDraft(node.id), [node.id]);
  const timeoutError = attrErrors?.get("timeout_sec") ?? null;

  return (
    <div className="space-y-3">
      <Field label="ブロック名" hint="キャンバスに表示される名前">
        <FieldInput
          value={node.label ?? ""}
          placeholder={entry?.label_ja}
          onChange={(e) =>
            dispatch({
              type: "update_node",
              id: node.id,
              patch: { label: e.target.value || null } as Partial<IrNode>,
            })
          }
          className="h-8"
        />
      </Field>
      <Field
        label="ID"
        hint="他のブロックから参照するときの名前（小文字・数字・_）"
      >
        <FieldInput
          value={idDraft}
          onChange={(e) => setIdDraft(e.target.value)}
          onBlur={() => {
            if (idDraft !== node.id) {
              dispatch({ type: "rename_node", id: node.id, nextId: idDraft });
              setIdDraft(node.id); // 不正なら reducer が無視 → 元に戻す（成功時は selection 経由で追随）。
            }
          }}
          className="h-8 font-mono text-xs max-md:text-base"
        />
      </Field>
      <div className="grid grid-cols-2 gap-3">
        <Field label="リトライ回数" hint="失敗時にやり直す回数">
          <NumberInput
            value={node.retry?.max_attempts ?? 1}
            min={1}
            max={10}
            onChange={(n) =>
              dispatch({
                type: "update_node",
                id: node.id,
                patch: {
                  retry: {
                    ...(node.retry ?? {
                      backoff: { base_sec: 2, max_sec: 300, jitter: true },
                    }),
                    max_attempts: n ?? 1,
                  },
                } as Partial<IrNode>,
              })
            }
          />
        </Field>
        {/* 時間の上限（ノードの timeout_sec）。エンジンが打ち切るようになったので出す（BUG-96・付録 D の N2.6）。
            打ち切らない種（流れの制御など・カタログに既定が無い）では出さない。 */}
        {entry?.timeout_default_sec && entry.timeout_max_sec ? (
          <Field
            label="時間の上限（秒・省略可）"
            hint={timeoutHint(entry.timeout_default_sec, entry.timeout_max_sec, node.type === "agent.invoke")}
            error={timeoutError}
          >
            <NumberInput
              value={node.timeout_sec ?? null}
              min={1}
              max={entry.timeout_max_sec}
              error={Boolean(timeoutError)}
              onChange={(n) =>
                dispatch({
                  type: "update_node",
                  id: node.id,
                  patch: { timeout_sec: n } as Partial<IrNode>,
                })
              }
            />
          </Field>
        ) : null}
      </div>
      <Field
        label="失敗したとき"
        hint={
          node.on_error === "continue"
            ? "「エラー時」の出口が増え、そちらに流れます"
            : "このブロックが失敗するとフロー全体が失敗します"
        }
      >
        <Select
          value={node.on_error ?? "fail_run"}
          onValueChange={(v) =>
            dispatch({
              type: "update_node",
              id: node.id,
              patch: { on_error: v } as Partial<IrNode>,
            })
          }
        >
          <FieldSelectTrigger className="h-8">
            <SelectValue />
          </FieldSelectTrigger>
          <SelectContent>
            <SelectItem value="fail_run">フローを失敗にする</SelectItem>
            <SelectItem value="continue">エラー用の出口に流す</SelectItem>
          </SelectContent>
        </Select>
      </Field>
    </div>
  );
}
