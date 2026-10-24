"use client";

import * as React from "react";
import { ChevronDown, Search, X } from "lucide-react";

import { cn } from "@/lib/utils";
import type { AuditDecision, AuditQuery } from "@/lib/audit-api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { SegmentedControl } from "@/components/ui/segmented-control";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { addDays, todayJst } from "../format";
import { defaultRange } from "./audit-query";
import { AUDIT_CATEGORIES, actionOptions, type AuditCategory } from "./audit-vocab";

const PRESETS = [
  { value: "1", label: "今日" },
  { value: "7", label: "7日" },
  { value: "30", label: "30日" },
  { value: "90", label: "90日" },
];

const ANY = "__any__";

/// 期間がプリセットのどれに当たるか（今日で終わる期間だけ）。
function presetOf(q: AuditQuery): string {
  if (q.to !== todayJst()) return "";
  return PRESETS.find((p) => addDays(q.to, -(Number(p.value) - 1)) === q.from)?.value ?? "";
}

function Field({ label, htmlFor, children, className }: {
  label: string;
  htmlFor: string;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <div className={cn("flex min-w-0 flex-col gap-1", className)}>
      <label htmlFor={htmlFor} className="text-xs text-muted-foreground">
        {label}
      </label>
      {children}
    </div>
  );
}

