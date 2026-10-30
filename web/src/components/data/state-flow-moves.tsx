"use client";

/// 「状態の進め方」の画面の進め方の表（GAP-44）: どの状態からどの状態へ、誰が進められるか。
///
/// PC は 1 行 1 つの表、スマホ（640px 未満）は 1 つずつのカードにする。進められる人は「どれか 1 つに当てはまる人」で、
/// 「申請した本人は除く」は申請者の列（メンバーの列）があるときだけ使える（D-115）。

import { ArrowRight, Plus, Trash2, X } from "lucide-react";
import * as React from "react";

import { RefPicker } from "@/components/data/ref-picker";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import type { FieldDef, PendingApprovalMove } from "@/lib/data-api";
import { cachedRoleName, rememberRoleName, resolveRoleNames } from "@/lib/role-names";
import type { ActorModel, FlowDraft, MoveDraft, Who } from "@/lib/state-flow";
import { cn } from "@/lib/utils";

const label = (f: FieldDef) => f.display_name || f.key;

/// 進められる人 1 つの名前（チップと追加のメニューで同じ語）。
export function whoLabel(w: Who, fields: FieldDef[]): string {
  const f = (k: string) => fields.find((x) => x.key === k);
  switch (w.kind) {
    case "editors":
      return "テーブルを編集できる人";
    case "creator":
      return "レコードを追加した人";
    case "user_field": {
      const x = f(w.field);
      return x ? `「${label(x)}」の人` : "見つからない列の人";
    }
    case "role_field": {
      const x = f(w.field);
      return x ? `「${label(x)}」の部署・グループの人` : "見つからない列の人";
    }
    case "role":
      return w.role ? `部署・グループ「${cachedRoleName(w.role) || w.role}」の人` : "部署・グループ";
  }
}

const sameWho = (a: Who, b: Who) => JSON.stringify(a) === JSON.stringify(b);

/// 部署・グループを探して選ぶ（ディレクトリの部署・`role_ref` の列と同じ探し方）。
const ROLE_FIELD: FieldDef = { key: "role", display_name: "部署・グループ", type: "role_ref" } as FieldDef;

