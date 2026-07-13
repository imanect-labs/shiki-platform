"use client";

/// リッチ入力フォーム部品（PR3）。ネイティブ input を四季トークンで装飾し、
/// 各コンポーネントは内部で構造化状態を持ち、送信用に**人間可読な文字列**を親へ返す
/// （backend は「ラベル: 値」で整形するため値はラベル/読みやすい表現にする）。

import * as React from "react";

import { Star } from "lucide-react";

import type {
  CheckboxGroupProps,
  DateProps,
  RadioGroupProps,
  RatingProps,
  SelectProps,
  SliderProps,
} from "@/generated/gui-spec";
import { cn } from "@/lib/utils";

const OTHER = "__other__";

/// 既定値を一度だけ親へ反映する（未操作でも default が送信される）。
function useInitialEmit(onChange: (v: string) => void, initial: string) {
  const done = React.useRef(false);
  React.useEffect(() => {
    if (!done.current && initial) {
      done.current = true;
      onChange(initial);
    }
    // マウント時に一度だけ（onChange は毎レンダー再生成されるため依存に含めない）。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
}

function FieldLabel({ id, label, required }: { id?: string; label: string; required?: boolean }) {
  return (
    <label htmlFor={id} className="block text-xs font-medium text-foreground/70">
      {label}
      {required ? <span className="ml-0.5 text-destructive">*</span> : null}
    </label>
  );
}

/// 「その他」自由記述欄。
function OtherInput({
  value,
  onChange,
  disabled,
}: {
  value: string;
  onChange: (v: string) => void;
  disabled: boolean;
}) {
  return (
    <input
      type="text"
      value={value}
      onChange={(e) => onChange(e.target.value)}
      placeholder="その他（自由記述）"
      disabled={disabled}
      className="mt-1 h-8 w-full rounded-lg border border-input bg-background px-2.5 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"
    />
  );
}

export function SelectField({
  field,
  onChange,
  disabled,
}: {
  field: SelectProps;
  onChange: (v: string) => void;
  disabled: boolean;
}) {
  const id = `genui-field-${field.id}`;
  const options = field.options ?? [];
  const [choice, setChoice] = React.useState<string>(field.default ?? "");
  const [other, setOther] = React.useState("");
  useInitialEmit(
    onChange,
    field.default ? (options.find((o) => o.value === field.default)?.label ?? field.default) : "",
  );

  const pick = (v: string, otherText: string) => {
    setChoice(v);
    if (v === OTHER) onChange(otherText.trim());
    else onChange(options.find((o) => o.value === v)?.label ?? v);
  };

  return (
    <div className="space-y-1.5">
      <FieldLabel id={id} label={field.label} required={field.required} />
      <select
        id={id}
        value={choice}
        onChange={(e) => pick(e.target.value, other)}
        required={field.required}
        disabled={disabled}
        className="h-9 w-full rounded-lg border border-input bg-background px-3 text-sm text-foreground shadow-sm transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"
      >
        {!field.required && !field.default ? <option value="">選択してください</option> : null}
        {options.map((opt) => (
          <option key={opt.value} value={opt.value}>
            {opt.label}
          </option>
        ))}
        {field.allow_other ? <option value={OTHER}>その他</option> : null}
      </select>
      {field.allow_other && choice === OTHER ? (
        <OtherInput
          value={other}
          onChange={(v) => {
            setOther(v);
            onChange(v.trim());
          }}
          disabled={disabled}
        />
      ) : null}
    </div>
  );
}

export function CheckboxField({
  field,
  onChange,
  disabled,
}: {
  field: CheckboxGroupProps;
  onChange: (v: string) => void;
  disabled: boolean;
}) {
  const options = React.useMemo(() => field.options ?? [], [field.options]);
  const [selected, setSelected] = React.useState<Set<string>>(
    () => new Set(field.default ?? []),
  );
  const [other, setOther] = React.useState("");
  const [otherOn, setOtherOn] = React.useState(false);
  useInitialEmit(
    onChange,
    options
      .filter((o) => (field.default ?? []).includes(o.value))
      .map((o) => o.label)
      .join(", "),
  );

  const emit = React.useCallback(
    (sel: Set<string>, otherText: string, useOther: boolean) => {
      const labels = options.filter((o) => sel.has(o.value)).map((o) => o.label);
      if (useOther && otherText.trim()) labels.push(otherText.trim());
      onChange(labels.join(", "));
    },
    [options, onChange],
  );

  const toggle = (value: string) => {
    const next = new Set(selected);
    if (next.has(value)) next.delete(value);
    else next.add(value);
    setSelected(next);
    emit(next, other, otherOn);
  };

  return (
    <fieldset className="space-y-1.5">
      <legend className="text-xs font-medium text-foreground/70">
        {field.label}
        {field.required ? <span className="ml-0.5 text-destructive">*</span> : null}
      </legend>
      <div className="flex flex-col gap-1.5">
        {options.map((opt) => (
          <label key={opt.value} className="flex items-center gap-2 text-[13px] text-foreground/90">
            <input
              type="checkbox"
              checked={selected.has(opt.value)}
              onChange={() => toggle(opt.value)}
              disabled={disabled}
              className="size-4 rounded border-input accent-[var(--primary)]"
            />
            {opt.label}
          </label>
        ))}
        {field.allow_other ? (
          <label className="flex items-center gap-2 text-[13px] text-foreground/90">
            <input
              type="checkbox"
              checked={otherOn}
              onChange={() => {
                const on = !otherOn;
                setOtherOn(on);
                emit(selected, other, on);
              }}
              disabled={disabled}
              className="size-4 rounded border-input accent-[var(--primary)]"
            />
            その他
          </label>
        ) : null}
      </div>
      {field.allow_other && otherOn ? (
        <OtherInput
          value={other}
          onChange={(v) => {
            setOther(v);
            emit(selected, v, true);
          }}
          disabled={disabled}
        />
      ) : null}
    </fieldset>
  );
}

export function RadioField({
  field,
  onChange,
  disabled,
}: {
  field: RadioGroupProps;
  onChange: (v: string) => void;
  disabled: boolean;
}) {
  const options = field.options ?? [];
  const [value, setValue] = React.useState<string>(field.default ?? "");
  const [other, setOther] = React.useState("");
  useInitialEmit(
    onChange,
    field.default ? (options.find((o) => o.value === field.default)?.label ?? field.default) : "",
  );

  const pick = (v: string, otherText: string) => {
    setValue(v);
    if (v === OTHER) onChange(otherText.trim());
    else onChange(options.find((o) => o.value === v)?.label ?? v);
  };

  return (
    <fieldset className="space-y-1.5">
      <legend className="text-xs font-medium text-foreground/70">
        {field.label}
        {field.required ? <span className="ml-0.5 text-destructive">*</span> : null}
      </legend>
      <div className="flex flex-col gap-1.5">
        {options.map((opt) => (
          <label key={opt.value} className="flex items-center gap-2 text-[13px] text-foreground/90">
            <input
              type="radio"
              name={`genui-radio-${field.id}`}
              checked={value === opt.value}
              onChange={() => pick(opt.value, other)}
              disabled={disabled}
              className="size-4 accent-[var(--primary)]"
            />
            {opt.label}
          </label>
        ))}
        {field.allow_other ? (
          <label className="flex items-center gap-2 text-[13px] text-foreground/90">
            <input
              type="radio"
              name={`genui-radio-${field.id}`}
              checked={value === OTHER}
              onChange={() => pick(OTHER, other)}
              disabled={disabled}
              className="size-4 accent-[var(--primary)]"
            />
            その他
          </label>
        ) : null}
      </div>
      {field.allow_other && value === OTHER ? (
        <OtherInput
          value={other}
          onChange={(v) => {
            setOther(v);
            onChange(v.trim());
          }}
          disabled={disabled}
        />
      ) : null}
    </fieldset>
  );
}

export function DateField({
  field,
  onChange,
  disabled,
}: {
  field: DateProps;
  onChange: (v: string) => void;
  disabled: boolean;
}) {
  const id = `genui-field-${field.id}`;
  const [start, setStart] = React.useState(field.default ?? "");
  const [end, setEnd] = React.useState("");
  useInitialEmit(onChange, field.range ? "" : (field.default ?? ""));
  const inputCls =
    "h-9 rounded-lg border border-input bg-background px-3 text-sm text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50";
  return (
    <div className="space-y-1.5">
      <FieldLabel id={id} label={field.label} required={field.required} />
      {field.range ? (
        <div className="flex items-center gap-2">
          <input
            id={id}
            type="date"
            value={start}
            min={field.min ?? undefined}
            max={field.max ?? undefined}
            onChange={(e) => {
              setStart(e.target.value);
              onChange(`${e.target.value} 〜 ${end}`);
            }}
            disabled={disabled}
            className={inputCls}
          />
          <span className="text-muted-foreground">〜</span>
          <input
            type="date"
            value={end}
            min={start || (field.min ?? undefined)}
            max={field.max ?? undefined}
            onChange={(e) => {
              setEnd(e.target.value);
              onChange(`${start} 〜 ${e.target.value}`);
            }}
            disabled={disabled}
            className={inputCls}
          />
        </div>
      ) : (
        <input
          id={id}
          type="date"
          value={start}
          min={field.min ?? undefined}
          max={field.max ?? undefined}
          onChange={(e) => {
            setStart(e.target.value);
            onChange(e.target.value);
          }}
          disabled={disabled}
          className={inputCls}
        />
      )}
    </div>
  );
}

export function SliderField({
  field,
  onChange,
  disabled,
}: {
  field: SliderProps;
  onChange: (v: string) => void;
  disabled: boolean;
}) {
  const id = `genui-field-${field.id}`;
  const [value, setValue] = React.useState<number>(field.default ?? field.min);
  useInitialEmit(onChange, String(field.default ?? field.min));
  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between">
        <FieldLabel id={id} label={field.label} />
        <span className="text-[13px] font-medium tabular-nums text-foreground">{value}</span>
      </div>
      <input
        id={id}
        type="range"
        min={field.min}
        max={field.max}
        step={field.step ?? undefined}
        value={value}
        onChange={(e) => {
          const n = Number(e.target.value);
          setValue(n);
          onChange(String(n));
        }}
        disabled={disabled}
        className="h-2 w-full cursor-pointer accent-[var(--primary)]"
      />
    </div>
  );
}

export function RatingField({
  field,
  onChange,
  disabled,
}: {
  field: RatingProps;
  onChange: (v: string) => void;
  disabled: boolean;
}) {
  const max = field.max ?? 5;
  const [value, setValue] = React.useState<number>(field.default ?? 0);
  useInitialEmit(onChange, field.default ? String(field.default) : "");
  return (
    <div className="space-y-1.5">
      <FieldLabel label={field.label} required={field.required} />
      <div className="flex items-center gap-1" role="radiogroup" aria-label={field.label}>
        {Array.from({ length: max }, (_, i) => i + 1).map((n) => (
          <button
            key={n}
            type="button"
            role="radio"
            aria-checked={value === n}
            aria-label={`${n}`}
            disabled={disabled}
            onClick={() => {
              setValue(n);
              onChange(String(n));
            }}
            className="text-[var(--season-autumn)] transition-transform hover:scale-110 disabled:opacity-50"
          >
            <Star className={cn("size-5", n <= value ? "fill-current" : "fill-none opacity-40")} />
          </button>
        ))}
      </div>
    </div>
  );
}
