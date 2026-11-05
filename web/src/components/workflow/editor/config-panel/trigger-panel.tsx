"use client";

/// トリガ（きっかけ）の設定パネル。
///
/// - スケジュール: プリセット（毎時/毎日/平日/毎週/毎月）から cron を生成し（曜日は MON〜SUN の名前で書く）、
///   次の実行をサーバーで数えて出す（発火と同じ読み方・BUG-95）。上級者向けに raw cron 欄も残す。
/// - できごと: source（ファイル保存・テーブルの行の追加／更新／状態の変更）。ファイル保存は対象フォルダ（既存の
///   FolderPicker 再利用）、データ変更は対象テーブル（業務データのブロックと同じ選択欄）を必ず決める。状態の
///   変更は「この状態になったとき」で絞れる（filter の `/to`・GAP-07）。
/// - 手動: ボタンから実行。実行するときに入れてもらう入力項目（IR の `input_schema`・GAP-12）を編集する
///   （input_schema はワークフローに 1 つで、手動の実行・ミニアプリのボタン・チャットからの実行で共通）。

import * as React from "react";
import { AlertCircle, FolderOpen, Plus, Trash2 } from "lucide-react";

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
import type { Condition, Trigger, ValidationError } from "@/generated/workflow-ir";
import { Field, FieldInput, FieldSelectTrigger } from "./common";
import { previewSchedule } from "@/lib/workflow-api";
import { DataTableSelect, isFsmTable, NoFsmTableNote, useDataTables } from "./data-tables";
import { useDriveItem } from "./forms-files";
import type { InputRefUsage } from "./input-schema-editor";
import { InputsSection } from "./inputs-section";
import { triggerErrorView } from "../validation";

/// きっかけの中の位置（`scope/folder`・`cron` など）→ 保存時の誤りの文言。
type FieldErrors = Map<string, string>;
const NO_FIELD_ERRORS: FieldErrors = new Map();

/// 曜日（cron に書く名前と画面の名前・月曜から）。cron には数でなく名前で書く（曜日の番号の読み方の違いで
/// 1 日ずれないように・BUG-95。サーバーは 0 と 7 を日曜、1 を月曜として読む）。
const WEEKDAYS = [
  { cron: "MON", label: "月曜日" },
  { cron: "TUE", label: "火曜日" },
  { cron: "WED", label: "水曜日" },
  { cron: "THU", label: "木曜日" },
  { cron: "FRI", label: "金曜日" },
  { cron: "SAT", label: "土曜日" },
  { cron: "SUN", label: "日曜日" },
] as const;
type Weekday = (typeof WEEKDAYS)[number]["cron"];

/// 平日（月〜金）の曜日の欄。
const WEEKDAYS_MON_FRI = "MON-FRI";

/// 標準の cron の曜日の数（0 と 7 が日曜）。
const WEEKDAY_BY_NUMBER: Weekday[] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT", "SUN"];

/// 曜日の欄の 1 つの曜日（名前か 0〜7 の数）を名前にする（1 つの曜日でなければ null）。
function weekdayOf(dow: string): Weekday | null {
  if (/^[0-7]$/.test(dow)) return WEEKDAY_BY_NUMBER[Number(dow)];
  const upper = dow.toUpperCase();
  return WEEKDAYS.find((w) => w.cron === upper)?.cron ?? null;
}

type Preset =
  | { kind: "hourly"; minute: number }
  | { kind: "daily"; hour: number; minute: number }
  | { kind: "weekdays"; hour: number; minute: number }
  | { kind: "weekly"; weekday: Weekday; hour: number; minute: number }
  | { kind: "monthly"; day: number; hour: number; minute: number }
  | { kind: "custom" };

function presetToCron(p: Preset): string | null {
  switch (p.kind) {
    case "hourly":
      return `${p.minute} * * * *`;
    case "daily":
      return `${p.minute} ${p.hour} * * *`;
    case "weekdays":
      return `${p.minute} ${p.hour} * * ${WEEKDAYS_MON_FRI}`;
    case "weekly":
      return `${p.minute} ${p.hour} * * ${p.weekday}`;
    case "monthly":
      return `${p.minute} ${p.hour} ${p.day} * *`;
    default:
      return null;
  }
}