function WhoEditor({
  actor,
  fields,
  applicantField,
  onChange,
  name,
}: {
  actor: Extract<ActorModel, { kind: "simple" }>;
  fields: FieldDef[];
  applicantField: string | null;
  onChange: (a: ActorModel) => void;
  name: string;
}) {
  const [, rerender] = React.useReducer((x: number) => x + 1, 0);
  const roles = actor.who.flatMap((w) => (w.kind === "role" && w.role ? [w.role] : []));
  const rolesKey = roles.join(",");
  React.useEffect(() => {
    void resolveRoleNames(roles).then((learned) => learned && rerender());
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [rolesKey]);
  const all: Who[] = [
    { kind: "editors" },
    { kind: "creator" },
    ...fields
      .filter((f) => f.type === "user_ref" && f.key !== applicantField)
      .map((f): Who => ({ kind: "user_field", field: f.key })),
    ...fields.filter((f) => f.type === "role_ref").map((f): Who => ({ kind: "role_field", field: f.key })),
  ];
  const choices = all.filter((c) => !actor.who.some((w) => sameWho(w, c)));
  const set = (who: Who[]) => onChange({ ...actor, who });
  // 「テーブルを編集できる人」はほかを含むので、選んだらほかを外す（ほかを選んだら外す）。
  const add = (w: Who) =>
    set(w.kind === "editors" ? [w] : [...actor.who.filter((x) => x.kind !== "editors"), w]);
  return (
    <div className="space-y-1.5">
      <ul className="flex flex-wrap gap-1" aria-label={`${name}の進められる人`}>
        {actor.who.map((w, i) => (
          <li
            key={`${w.kind}-${i}`}
            className="inline-flex max-w-full items-center gap-1 rounded-full border bg-card py-0.5 pl-2 pr-0.5 text-xs"
          >
            <span className="truncate">{whoLabel(w, fields)}</span>
            <button
              type="button"
              aria-label={`「${whoLabel(w, fields)}」を外す`}
              onClick={() => set(actor.who.filter((_, j) => j !== i))}
              className="rounded-full p-0.5 text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring pointer-coarse:p-2"
            >
              <X className="size-3" aria-hidden />
            </button>
          </li>
        ))}
      </ul>
      {actor.who.map((w, i) =>
        w.kind === "role" && !w.role ? (
          <RefPicker
            key={`role-${i}`}
            field={ROLE_FIELD}
            value={[]}
            ariaLabel={`${name}の進められる部署・グループを探す`}
            onChange={(ids, labels) => {
              const id = ids[0];
              if (!id) return;
              if (labels[id]) rememberRoleName(id, labels[id]);
              set(actor.who.map((x, j) => (j === i ? { kind: "role", role: id } : x)));
            }}
          />
        ) : null,
      )}
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button variant="outline" size="sm" className="h-7 gap-1 px-2 text-xs" aria-label={`${name}の進められる人を追加`}>
            <Plus className="size-3.5" aria-hidden />
            進められる人を追加
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="max-w-72">
          {choices.map((c) => (
            <DropdownMenuItem key={JSON.stringify(c)} onSelect={() => add(c)}>
              {whoLabel(c, fields)}
            </DropdownMenuItem>
          ))}
          <DropdownMenuItem onSelect={() => add({ kind: "role", role: "" })}>部署・グループを選ぶ</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}

function StateSelect({
  value,
  states,
  onChange,
  aria,
}: {
  value: string;
  states: FlowDraft["states"];
  onChange: (id: string) => void;
  aria: string;
}) {
  return (
    <Select value={value || undefined} onValueChange={onChange}>
      <SelectTrigger aria-label={aria} className="h-8 text-[13px]">
        <SelectValue placeholder="選んでください" />
      </SelectTrigger>
      <SelectContent>
        {states
          .filter((s) => s.name.trim())
          .map((s) => (
            <SelectItem key={s.id} value={s.id}>
              {s.name.trim()}
            </SelectItem>
          ))}
      </SelectContent>
    </Select>
  );
}

/// 進め方の 1 行（PC は表の行・スマホはカード）。
function MoveRow({
  move,
  index,
  draft,
  fields,
  error,
  pending,
  onChange,
  onRemove,
}: {
  move: MoveDraft;
  index: number;
  draft: FlowDraft;
  fields: FieldDef[];
  error: string | undefined;
  pending: number;
  onChange: (m: MoveDraft) => void;
  onRemove: () => void;
}) {
  const nameOf = (id: string) => draft.states.find((s) => s.id === id)?.name.trim() || "";
  const name = `進め方 ${index + 1}`;
  const a = move.actor;
  const errId = `move-error-${move.id}`;
  const remove = (
    <Button
      variant="ghost"
      size="icon"
      className="size-8 text-muted-foreground hover:text-destructive pointer-coarse:size-10"
      aria-label={`${name}（「${nameOf(move.from) || "未選択"}」から「${nameOf(move.to) || "未選択"}」）を削除`}
      onClick={onRemove}
    >
      <Trash2 className="size-4" aria-hidden />
    </Button>
  );
  return (
    <li
      className={cn(
        "grid gap-2 rounded-lg border p-2.5 sm:grid-cols-[8.5rem_1rem_8.5rem_minmax(0,1fr)_7.5rem_2rem] sm:items-start sm:rounded-none sm:border-0 sm:border-t sm:px-2 sm:py-2",
        error && "border-destructive/50 sm:border-t-border",
      )}
      data-testid="state-flow-move"
      aria-label={name}
      aria-describedby={error ? errId : undefined}
    >
      {/* スマホのカードの見出し（PC の表は行の最後に削除のボタンを置く）。 */}
      <div className="flex items-center justify-between sm:hidden">
        <span className="text-xs font-medium">{name}</span>
        {remove}
      </div>
      <div className="space-y-1">
        <span className="text-[11px] text-muted-foreground sm:sr-only">いまの状態</span>
        <StateSelect value={move.from} states={draft.states} aria={`${name}のいまの状態`} onChange={(from) => onChange({ ...move, from })} />
      </div>
      <ArrowRight className="mt-2 hidden size-4 text-muted-foreground sm:block" aria-hidden />
      <div className="space-y-1">
        <span className="text-[11px] text-muted-foreground sm:sr-only">次の状態</span>
        <StateSelect
          value={move.to}
          states={draft.states}
          aria={`${name}の次の状態`}
          onChange={(to) =>
            // 足したばかりの進め方で、まだ「申請した本人は除く」に触っていなければ、行き先に合わせて既定を決め直す
            // （最初の状態へ戻す進め方は申請者がするので除かない・D-115）。
            onChange(
              move.autoExclude && a.kind === "simple"
                ? { ...move, to, actor: { ...a, excludeApplicant: to !== draft.states[0]?.id } }
                : { ...move, to },
            )
          }
        />
      </div>
      <div className="min-w-0 space-y-1">
        <span className="text-[11px] text-muted-foreground sm:sr-only">進められる人</span>
        {a.kind === "simple" ? (
          <WhoEditor actor={a} fields={fields} applicantField={draft.applicantField} name={name} onChange={(actor) => onChange({ ...move, actor })} />
        ) : (
          <p className="rounded-md bg-muted/60 px-2 py-1.5 text-[11px] leading-relaxed text-muted-foreground" data-testid="state-flow-custom-actor">
            チャットや API で決めた条件です。ここでは変えられません。変えるには、この進め方を削除して追加し直してください。
          </p>
        )}
      </div>
      <div className="space-y-1">
        <label className="flex items-center gap-2 text-[12px] sm:flex-col sm:items-start sm:gap-1">
          <Switch
            checked={a.kind === "simple" && a.excludeApplicant && Boolean(draft.applicantField)}
            disabled={a.kind !== "simple" || !draft.applicantField}
            aria-label={`${name}で申請した本人は除く`}
            onCheckedChange={(on) =>
              a.kind === "simple" && onChange({ ...move, autoExclude: false, actor: { ...a, excludeApplicant: on } })
            }
          />
          <span className="sm:text-[11px] sm:text-muted-foreground">申請した本人は除く</span>
        </label>
        {a.kind === "simple" && draft.applicantField && !a.excludeApplicant ? (
          <p className="text-[11px] leading-relaxed text-[var(--season-autumn)]" data-testid="state-flow-self-allowed">
            申請した本人も、この進め方で進められます。
          </p>
        ) : null}
      </div>
      <div className="hidden justify-self-end sm:block">{remove}</div>
      {error || pending > 0 ? (
        <div className="space-y-0.5 sm:col-span-6">
          {error ? (
            <p id={errId} className="text-[11px] text-destructive">
              {error}
            </p>
          ) : null}
          {pending > 0 ? (
            <p className="text-[11px] text-muted-foreground">承認を待っている {pending.toLocaleString("ja-JP")} 件が、この進め方を使います。</p>
          ) : null}
        </div>
      ) : null}
    </li>
  );
}

export function MovesTable({
  draft,
  fields,
  errors,
  pending,
  onChange,
  onAdd,
}: {
  draft: FlowDraft;
  fields: FieldDef[];
  errors: Map<string, string>;
  pending: PendingApprovalMove[];
  onChange: (moves: MoveDraft[]) => void;
  onAdd: () => void;
}) {
  const nameOf = (id: string) => draft.states.find((s) => s.id === id)?.name.trim() ?? "";
  const pendingOf = (m: MoveDraft) =>
    pending.filter((p) => p.from === nameOf(m.from) && p.to === nameOf(m.to)).reduce((n, p) => n + p.count, 0);
  return (
    <div className="space-y-2">
      {draft.moves.length === 0 ? (
        <p className="rounded-lg border border-dashed px-3 py-4 text-center text-xs text-muted-foreground">
          まだ進め方がありません。「進め方を追加」で、どの状態からどの状態へ進めるかを決めます。
        </p>
      ) : (
        <ul className="space-y-2 sm:space-y-0 sm:rounded-lg sm:border" data-testid="state-flow-moves">
          <li
            className="hidden text-[11px] font-medium text-muted-foreground sm:grid sm:grid-cols-[8.5rem_1rem_8.5rem_minmax(0,1fr)_7.5rem_2rem] sm:gap-2 sm:px-2 sm:py-1.5"
            aria-hidden
          >
            <span>いまの状態</span>
            <span />
            <span>次の状態</span>
            <span>進められる人</span>
            <span>申請した本人</span>
            <span />
          </li>
          {draft.moves.map((m, i) => (
            <MoveRow
              key={m.id}
              move={m}
              index={i}
              draft={draft}
              fields={fields}
              error={errors.get(m.id)}
              pending={pendingOf(m)}
              onChange={(next) => onChange(draft.moves.map((x) => (x.id === m.id ? next : x)))}
              onRemove={() => onChange(draft.moves.filter((x) => x.id !== m.id))}
            />
          ))}
        </ul>
      )}
      <Button
        variant="outline"
        size="sm"
        className="w-full border-dashed"
        onClick={onAdd}
        disabled={draft.states.filter((s) => s.name.trim()).length < 2}
        data-testid="state-flow-add-move"
      >
        <Plus className="size-4" aria-hidden />
        進め方を追加
      </Button>
      {draft.states.filter((s) => s.name.trim()).length < 2 ? (
        <p className="text-[11px] text-muted-foreground">進め方を追加するには、状態が 2 つ以上要ります。</p>
      ) : null}
    </div>
  );
}
