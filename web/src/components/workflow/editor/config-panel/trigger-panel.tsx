"use client";

/// トリガ（きっかけ）の設定パネル。
///
/// - スケジュール: プリセット（毎時/毎日/毎週/毎月）から cron を生成し、次回実行を
///   cron-parser でプレビュー。上級者向けに raw cron 欄も残す。
/// - できごと: source（ファイル保存・テーブルの行の追加／更新／状態の変更）。ファイル保存は対象フォルダ（既存の
///   FolderPicker 再利用）、データ変更は対象テーブル（業務データのブロックと同じ選択欄）を必ず決める。状態の
///   変更は「この状態になったとき」で絞れる（filter の `/to`・GAP-07）。
/// - 手動: ボタンから実行。実行するときに入れてもらう入力項目（IR の `input_schema`・GAP-12）を編集する
///   （input_schema はワークフローに 1 つで、手動の実行・ミニアプリのボタン・チャットからの実行で共通）。

import * as React from "react";
import parser from "cron-parser";
import { FolderOpen, Plus, Trash2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { FolderPicker, type FolderChoice } from "@/components/artifacts/folder-picker";
import { EVENT_CATALOG } from "@/generated/workflow-catalog";
import type { Condition, Trigger } from "@/generated/workflow-ir";
import { Field, FieldInput, FieldSelectTrigger } from "./common";
import { DataTableSelect, useDataTables } from "./data-tables";
import { InputSchemaEditor, type InputRefUsage } from "./input-schema-editor";

const WEEKDAYS = ["日", "月", "火", "水", "木", "金", "土"];

type Preset =
  | { kind: "hourly"; minute: number }
  | { kind: "daily"; hour: number; minute: number }
  | { kind: "weekly"; weekday: number; hour: number; minute: number }
  | { kind: "monthly"; day: number; hour: number; minute: number }
  | { kind: "custom" };

function presetToCron(p: Preset): string | null {
  switch (p.kind) {
    case "hourly":
      return `${p.minute} * * * *`;
    case "daily":
      return `${p.minute} ${p.hour} * * *`;
    case "weekly":
      return `${p.minute} ${p.hour} * * ${p.weekday}`;
    case "monthly":
      return `${p.minute} ${p.hour} ${p.day} * *`;
    default:
      return null;
  }
}

/// cron からプリセットを推定する（一致しなければ custom）。
function cronToPreset(cron: string): Preset {
  const m = cron.trim().split(/\s+/);
  if (m.length !== 5) return { kind: "custom" };
  const [min, hour, day, month, dow] = m;
  const n = (s: string) => (/^\d+$/.test(s) ? Number(s) : null);
  if (n(min) !== null && hour === "*" && day === "*" && month === "*" && dow === "*")
    return { kind: "hourly", minute: n(min)! };
  if (n(min) !== null && n(hour) !== null && day === "*" && month === "*" && dow === "*")
    return { kind: "daily", hour: n(hour)!, minute: n(min)! };
  if (n(min) !== null && n(hour) !== null && day === "*" && month === "*" && n(dow) !== null)
    return { kind: "weekly", weekday: n(dow)!, hour: n(hour)!, minute: n(min)! };
  if (n(min) !== null && n(hour) !== null && n(day) !== null && month === "*" && dow === "*")
    return { kind: "monthly", day: n(day)!, hour: n(hour)!, minute: n(min)! };
  return { kind: "custom" };
}

function nextFires(cron: string, tz: string, count = 2): string[] {
  // backend スケジューラは 5 フィールド（分 時 日 月 曜日）のみ受け付ける。
  // cron-parser は秒付き 6 フィールドも解釈できてしまうため、先に形を照合する
  //（プレビューが出るのに保存/有効化で落ちる齟齬を防ぐ）。
  if (cron.trim().split(/\s+/).length !== 5) return [];
  try {
    const it = parser.parseExpression(cron, { tz });
    const out: string[] = [];
    for (let i = 0; i < count; i += 1) {
      const d = it.next().toDate();
      out.push(
        d.toLocaleString("ja-JP", {
          timeZone: tz,
          month: "numeric",
          day: "numeric",
          weekday: "short",
          hour: "2-digit",
          minute: "2-digit",
        }),
      );
    }
    return out;
  } catch {
    return [];
  }
}

function ScheduleEditor({
  trigger,
  onChange,
}: {
  trigger: Extract<Trigger, { kind: "schedule" }>;
  onChange: (next: Trigger) => void;
}) {
  const preset = cronToPreset(trigger.cron);
  const tz = trigger.tz || "Asia/Tokyo";
  const fires = nextFires(trigger.cron, tz);
  const set = (p: Preset) => {
    const cron = presetToCron(p);
    if (cron) onChange({ ...trigger, cron });
  };
  const numeric = (v: string, fallback: number) =>
    /^\d+$/.test(v) ? Number(v) : fallback;

  return (
    <div className="space-y-3">
      <Field label="繰り返し">
        <Select
          value={preset.kind}
          onValueChange={(v) => {
            if (v === "hourly") set({ kind: "hourly", minute: 0 });
            if (v === "daily") set({ kind: "daily", hour: 9, minute: 0 });
            if (v === "weekly") set({ kind: "weekly", weekday: 1, hour: 9, minute: 0 });
            if (v === "monthly") set({ kind: "monthly", day: 1, hour: 9, minute: 0 });
          }}
        >
          <FieldSelectTrigger className="h-8">
            <SelectValue />
          </FieldSelectTrigger>
          <SelectContent>
            <SelectItem value="hourly">毎時</SelectItem>
            <SelectItem value="daily">毎日</SelectItem>
            <SelectItem value="weekly">毎週</SelectItem>
            <SelectItem value="monthly">毎月</SelectItem>
            <SelectItem value="custom" disabled={preset.kind !== "custom"}>
              カスタム（cron）
            </SelectItem>
          </SelectContent>
        </Select>
      </Field>

      {preset.kind === "hourly" ? (
        <Field label="何分に">
          <FieldInput
            type="number"
            min={0}
            max={59}
            value={preset.minute}
            onChange={(e) => set({ kind: "hourly", minute: numeric(e.target.value, 0) })}
            className="h-8 w-24"
          />
        </Field>
      ) : null}
      {preset.kind === "daily" || preset.kind === "weekly" || preset.kind === "monthly" ? (
        <div className="flex flex-wrap items-end gap-2">
          {preset.kind === "weekly" ? (
            <Field label="曜日">
              <Select
                value={String(preset.weekday)}
                onValueChange={(v) => set({ ...preset, weekday: Number(v) })}
              >
                <FieldSelectTrigger className="h-8 w-24">
                  <SelectValue />
                </FieldSelectTrigger>
                <SelectContent>
                  {WEEKDAYS.map((w, i) => (
                    <SelectItem key={i} value={String(i)}>
                      {w}曜日
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
          ) : null}
          {preset.kind === "monthly" ? (
            <Field label="日にち">
              <FieldInput
                type="number"
                min={1}
                max={31}
                value={preset.day}
                onChange={(e) => set({ ...preset, day: numeric(e.target.value, 1) })}
                className="h-8 w-20"
              />
            </Field>
          ) : null}
          <Field label="時刻">
            <div className="flex items-center gap-1">
              <Input
                type="number"
                min={0}
                max={23}
                value={preset.hour}
                onChange={(e) => set({ ...preset, hour: numeric(e.target.value, 9) })}
                className="h-8 w-16"
                aria-label="時"
              />
              <span className="text-xs text-muted-foreground">:</span>
              <Input
                type="number"
                min={0}
                max={59}
                value={preset.minute}
                onChange={(e) => set({ ...preset, minute: numeric(e.target.value, 0) })}
                className="h-8 w-16"
                aria-label="分"
              />
            </div>
          </Field>
        </div>
      ) : null}

      <Field label="cron 式（上級者向け）" hint="分 時 日 月 曜日">
        <FieldInput
          value={trigger.cron}
          onChange={(e) => onChange({ ...trigger, cron: e.target.value })}
          className="h-8 font-mono text-xs max-md:text-base"
        />
      </Field>
      <Field label="タイムゾーン">
        <Select value={tz} onValueChange={(v) => onChange({ ...trigger, tz: v })}>
          <FieldSelectTrigger className="h-8">
            <SelectValue />
          </FieldSelectTrigger>
          <SelectContent>
            <SelectItem value="Asia/Tokyo">日本（Asia/Tokyo）</SelectItem>
            <SelectItem value="UTC">UTC</SelectItem>
          </SelectContent>
        </Select>
      </Field>
      {fires.length > 0 ? (
        <p className="rounded-md border bg-muted/40 p-2 text-[11px] text-muted-foreground">
          次回の実行: {fires.join("、")}
        </p>
      ) : (
        <p className="text-[11px] text-destructive">cron 式を確認してください</p>
      )}
    </div>
  );
}

/// きっかけにできるできごと（イベントのカタログの単一定義・使えるものだけ）。
const EVENT_SOURCES = EVENT_CATALOG.filter((e) => e.available);

type EventTrigger = Extract<Trigger, { kind: "event" }>;

/// 「この状態になったとき」の filter（`$from trigger` の `/to` と一致）から状態を読む。
function transitionTarget(filter: Condition | undefined): string | null {
  if (filter && "cmp" in filter) {
    const { left, op, right } = filter.cmp;
    const ref = left as { $from?: unknown; path?: unknown } | null;
    const isTo = ref !== null && typeof ref === "object" && ref.$from === "trigger" && ref.path === "/to";
    if (isTo && op === "eq" && typeof right === "string") return right;
  }
  return null;
}

function EventEditor({ trigger, onChange }: { trigger: EventTrigger; onChange: (next: Trigger) => void }) {
  const source = EVENT_SOURCES.find((e) => e.source === trigger.source);
  const isTable = source?.scope_key === "table";
  return (
    <div className="space-y-3">
      <Field label="できごと">
        <Select
          value={trigger.source}
          onValueChange={(v) => {
            if (v === trigger.source) return;
            // 束縛の種類（フォルダ／テーブル）が変わるので、範囲と条件は選び直してもらう。
            onChange({ kind: "event", source: v, scope: {} } as unknown as Trigger);
          }}
        >
          <FieldSelectTrigger className="h-8">
            <SelectValue />
          </FieldSelectTrigger>
          <SelectContent>
            {EVENT_SOURCES.map((e) => (
              <SelectItem key={e.source} value={e.source}>
                {e.label_ja}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </Field>
      {isTable ? <TableEventFields trigger={trigger} onChange={onChange} /> : <FolderEventFields trigger={trigger} onChange={onChange} />}
    </div>
  );
}

function FolderEventFields({ trigger, onChange }: { trigger: EventTrigger; onChange: (next: Trigger) => void }) {
  const [pickerOpen, setPickerOpen] = React.useState(false);
  const [folderName, setFolderName] = React.useState<string | null>(null);
  const folder = (trigger.scope as { folder?: string } | null)?.folder ?? "";
  return (
    <>
      <Field
        label="対象フォルダ"
        hint="このフォルダ（配下含む）にファイルが保存されたとき（新しい版を含む）に動きます。同じファイルの続く保存は 1 回にまとめ、このワークフロー自身の保存では動きません。必須です"
      >
        <div className="flex items-center gap-1.5">
          <FieldInput
            value={folderName ?? folder}
            readOnly
            placeholder="フォルダを選択…"
            className="h-8 flex-1 text-xs max-md:text-base"
          />
          <Button
            variant="outline"
            size="sm"
            className="h-8"
            onClick={() => setPickerOpen(true)}
          >
            <FolderOpen className="size-3.5" aria-hidden />
            選ぶ
          </Button>
        </div>
      </Field>
      <FolderPicker
        open={pickerOpen}
        onOpenChange={setPickerOpen}
        onSelect={(choice: FolderChoice) => {
          setFolderName(choice.name);
          onChange({ ...trigger, scope: { folder: choice.id } } as Trigger);
          setPickerOpen(false);
        }}
      />
    </>
  );
}

/// 状態を絞らないときの選択肢の値（Select は空文字を値にできない）。
const ANY_STATE = "__any__";

function TableEventFields({ trigger, onChange }: { trigger: EventTrigger; onChange: (next: Trigger) => void }) {
  const tableId = (trigger.scope as { table?: string } | null)?.table ?? null;
  // 束縛済みのテーブルが一覧（200 件で打ち切る）に無くても、1 件取って足す（BUG-68）。
  const source = useDataTables({ bound: [tableId] });
  const found = source.lookup(tableId);
  const table = found.state === "ok" ? found.table : null;
  const statusField = table?.schema.status_field
    ? table.schema.fields.find((f) => f.key === table.schema.status_field)
    : undefined;
  const states = statusField?.options ?? [];
  const target = transitionTarget(trigger.filter);
  return (
    <>
      <Field
        label="対象のテーブル"
        hint="有効化した人に見える行・列の変更だけで動きます。一括取込と、このワークフロー自身の書込では動きません。必須です"
        error={source.error ? "テーブルの一覧を読み込めませんでした" : null}
      >
        <DataTableSelect
          value={tableId}
          source={source}
          onChange={(id) => {
            // テーブルを変えたら状態の条件は外す（別のテーブルの状態名は当てはまらない）。
            const { filter: _filter, ...rest } = trigger;
            void _filter;
            onChange({ ...rest, scope: { table: id } } as Trigger);
          }}
        />
      </Field>
      {trigger.source === "data.transition" ? (
        <Field label="この状態になったとき" hint={states.length === 0 ? "状態の列があるテーブルで選べます" : "選ばないとどの状態への変更でも動きます"}>
          <Select
            value={target ?? ANY_STATE}
            disabled={states.length === 0}
            onValueChange={(v) => {
              const { filter: _filter, ...rest } = trigger;
              void _filter;
              const next: EventTrigger =
                v === ANY_STATE
                  ? (rest as EventTrigger)
                  : {
                      ...rest,
                      filter: { cmp: { left: { $from: "trigger", path: "/to" }, op: "eq", right: v } } as Condition,
                    };
              onChange(next as Trigger);
            }}
          >
            <SelectTrigger className="h-8" aria-label="この状態になったとき">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={ANY_STATE}>どの状態でも</SelectItem>
              {states.map((s) => (
                <SelectItem key={s} value={s}>
                  {s}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </Field>
      ) : null}
      {target === null && trigger.filter ? (
        <p className="text-[11px] text-muted-foreground">条件（filter）は AI 編集や IR で設定されています</p>
      ) : null}
    </>
  );
}

export function TriggerPanel({
  triggers,
  index,
  onChange,
  inputSchema,
  onInputSchemaChange,
  inputUsages,
}: {
  triggers: Trigger[];
  index: number;
  onChange: (next: Trigger[]) => void;
  /// ワークフローの入力項目（IR の `input_schema`）。`renames` はキーを変えた項目（旧キー → 新キー）。
  inputSchema: unknown;
  onInputSchemaChange: (next: unknown, renames?: Record<string, string>) => void;
  /// ブロックが参照している入力項目のキー（定義に無いキーを知らせる）。
  inputUsages?: InputRefUsage[];
}) {
  const trigger = triggers[index];
  if (!trigger) return null;
  const replace = (next: Trigger) =>
    onChange(triggers.map((t, i) => (i === index ? next : t)));

  return (
    <div className="space-y-3">
      <Field label="きっかけの種類">
        <Select
          value={trigger.kind}
          onValueChange={(v) => {
            if (v === trigger.kind) return;
            if (v === "interactive") replace({ kind: "interactive" } as Trigger);
            if (v === "schedule")
              replace({ kind: "schedule", cron: "0 9 * * *", tz: "Asia/Tokyo", catchup: "skip" } as Trigger);
            if (v === "event")
              replace({ kind: "event", source: "storage.write", scope: {} } as unknown as Trigger);
          }}
        >
          <FieldSelectTrigger className="h-8">
            <SelectValue />
          </FieldSelectTrigger>
          <SelectContent>
            <SelectItem value="interactive">手動で実行</SelectItem>
            <SelectItem value="schedule">スケジュール</SelectItem>
            <SelectItem value="event">できごとが起きたとき</SelectItem>
          </SelectContent>
        </Select>
      </Field>
      {trigger.kind === "schedule" ? (
        <ScheduleEditor trigger={trigger} onChange={replace} />
      ) : null}
      {trigger.kind === "event" ? <EventEditor trigger={trigger} onChange={replace} /> : null}
      {trigger.kind === "interactive" ? (
        <>
          <p className="text-[11px] text-muted-foreground">
            「実行」ボタンや AI チャットから手動で開始できます
          </p>
          <div className="border-t pt-3">
            <InputSchemaEditor schema={inputSchema} onChange={onInputSchemaChange} usages={inputUsages} />
          </div>
        </>
      ) : null}
      <div className="flex items-center gap-2 border-t pt-3">
        <Button
          variant="outline"
          size="sm"
          className="h-7 text-xs"
          onClick={() =>
            onChange([...triggers, { kind: "interactive" } as Trigger])
          }
        >
          <Plus className="size-3.5" aria-hidden />
          きっかけを追加
        </Button>
        {triggers.length > 1 ? (
          <Button
            variant="ghost"
            size="sm"
            className="h-7 text-xs text-destructive"
            onClick={() => onChange(triggers.filter((_, i) => i !== index))}
          >
            <Trash2 className="size-3.5" aria-hidden />
            このきっかけを削除
          </Button>
        ) : null}
      </div>
    </div>
  );
}