/// cron からプリセットを推定する（一致しなければ custom）。保存済みの数の曜日（0〜7）も標準の読み方で読む。
function cronToPreset(cron: string): Preset {
  const m = cron.trim().split(/\s+/);
  if (m.length !== 5) return { kind: "custom" };
  const [min, hour, day, month, dow] = m;
  const n = (s: string) => (/^\d+$/.test(s) ? Number(s) : null);
  if (n(min) !== null && hour === "*" && day === "*" && month === "*" && dow === "*")
    return { kind: "hourly", minute: n(min)! };
  if (n(min) === null || n(hour) === null || month !== "*") return { kind: "custom" };
  const time = { hour: n(hour)!, minute: n(min)! };
  if (day === "*" && dow === "*") return { kind: "daily", ...time };
  if (day === "*" && (dow.toUpperCase() === WEEKDAYS_MON_FRI || dow === "1-5"))
    return { kind: "weekdays", ...time };
  const weekday = day === "*" ? weekdayOf(dow) : null;
  if (weekday) return { kind: "weekly", weekday, ...time };
  if (n(day) !== null && dow === "*") return { kind: "monthly", day: n(day)!, ...time };
  return { kind: "custom" };
}

/// 時刻を「10/28（火）9:00」の形にする（時間帯はスケジュールの時間帯）。
function formatFire(iso: string, tz: string): string {
  const parts = new Intl.DateTimeFormat("ja-JP", {
    timeZone: tz,
    month: "numeric",
    day: "numeric",
    weekday: "short",
    hour: "numeric",
    minute: "2-digit",
    hourCycle: "h23",
  }).formatToParts(new Date(iso));
  const get = (t: Intl.DateTimeFormatPartTypes) => parts.find((p) => p.type === t)?.value ?? "";
  return `${get("month")}/${get("day")}（${get("weekday")}）${get("hour")}:${get("minute")}`;
}

/// 時間帯の呼び名（次の実行の見出しの括弧に入れる）。
function zoneLabel(tz: string): string {
  if (tz === "Asia/Tokyo") return "日本時間";
  return tz;
}

/// 次の実行の見通し（サーバーの発火と同じ読み方で数える・式と時間帯が変わるたびに少し待って取り直す）。
type FirePreview =
  | { key: string; state: "ok"; next: string[] }
  | { key: string; state: "invalid"; message: string }
  | { key: string; state: "unavailable" };

function useFirePreview(cron: string, tz: string): FirePreview | null {
  const key = `${cron}\u0000${tz}`;
  const [preview, setPreview] = React.useState<FirePreview | null>(null);
  React.useEffect(() => {
    let active = true;
    const timer = setTimeout(() => {
      previewSchedule(cron, tz)
        .then((p) => {
          if (!active) return;
          setPreview(
            p.error
              ? { key, state: "invalid", message: p.error }
              : { key, state: "ok", next: p.next },
          );
        })
        .catch(() => {
          if (active) setPreview({ key, state: "unavailable" });
        });
    }, 300);
    return () => {
      active = false;
      clearTimeout(timer);
    };
  }, [cron, tz, key]);
  // 前の式の結果は出さない（打ち替えた直後に古い曜日を見せない）。
  return preview?.key === key ? preview : null;
}

function FirePreviewLine({ preview, tz }: { preview: FirePreview | null; tz: string }) {
  if (!preview) {
    return (
      <p className="text-[11px] text-muted-foreground" aria-live="polite">
        次の実行を確かめています…
      </p>
    );
  }
  if (preview.state === "invalid") {
    return (
      <p className="text-[11px] text-destructive" role="alert">
        {preview.message}
      </p>
    );
  }
  if (preview.state === "unavailable" || preview.next.length === 0) {
    return (
      <p className="text-[11px] text-muted-foreground" aria-live="polite">
        次の実行を確かめられませんでした。
      </p>
    );
  }
  return (
    <p
      className="rounded-md border bg-muted/40 p-2 text-[11px] text-muted-foreground"
      aria-live="polite"
    >
      次の実行（{zoneLabel(tz)}）: {preview.next.map((t) => formatFire(t, tz)).join("、")}
    </p>
  );
}

