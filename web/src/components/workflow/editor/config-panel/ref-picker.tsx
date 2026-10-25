"use client";

/// 参照先の選択と「差し込む」メニュー（値の入力欄の部品・BUG-1）。
///
/// - `RefPicker`: 候補（入力項目・前のブロックの結果・繰り返し中の要素）から選ぶ。候補に無い場所は
///   「場所を指定する…」で参照元と path（候補つき）を手で書ける。
/// - `InsertMenu`: 候補を選ぶと、呼び出し側がカーソル位置に `{名前}` を差し込む。

import * as React from "react";
import { Braces } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectSeparator,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  pathHints,
  refKey,
  refOptions,
  sourceOptions,
  type RefContext,
  type RefOption,
  type RefValue,
} from "./value-refs";

const CUSTOM = "__custom";

/// 候補を見出しごとにまとめる（並びは候補の順）。
function grouped(options: RefOption[]): { group: string; options: RefOption[] }[] {
  const out: { group: string; options: RefOption[] }[] = [];
  for (const o of options) {
    const g = out.find((x) => x.group === o.group);
    if (g) g.options.push(o);
    else out.push({ group: o.group, options: [o] });
  }
  return out;
}

/// 参照先の選択。`value` が null なら未選択（「選んでください」）。
export function RefPicker({
  value,
  onChange,
  ctx,
  ariaLabel = "参照先",
}: {
  value: RefValue | null;
  onChange: (next: RefValue) => void;
  ctx: RefContext;
  ariaLabel?: string;
}) {
  const options = React.useMemo(() => refOptions(ctx), [ctx]);
  // 既定値（default）付きの参照は候補と同じにしない（選び直すと既定値が消えたように見えるため手書きの側で見せる）。
  const match =
    value && value.default === undefined ? options.find((o) => o.key === refKey(value)) : undefined;
  // 候補に無い参照（手で書いた場所）は手書きの欄で見せる。手書きを選んだ後は候補と同じ値になっても欄を残す。
  const [forceCustom, setForceCustom] = React.useState(false);
  const custom = forceCustom || (value !== null && !match);
  const listId = React.useId();
  const keepDefault = value?.default !== undefined ? { default: value.default } : {};
  const hints = custom ? pathHints(value?.$from ?? "input", ctx) : [];

  return (
    <div className="space-y-1.5">
      <Select
        value={custom ? CUSTOM : (match?.key ?? "")}
        onValueChange={(k) => {
          if (k === CUSTOM) {
            setForceCustom(true);
            return;
          }
          const o = options.find((x) => x.key === k);
          if (!o) return;
          setForceCustom(false);
          onChange({ ...o.ref, ...keepDefault });
        }}
      >
        <SelectTrigger className="h-8 text-xs" aria-label={ariaLabel}>
          <SelectValue placeholder="参照先を選んでください" />
        </SelectTrigger>
        <SelectContent>
          {grouped(options).map((g) => (
            <SelectGroup key={g.group}>
              <SelectLabel>{g.group}</SelectLabel>
              {g.options.map((o) => (
                <SelectItem key={o.key} value={o.key}>
                  {o.label}
                </SelectItem>
              ))}
            </SelectGroup>
          ))}
          <SelectSeparator />
          <SelectItem value={CUSTOM}>場所を指定する…</SelectItem>
        </SelectContent>
      </Select>
      {custom ? (
        <div className="flex items-center gap-1.5">
          <Select
            value={value?.$from ?? ""}
            onValueChange={(v) => onChange({ $from: v, ...(value?.path ? { path: value.path } : {}), ...keepDefault })}
          >
            <SelectTrigger className="h-8 min-w-0 flex-1 text-xs" aria-label="参照元">
              <SelectValue placeholder="参照元" />
            </SelectTrigger>
            <SelectContent>
              {sourceOptions(ctx).map((s) => (
                <SelectItem key={s.value} value={s.value}>
                  {s.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Input
            value={value?.path ?? ""}
            list={hints.length > 0 ? listId : undefined}
            onChange={(e) =>
              onChange({
                $from: value?.$from ?? "input",
                ...(e.target.value ? { path: e.target.value } : {}),
                ...keepDefault,
              })
            }
            placeholder="/title（空で全体）"
            aria-label="取り出す場所"
            className="h-8 min-w-0 flex-1 font-mono text-[11px] md:text-[11px] max-md:text-base"
          />
          {hints.length > 0 ? (
            <datalist id={listId}>
              {hints.map((h) => (
                <option key={h} value={h} />
              ))}
            </datalist>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

/// 「差し込む」メニュー。選ぶと `onPick` に参照を渡す（差し込む位置・名前は呼び出し側が決める）。
export function InsertMenu({
  ctx,
  onPick,
  onClosed,
}: {
  ctx: RefContext;
  onPick: (ref: RefValue) => void;
  /// 閉じた後にフォーカスを戻す先（入力欄）へ戻す。
  onClosed: () => void;
}) {
  const options = React.useMemo(() => refOptions(ctx), [ctx]);
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          size="sm"
          className="h-6 gap-1 px-1.5 text-[11px] text-primary pointer-coarse:h-8"
          data-testid="insert-var"
        >
          <Braces className="size-3.5" aria-hidden />
          差し込む
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="end"
        className="max-h-[min(20rem,var(--radix-dropdown-menu-content-available-height))] w-64 overflow-y-auto"
        onCloseAutoFocus={(e) => {
          // 開いたボタンではなく入力欄へ戻す（続けて書けるように）。
          e.preventDefault();
          onClosed();
        }}
      >
        {grouped(options).map((g) => (
          <DropdownMenuGroup key={g.group}>
            <DropdownMenuLabel>{g.group}</DropdownMenuLabel>
            {g.options.map((o) => (
              <DropdownMenuItem key={o.key} onSelect={() => onPick(o.ref)} className="text-[13px]">
                <span className="min-w-0 truncate">{o.label}</span>
              </DropdownMenuItem>
            ))}
          </DropdownMenuGroup>
        ))}
        {ctx.inputFields.length === 0 ? (
          <p className="px-2 pb-1.5 pt-1 text-[11px] leading-relaxed text-muted-foreground">
            実行するときに入れてもらう値（入力項目）は、「きっかけ」の設定で追加できます
          </p>
        ) : null}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
