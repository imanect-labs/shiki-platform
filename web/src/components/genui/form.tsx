"use client";

/// generative UI のフォーム（Task 6.6）。
///
/// 送信は宣言済みアクション（`form.submit.action`）への dispatch のみ。
/// 値は `{ fieldId: string }` の object として送る（サーバ側が束縛に照合して実行）。

import * as React from "react";

import { CheckCircle2, Loader2 } from "lucide-react";

import type { FormField, FormProps } from "@/generated/gui-spec";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { useGenUiAction } from "./action-context";
import { ActionResultNote, describeActionError } from "./action-result";
import {
  CheckboxField,
  DateField,
  RadioField,
  RatingField,
  SelectField,
  SliderField,
} from "./form-fields";

export function GenUiForm({ form }: { form: FormProps }) {
  const { dispatch, onActionCompleted } = useGenUiAction();
  const [values, setValues] = React.useState<Record<string, string>>(() => {
    const init: Record<string, string> = {};
    // text_input のみ親が値を持つ（他フィールドは自前で状態管理し onChange で反映）。
    for (const f of form.fields ?? []) {
      init[f.id] = f.component === "text_input" && typeof f.default === "string" ? f.default : "";
    }
    return init;
  });
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const [doneNote, setDoneNote] = React.useState<string | null>(null);

  const setValue = (id: string, v: string) => setValues((prev) => ({ ...prev, [id]: v }));

  const onSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (busy) return;
    setBusy(true);
    setError(null);
    setDoneNote(null);
    try {
      const result = await dispatch(form.submit.action, values);
      setDoneNote("送信しました");
      onActionCompleted?.(result);
    } catch (err) {
      setError(describeActionError(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form onSubmit={onSubmit} className="min-w-0 space-y-3" data-testid={`genui-form-${form.id}`}>
      {form.title ? (
        <h3 className="text-[13px] font-semibold tracking-wide text-foreground/80">{form.title}</h3>
      ) : null}
      {(form.fields ?? []).map((field) => (
        <FieldView
          key={field.id}
          field={field}
          value={values[field.id] ?? ""}
          onChange={(v) => setValue(field.id, v)}
          disabled={busy}
        />
      ))}
      <div className="flex items-center gap-3 pt-1">
        <Button type="submit" size="sm" disabled={busy}>
          {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
          {form.submit_label || "送信"}
        </Button>
        {doneNote ? (
          <span className="inline-flex items-center gap-1 text-xs text-primary">
            <CheckCircle2 className="size-3.5" aria-hidden />
            {doneNote}
          </span>
        ) : null}
      </div>
      <ActionResultNote error={error} />
    </form>
  );
}

function FieldView({
  field,
  value,
  onChange,
  disabled,
}: {
  field: FormField;
  value: string;
  onChange: (v: string) => void;
  disabled: boolean;
}) {
  const id = `genui-field-${field.id}`;
  switch (field.component) {
    case "text_input":
      return (
        <div className="space-y-1.5">
          <label htmlFor={id} className="block text-xs font-medium text-foreground/70">
            {field.label}
            {field.required ? <span className="ml-0.5 text-destructive">*</span> : null}
          </label>
          {field.multiline ? (
            <Textarea
              id={id}
              value={value}
              onChange={(e) => onChange(e.target.value)}
              placeholder={field.placeholder ?? undefined}
              required={field.required}
              disabled={disabled}
              rows={3}
            />
          ) : (
            <Input
              id={id}
              value={value}
              onChange={(e) => onChange(e.target.value)}
              placeholder={field.placeholder ?? undefined}
              required={field.required}
              disabled={disabled}
            />
          )}
        </div>
      );
    case "select":
      return <SelectField field={field} onChange={onChange} disabled={disabled} />;
    case "checkbox":
      return <CheckboxField field={field} onChange={onChange} disabled={disabled} />;
    case "radio":
      return <RadioField field={field} onChange={onChange} disabled={disabled} />;
    case "date":
      return <DateField field={field} onChange={onChange} disabled={disabled} />;
    case "slider":
      return <SliderField field={field} onChange={onChange} disabled={disabled} />;
    case "rating":
      return <RatingField field={field} onChange={onChange} disabled={disabled} />;
    default:
      return null; // 未知フィールドは黙って落とす（クラッシュさせない）
  }
}