function ScheduleEditor({
  trigger,
  onChange,
  fieldErrors = NO_FIELD_ERRORS,
}: {
  trigger: Extract<Trigger, { kind: "schedule" }>;
  onChange: (next: Trigger) => void;
  fieldErrors?: FieldErrors;
}) {
  const preset = cronToPreset(trigger.cron);
  const tz = trigger.tz || "Asia/Tokyo";
  const fires = useFirePreview(trigger.cron, tz);
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
            if (v === "weekdays") set({ kind: "weekdays", hour: 9, minute: 0 });
            if (v === "weekly") set({ kind: "weekly", weekday: "MON", hour: 9, minute: 0 });
            if (v === "monthly") set({ kind: "monthly", day: 1, hour: 9, minute: 0 });
          }}
        >
          <FieldSelectTrigger className="h-8">
            <SelectValue />
          </FieldSelectTrigger>
          <SelectContent>
            <SelectItem value="hourly">毎時</SelectItem>
            <SelectItem value="daily">毎日</SelectItem>
            <SelectItem value="weekdays">平日（月〜金）</SelectItem>
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
      {preset.kind === "daily" ||
      preset.kind === "weekdays" ||
      preset.kind === "weekly" ||
      preset.kind === "monthly" ? (
        <div className="flex flex-wrap items-start gap-2">
          {preset.kind === "weekly" ? (
            <Field label="曜日">
              <Select
                value={preset.weekday}
                onValueChange={(v) => {
                  const weekday = weekdayOf(v);
                  if (weekday) set({ ...preset, weekday });
                }}
              >
                <FieldSelectTrigger className="h-8 w-24">
                  <SelectValue />
                </FieldSelectTrigger>
                <SelectContent>
                  {WEEKDAYS.map((w) => (
                    <SelectItem key={w.cron} value={w.cron}>
                      {w.label}
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

      <Field
        label="cron 式（上級者向け）"
        hint="分 時 日 月 曜日 の 5 つを半角スペースで区切って書きます。曜日は MON〜SUN の名前で書けます。"
        error={fieldErrors.get("cron")}
      >
        <FieldInput
          value={trigger.cron}
          placeholder="例: 0 9 * * MON-FRI"
          onChange={(e) => onChange({ ...trigger, cron: e.target.value })}
          className="h-8 font-mono text-xs max-md:text-base"
        />
      </Field>
      <Field label="タイムゾーン" error={fieldErrors.get("tz")}>
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
      <FirePreviewLine preview={fires} tz={tz} />
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

function EventEditor({
  trigger,
  onChange,
  fieldErrors,
}: {
  trigger: EventTrigger;
  onChange: (next: Trigger) => void;
  fieldErrors: FieldErrors;
}) {
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
      {isTable ? (
        <TableEventFields trigger={trigger} onChange={onChange} error={fieldErrors.get("scope/table")} />
      ) : (
        <FolderEventFields trigger={trigger} onChange={onChange} error={fieldErrors.get("scope/folder")} />
      )}
    </div>
  );
}

function FolderEventFields({
  trigger,
  onChange,
  error,
}: {
  trigger: EventTrigger;
  onChange: (next: Trigger) => void;
  /// 保存時の誤り（選んだフォルダーが見つからないなど）。
  error?: string;
}) {
  const [pickerOpen, setPickerOpen] = React.useState(false);
  const [folderName, setFolderName] = React.useState<string | null>(null);
  const folder = (trigger.scope as { folder?: string } | null)?.folder ?? "";
  // 保存した定義を開き直したときも名前で出す（id は出さない）。引けなければ選び直しを促す。
  const saved = useDriveItem(folder || null);
  return (
    <>
      <Field
        label="対象フォルダ"
        hint="このフォルダ（配下含む）にファイルが保存されたとき（新しい版を含む）に動きます。同じファイルの続く保存は 1 回にまとめ、このワークフロー自身の保存では動きません。必須です"
        error={error}
      >
        <div className="flex items-center gap-1.5">
          <FieldInput
            value={folderName ?? saved.item?.name ?? ""}
            readOnly
            placeholder={folder && saved.missing ? "見つからないフォルダーです。選び直してください" : "フォルダを選択…"}
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

function TableEventFields({
  trigger,
  onChange,
  error,
}: {
  trigger: EventTrigger;
  onChange: (next: Trigger) => void;
  /// 保存時の誤り（選んだテーブルが見つからないなど）。
  error?: string;
}) {
  const tableId = (trigger.scope as { table?: string } | null)?.table ?? null;
  // 束縛済みのテーブルが一覧（200 件で打ち切る）に無くても、1 件取って足す（BUG-68）。
  const source = useDataTables({ bound: [tableId] });
  const found = source.lookup(tableId);
  const table = found.state === "ok" ? found.table : null;
  const statusField = table?.schema.status_field
    ? table.schema.fields.find((f) => f.key === table.schema.status_field)
    : undefined;
  // 状態の変化のきっかけは、状態の進め方（FSM）のあるテーブルでしか起きない（無いテーブルを選ぶと、保存はでき
  // てもいつまでも実行しなかった・保存時の照合も止める・BUG-105）。
  const byState = trigger.source === "data.transition";
  const states = table && isFsmTable(table) ? (statusField?.options ?? []) : [];
  const target = transitionTarget(trigger.filter);
  return (
    <>
      <Field
        label="対象のテーブル"
        hint="有効化した人に見える行・列の変更だけで動きます。一括取込と、このワークフロー自身の書込では動きません。必須です"
        error={error ?? (source.error ? "テーブルの一覧を読み込めませんでした" : null)}
      >
        <DataTableSelect
          value={tableId}
          source={source}
          eligible={byState ? isFsmTable : undefined}
          ineligibleNote={
            byState
              ? "このテーブルには状態の進め方がありません。状態の進め方（例: 申請中 → 承認済み）を決めたテーブルを選んでください。"
              : undefined
          }
          stateFlowLink={byState}
          onChange={(id) => {
            // テーブルを変えたら状態の条件は外す（別のテーブルの状態名は当てはまらない）。
            const { filter: _filter, ...rest } = trigger;
            void _filter;
            onChange({ ...rest, scope: { table: id } } as Trigger);
          }}
        />
        {byState ? <NoFsmTableNote source={source} testId="trigger-no-fsm-table" /> : null}
      </Field>
      {byState ? (
        <Field
          label="この状態になったとき"
          hint={
            states.length === 0
              ? "先に「対象のテーブル」で、状態の進め方のあるテーブルを選んでください。"
              : "「どの状態でも」なら、どの状態に変わっても実行します。"
          }
        >
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
  errors = [],
  inputSchemaErrors = [],
}: {
  triggers: Trigger[];
  index: number;
  onChange: (next: Trigger[]) => void;
  /// ワークフローの入力項目（IR の `input_schema`）。`renames` はキーを変えた項目（旧キー → 新キー）。
  inputSchema: unknown;
  onInputSchemaChange: (next: unknown, renames?: Record<string, string>) => void;
  /// ブロックが参照している入力項目のキー（定義に無いキーを知らせる）。
  inputUsages?: InputRefUsage[];
  /// このきっかけの保存時の誤り（欄に写せるものは欄の下、ほかは上の帯に出す）。
  errors?: ValidationError[];
  /// 入力項目の保存時の誤り（入力項目の編集の下に出す）。
  inputSchemaErrors?: ValidationError[];
}) {
  const view = React.useMemo(() => triggerErrorView(errors), [errors]);
  const trigger = triggers[index];
  if (!trigger) return null;
  const replace = (next: Trigger) =>
    onChange(triggers.map((t, i) => (i === index ? next : t)));

  return (
    <div className="space-y-3">
      {view.banner.length > 0 ? (
        <div
          role="alert"
          className="space-y-1 rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-xs text-destructive"
          data-testid="trigger-errors"
        >
          {view.banner.map((m, i) => (
            <p key={i} className="flex items-start gap-1.5 [overflow-wrap:anywhere]">
              <AlertCircle className="mt-0.5 size-3.5 shrink-0" aria-hidden />
              {m}
            </p>
          ))}
        </div>
      ) : null}
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
        <ScheduleEditor trigger={trigger} onChange={replace} fieldErrors={view.fields} />
      ) : null}
      {trigger.kind === "event" ? (
        <EventEditor trigger={trigger} onChange={replace} fieldErrors={view.fields} />
      ) : null}
      {trigger.kind === "interactive" ? (
        <>
          <p className="text-[11px] text-muted-foreground">
            「実行」ボタンや AI チャットから手動で開始できます
          </p>
          <div className="border-t pt-3">
            <InputsSection
              schema={inputSchema}
              onChange={onInputSchemaChange}
              usages={inputUsages}
              errors={inputSchemaErrors}
            />
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