/// 監査ログの絞り込み。入力は手元に持ち、「絞り込む」で URL に反映する（打つたびに検索しない）。
export function AuditFilters({ query, onApply }: { query: AuditQuery; onApply: (q: AuditQuery) => void }) {
  const [draft, setDraft] = React.useState<AuditQuery>(query);
  React.useEffect(() => setDraft(query), [query]);
  const advancedSet = Boolean(query.object_type || query.object_id || query.chunk_id || query.trace_id);
  const [advanced, setAdvanced] = React.useState(advancedSet);
  React.useEffect(() => {
    if (advancedSet) setAdvanced(true);
  }, [advancedSet]);

  const set = (p: Partial<AuditQuery>) => setDraft((d) => ({ ...d, ...p }));
  const rangeError = draft.from > draft.to ? "開始日が終了日より後になっています" : null;
  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    if (!rangeError) onApply(draft);
  };
  const dirty = JSON.stringify(draft) !== JSON.stringify(query);
  const conditionCount = [
    query.actor,
    query.category,
    query.action,
    query.decision,
    query.object_type,
    query.object_id,
    query.chunk_id,
    query.trace_id,
  ].filter(Boolean).length;
  const hasConditions = conditionCount > 0;
  // スマホでは期間だけを出し、ほかの条件は開いたときだけ（一覧を先に見せる）。PC は常に出す。
  const [mobileOpen, setMobileOpen] = React.useState(false);
  const mobileHidden = !mobileOpen && "max-md:hidden";

  return (
    <form
      onSubmit={submit}
      className="rule-soft flex flex-col gap-3 rounded-xl border bg-card p-3 md:p-4"
      aria-label="監査ログの絞り込み"
    >
      <div className="flex flex-wrap items-end gap-x-3 gap-y-2">
        <div className="flex min-w-0 flex-col gap-1 max-md:w-full">
          <span className="text-xs text-muted-foreground" aria-hidden>
            期間
          </span>
          <SegmentedControl
            aria-label="期間"
            size="sm"
            options={PRESETS}
            value={presetOf(draft)}
            onValueChange={(v) => {
              const to = todayJst();
              onApply({ ...draft, from: addDays(to, -(Number(v) - 1)), to });
            }}
            className="max-md:w-full max-md:[&>*]:flex-1"
          />
        </div>
        <button
          type="button"
          onClick={() => setMobileOpen((v) => !v)}
          aria-expanded={mobileOpen}
          aria-controls="audit-conditions"
          className="inline-flex min-h-11 w-full items-center justify-between rounded-md text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring md:hidden"
        >
          <span>
            条件で絞り込む
            {hasConditions ? (
              <span className="ml-1.5 rounded-full bg-primary px-1.5 py-0.5 text-[11px] font-medium text-primary-foreground">
                {conditionCount}
              </span>
            ) : null}
          </span>
          <ChevronDown className={cn("size-4 transition-transform", mobileOpen && "rotate-180")} aria-hidden />
        </button>
        <div id="audit-conditions" className={cn("flex items-end gap-1.5 max-md:w-full", mobileHidden)}>
          <Field label="開始日" htmlFor="audit-from" className="max-md:flex-1">
            <Input
              id="audit-from"
              type="date"
              value={draft.from}
              max={todayJst()}
              onChange={(e) => set({ from: e.target.value || defaultRange().from })}
              aria-invalid={rangeError ? true : undefined}
              className="h-8 md:w-[9.5rem]"
            />
          </Field>
          <span className="pb-1.5 text-muted-foreground" aria-hidden>
            〜
          </span>
          <Field label="終了日" htmlFor="audit-to" className="max-md:flex-1">
            <Input
              id="audit-to"
              type="date"
              value={draft.to}
              max={todayJst()}
              onChange={(e) => set({ to: e.target.value || todayJst() })}
              aria-invalid={rangeError ? true : undefined}
              className="h-8 md:w-[9.5rem]"
            />
          </Field>
        </div>
      </div>
      <div
        className={cn(
          "grid grid-cols-2 gap-x-3 gap-y-2 md:grid-cols-[minmax(0,1fr)_9.5rem_minmax(0,1fr)_8rem]",
          mobileHidden,
        )}
      >
        {/* スマホは 人 / 分類・判定 / 操作 の順、PC は 人・分類・操作・判定 の順に並べる。 */}
        <Field label="操作した人" htmlFor="audit-actor" className="col-span-2 md:order-1 md:col-span-1">
          <Input
            id="audit-actor"
            value={draft.actor ?? ""}
            onChange={(e) => set({ actor: e.target.value })}
            placeholder="メールアドレスかユーザー ID"
            autoComplete="off"
            className="h-8"
          />
        </Field>
        <Field label="分類" htmlFor="audit-category" className="md:order-2">
          <Select
            value={draft.category ?? ANY}
            onValueChange={(v) => set({ category: v === ANY ? undefined : (v as AuditCategory) })}
          >
            <SelectTrigger id="audit-category" className="h-8 w-full text-[13px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={ANY}>すべて</SelectItem>
              {AUDIT_CATEGORIES.map((c) => (
                <SelectItem key={c.id} value={c.id}>
                  {c.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </Field>
        <Field label="判定" htmlFor="audit-decision" className="md:order-4">
          <Select
            value={draft.decision ?? ANY}
            onValueChange={(v) => set({ decision: v === ANY ? undefined : (v as AuditDecision) })}
          >
            <SelectTrigger id="audit-decision" className="h-8 w-full text-[13px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={ANY}>すべて</SelectItem>
              <SelectItem value="allow">許可</SelectItem>
              <SelectItem value="deny">拒否</SelectItem>
            </SelectContent>
          </Select>
        </Field>
        <Field label="操作の種類" htmlFor="audit-action" className="col-span-2 md:order-3 md:col-span-1">
          <Input
            id="audit-action"
            list="audit-action-hints"
            value={draft.action ?? ""}
            onChange={(e) => set({ action: e.target.value })}
            placeholder="例: file.delete・file.（前方一致）"
            autoComplete="off"
            className="h-8 font-mono"
          />
          <datalist id="audit-action-hints">
            {actionOptions(draft.category).map((a) => (
              <option key={a.action} value={a.action}>
                {a.label}
              </option>
            ))}
          </datalist>
        </Field>
      </div>
      {advanced ? (
        <div className={cn("grid grid-cols-2 gap-x-3 gap-y-2 md:grid-cols-4", mobileHidden)}>
          <Field label="対象の種類" htmlFor="audit-object-type">
            <Input
              id="audit-object-type"
              value={draft.object_type ?? ""}
              onChange={(e) => set({ object_type: e.target.value })}
              placeholder="例: file"
              autoComplete="off"
              className="h-8 font-mono"
            />
          </Field>
          <Field label="対象の ID" htmlFor="audit-object-id">
            <Input
              id="audit-object-id"
              value={draft.object_id ?? ""}
              onChange={(e) => set({ object_id: e.target.value })}
              autoComplete="off"
              className="h-8 font-mono"
            />
          </Field>
          <Field label="引用されたチャンク ID" htmlFor="audit-chunk">
            <Input
              id="audit-chunk"
              value={draft.chunk_id ?? ""}
              onChange={(e) => set({ chunk_id: e.target.value })}
              autoComplete="off"
              className="h-8 font-mono"
            />
          </Field>
          <Field label="trace_id" htmlFor="audit-trace">
            <Input
              id="audit-trace"
              value={draft.trace_id ?? ""}
              onChange={(e) => set({ trace_id: e.target.value })}
              autoComplete="off"
              className="h-8 font-mono"
            />
          </Field>
        </div>
      ) : null}
      <div className={cn("flex flex-wrap items-center gap-2", mobileHidden)}>
        <button
          type="button"
          onClick={() => setAdvanced((v) => !v)}
          aria-expanded={advanced}
          className="inline-flex items-center gap-1 rounded-md text-xs text-muted-foreground outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring pointer-coarse:min-h-11"
        >
          <ChevronDown className={cn("size-3.5 transition-transform", advanced && "rotate-180")} aria-hidden />
          詳しい条件（対象・引用チャンク・trace_id）
        </button>
        {rangeError ? (
          <span role="alert" className="text-xs text-destructive">
            {rangeError}
          </span>
        ) : null}
        <span className="flex-1" />
        {hasConditions ? (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            onClick={() => onApply({ from: draft.from, to: draft.to })}
          >
            <X aria-hidden />
            条件をクリア
          </Button>
        ) : null}
        <Button type="submit" size="sm" disabled={Boolean(rangeError) || !dirty}>
          <Search aria-hidden />
          絞り込む
        </Button>
      </div>
    </form>
  );
}
