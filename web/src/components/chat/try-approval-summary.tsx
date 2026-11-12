"use client";

/// 会話の AI の試運転（`try_workflow`）の承認カードの中身（T04・D-10・D-197）: 生の id と JSON の代わりに、どの
/// ワークフローのどの版を・誰が作ったものを・どんな入力で試し、実行すると何が起きるかを読める形で見せる。
///
/// 中身は下見の口（`GET /workflows/try-preview`）を承認する人本人の権限で読む。起きること（`effects`）はサーバーが
/// IR から導いたもので、ここに対応表を持たない（ブロックの名前は生成したカタログから引く）。表示のための読み取りで、
/// 実行してよいかは起動の入口が承認の後に改めて確かめる。そのため、試運転できるか（`can_try`）は読んだ時点のもので、
/// 画面に戻ってきたとき（focus・visibilitychange）に読み直す。閲覧のみでも「実行されない」と言い切らず、実データの
/// 注意は常に出す（承認までに編集できるようになれば実行される）。フォルダ・ファイルの名前はサーバーが引かないので、
/// ここで本人の権限で引く（読めなければ名前を出さない）。

import {
  Bell,
  CircleAlert,
  FileText,
  Folder,
  Globe,
  Loader2,
  RotateCw,
  ShieldCheck,
  Sparkles,
  Table2,
  Terminal,
  TriangleAlert,
  Workflow,
  type LucideIcon,
} from "lucide-react";
import * as React from "react";

import { Skeleton } from "@/components/ui/skeleton";
import type { components } from "@/generated/api";
import { formatDateTime } from "@/lib/format";
import { breadcrumb } from "@/lib/storage";
import { cachedUserName, resolveUserNames } from "@/lib/user-names";
import { blockDisplayName } from "@/lib/workflow-block-name";
import { fetchTryPreview, type TryPreview } from "@/lib/workflow-api";
import { scopeLabel } from "@/lib/workflow-scopes";

type Schemas = components["schemas"];

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/// 一覧ごとに出す上限（超えた分は「ほか N 件」）。
const LIST_LIMIT = 6;
/// 入力の値 1 つを出す長さの上限。
const VALUE_CHARS = 160;
/// 画面に戻ってきたときに読み直す間隔の下限（ミリ秒）。
const RELOAD_GAP_MS = 1_000;

/// 道具が受ける入力（`crates/chat/src/workflow_try_tool`）。`input`・`trigger` は object か JSON の文字列。
type TryInput = {
  workflow_id?: unknown;
  version?: unknown;
  input?: unknown;
  trigger?: unknown;
  scheduled_at?: unknown;
  rerun?: unknown;
};

/// 版の読み方は道具（`TryRequest::parse`）と同じ: JSON の整数か、前後の空白と頭の v・V を除いて十進の整数として読める
/// 文字列（"2"・"v2"・"+2"）で、1 以上。"0x2"・"1e0"・"2.0"・"v 2" は道具が断るので、ここも読まない。
///
/// JSON の 2.0 は JS では 2 と区別できないので、ここは読めて道具が断ることがある。ほかにも、ここで読めて道具が断る
/// 書き方（予定の日時の読めない文字列など）は残る。そのときはカードが「試運転します」と出しても実行されない
/// （fail-closed）。逆に、道具が受けてここで読めない書き方は作らない（「実行されません」と言って実行されないように）。
function versionOf(v: unknown): number | null {
  if (typeof v === "number") return Number.isSafeInteger(v) && v >= 1 ? v : null;
  if (typeof v !== "string") return null;
  const s = v.trim().replace(/^[vV]+/, "");
  if (!/^\+?\d+$/.test(s)) return null;
  const n = Number(s);
  return Number.isSafeInteger(n) && n >= 1 ? n : null;
}

/// object か JSON の文字列の object を読む（道具の `accept_object_args` と同じ受け方）。読めなければ null。
function objectOf(v: unknown): Record<string, unknown> | null {
  let o = v;
  if (typeof v === "string") {
    try {
      o = JSON.parse(v);
    } catch {
      return null;
    }
  }
  return o && typeof o === "object" && !Array.isArray(o) ? (o as Record<string, unknown>) : null;
}

function clip(s: string, max = VALUE_CHARS): string {
  return s.length <= max ? s : `${s.slice(0, max)}…`;
}

function valueText(v: unknown): string {
  if (v === null || v === undefined || v === "") return "（空）";
  if (typeof v === "string") return clip(v);
  if (typeof v === "number" || typeof v === "boolean") return String(v);
  try {
    return clip(JSON.stringify(v));
  } catch {
    return clip(String(v));
  }
}

type State =
  | { kind: "loading" }
  | { kind: "ready"; preview: TryPreview; places: Map<string, string | null> }
  | { kind: "missing" }
  /// 名前で指定した（道具は名前では起動しない）。
  | { kind: "named" }
  /// 版を読めない（道具も起動しない）。
  | { kind: "badVersion" }
  | { kind: "failed" };

/// 書く先のフォルダ・ファイルの場所（id → 「親 / 自分」・読めなければ null）。
async function placesOf(preview: TryPreview): Promise<Map<string, string | null>> {
  const ids = [
    ...new Set(
      preview.effects.writes
        .filter((w) => w.target !== "table" && w.target_id && UUID_RE.test(w.target_id.trim()))
        .map((w) => w.target_id!.trim()),
    ),
  ].slice(0, LIST_LIMIT);
  const out = new Map<string, string | null>();
  await Promise.all(
    ids.map(async (id) => {
      const crumbs = await breadcrumb(id).catch(() => null);
      out.set(id, crumbs?.length ? crumbs.map((c) => c.name).join(" / ") : null);
    }),
  );
  return out;
}

export function TryWorkflowSummary({ input }: { input: unknown }) {
  const p = (input ?? {}) as TryInput;
  const workflowId = typeof p.workflow_id === "string" ? p.workflow_id.trim() : "";
  const version = versionOf(p.version);
  const runInput = objectOf(p.input ?? {});
  const trigger = p.trigger === undefined || p.trigger === null ? null : objectOf(p.trigger);
  const scheduledAt = typeof p.scheduled_at === "string" && p.scheduled_at.trim() ? p.scheduled_at.trim() : null;
  const rerun = p.rerun === true;
  // 道具が起動を断る入力（ワークフロー・版が無い・入力・きっかけ・予定の日時が読めない形）は、口を読まずにそう言う。
  // 読み方は道具（`TryRequest::parse`・`accept_object_args`）と同じ: 入力・きっかけは省く・null・object・object に
  // なる JSON の文字列を受け、予定の日時は省く・null・文字列を受ける。id かどうかは道具と同じ読み方をする口に任せる
  // （ここで UUID の書き方を決め打ちすると、道具が受ける書き方を「実行されません」と言い得る）。
  const missingArg: string | null = !workflowId
    ? "試運転するワークフローが指定されていないため、承認しても実行されません。"
    : version === null
      ? p.version === undefined || p.version === null
        ? "試運転する版が指定されていないため、承認しても実行されません。"
        : "試運転する版が正しく指定されていないため、承認しても実行されません。"
      : p.input != null && runInput === null
        ? "入力が読めない形で渡されているため、承認しても実行されません。"
        : p.trigger != null && trigger === null
          ? "きっかけの内容が読めない形で渡されているため、承認しても実行されません。"
          : p.scheduled_at != null && typeof p.scheduled_at !== "string"
            ? "予定の日時が読めない形で渡されているため、承認しても実行されません。"
            : null;

  const [state, setState] = React.useState<State>({ kind: "loading" });
  const [attempt, setAttempt] = React.useState(0);
  /// 最後に読みに行った時刻（focus と visibilitychange は続けて来るので、続けて読み直さない）。
  const loadedAt = React.useRef(0);
  const fetchable = !missingArg;
  React.useEffect(() => {
    if (!fetchable || version === null || !workflowId) return;
    let active = true;
    loadedAt.current = Date.now();
    void (async () => {
      try {
        const preview = await fetchTryPreview(workflowId, version);
        // 作成者・この版を保存した人は表示名で見せる（引けなければ id のまま）。
        const others = [preview.owner, preview.saved_by].filter((id) => !!id);
        const [places] = await Promise.all([placesOf(preview), resolveUserNames(others)]);
        if (active) setState({ kind: "ready", preview, places });
      } catch (e) {
        const { status, field } = e as { status?: number; field?: string | null };
        if (!active) return;
        if (status === 400 && field === "workflow") setState({ kind: "named" });
        else if (status === 400 && field === "version") setState({ kind: "badVersion" });
        else setState({ kind: status === 404 ? "missing" : "failed" });
      }
    })();
    return () => {
      active = false;
    };
  }, [fetchable, workflowId, version, attempt]);

  // 承認するかを決めるのは画面に戻ってきた後なので、戻ってきたら読み直す（開いている間に共有の権限が変わると、
  // 試運転できるかが変わる・起動の入口は承認の時点の権限で決める）。読み直す間は今の表示を残す。
  React.useEffect(() => {
    if (!fetchable) return;
    const reload = () => {
      if (document.visibilityState !== "visible") return;
      if (Date.now() - loadedAt.current < RELOAD_GAP_MS) return;
      loadedAt.current = Date.now();
      setAttempt((n) => n + 1);
    };
    window.addEventListener("focus", reload);
    document.addEventListener("visibilitychange", reload);
    return () => {
      window.removeEventListener("focus", reload);
      document.removeEventListener("visibilitychange", reload);
    };
  }, [fetchable]);

  const blocked =
    missingArg ??
    (state.kind === "named"
      ? `「${clip(workflowId, 60)}」は ID ではなく名前で指定されているため、承認しても実行されません。`
      : state.kind === "badVersion"
        ? "試運転する版が正しく指定されていないため、承認しても実行されません。"
        : null);
  if (blocked) {
    return (
      <div className="mb-2 space-y-2 text-[13px]" data-testid="try-workflow-summary" data-state="blocked">
        <Notice tone="danger" icon={CircleAlert}>
          {blocked}
        </Notice>
      </div>
    );
  }
  if (state.kind === "loading") {
    return (
      <div
        className="mb-2 space-y-2"
        data-testid="try-workflow-summary"
        data-state="loading"
        aria-busy="true"
      >
        <p className="flex items-center gap-1.5 text-[12px] text-muted-foreground">
          <Loader2 className="size-3.5 animate-spin" aria-hidden />
          試運転の内容を確かめています…
        </p>
        <SummarySkeleton />
      </div>
    );
  }
  if (state.kind === "missing") {
    return (
      <div className="mb-2 space-y-2 text-[13px]" data-testid="try-workflow-summary" data-state="missing">
        {/* 口はワークフローが無い・見えない・版が無いを同じ 404 にする（在否を明かさない）ので、言い分けない。 */}
        <Notice tone="danger" icon={CircleAlert}>
          ワークフローの v{version} を読めませんでした。
          <span className="mt-0.5 block text-[12px] font-normal text-muted-foreground">
            このワークフローか、その版が無いか、見る権限がありません。送る内容（JSON）を確かめてから決めてください。
          </span>
        </Notice>
      </div>
    );
  }
  if (state.kind === "failed") {
    return (
      <div className="mb-2 space-y-2 text-[13px]" data-testid="try-workflow-summary" data-state="failed">
        <Notice tone="danger" icon={CircleAlert}>
          試運転の内容を読み込めませんでした。
          <span className="mt-0.5 block text-[12px] font-normal text-muted-foreground">
            もう一度読み込むか、送る内容（JSON）を確かめてから決めてください。
          </span>
        </Notice>
        <button
          type="button"
          onClick={() => {
            setState({ kind: "loading" });
            setAttempt((n) => n + 1);
          }}
          className="inline-flex h-7 items-center gap-1.5 rounded-full border border-border px-2.5 text-[12px] text-foreground/80 hover:bg-secondary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <RotateCw className="size-3.5" aria-hidden />
          もう一度読み込む
        </button>
      </div>
    );
  }

  // 名前・読めない版は上の `blocked` で返している。
  if (state.kind !== "ready") return null;
  const { preview, places } = state;
  const person = (id: string, mine: boolean) => (mine ? "自分" : (cachedUserName(id) ?? id));
  // 読めない入力・きっかけは上の `blocked` で返している。
  const inputEntries = Object.entries(runInput ?? {});
  return (
    <div className="mb-2 space-y-2.5 text-[13px]" data-testid="try-workflow-summary" data-state="ready">
      {preview.can_try ? (
        <p className="text-foreground [overflow-wrap:anywhere]">
          ワークフロー「<b>{preview.name}</b>」の v{preview.version} を試運転します。
        </p>
      ) : (
        // 閲覧のみは起動の入口が断る。決め手なので先頭で言う（下は何を頼まれたかの記録として残す）。入口は承認の時点の
        // 権限で決めるので、「実行されない」とは言い切らない（承認までに編集できるようになれば実行される）。
        <Notice tone="danger" icon={CircleAlert} testId="try-workflow-view-only">
          今は閲覧のみのため、試運転できません。却下してください。
          <span className="mt-0.5 block text-[12px] font-normal text-muted-foreground">
            承認するまでに編集できるようになった場合は、承認すると実行されます。
          </span>
        </Notice>
      )}
      <dl className="divide-y rounded-lg border bg-card/60 text-[12px]">
        {preview.can_try ? null : (
          <Row label="ワークフロー">
            <span className="font-medium text-foreground">{preview.name}</span>
          </Row>
        )}
        <Row label="ID">
          <span className="font-mono text-[11px] text-foreground/80">{preview.workflow_id}</span>
        </Row>
        <Row label="版">
          <span className="font-medium text-foreground">v{preview.version}</span>
          {preview.latest_version > preview.version ? (
            <span className="text-muted-foreground">（最新は v{preview.latest_version}）</span>
          ) : null}
        </Row>
        <Row label="作成者">
          <span className="font-medium text-foreground" data-testid="try-workflow-owner">
            {person(preview.owner, preview.owned_by_me)}
          </span>
        </Row>
        {preview.saved_by !== preview.owner ? (
          <Row label="この版を保存">
            <span className="font-medium text-foreground">{person(preview.saved_by, preview.saved_by_me)}</span>
            <span className="text-muted-foreground">・{formatDateTime(preview.saved_at)}</span>
          </Row>
        ) : null}
      </dl>

      <Section title="入力">
        {inputEntries.length === 0 ? (
          <p className="text-[12px] text-muted-foreground">なし</p>
        ) : (
          <dl className="divide-y rounded-lg border bg-card/60 text-[12px]">
            {inputEntries.map(([k, v]) => (
              <Row key={k} label={k}>
                <span className="font-medium text-foreground">{valueText(v)}</span>
              </Row>
            ))}
          </dl>
        )}
        {trigger || scheduledAt ? (
          <dl className="mt-1.5 divide-y rounded-lg border bg-card/60 text-[12px]">
            {trigger ? (
              <Row label="きっかけ">
                <span className="font-medium text-foreground">{valueText(trigger)}</span>
              </Row>
            ) : null}
            {scheduledAt ? (
              <Row label="予定の日時">
                <span className="font-medium text-foreground">{clip(scheduledAt, 40)}</span>
              </Row>
            ) : null}
          </dl>
        ) : null}
        {rerun ? (
          <p className="mt-1 text-[12px] text-muted-foreground">同じ入力で試した結果があっても、もう一度実行します。</p>
        ) : null}
      </Section>

      <Section title="実行すると起きること">
        <Effects preview={preview} places={places} />
      </Section>

      {/* 閲覧のみでも出す（承認までに編集できるようになれば、承認すると実行される）。 */}
      <Notice tone="warn" icon={TriangleAlert} testId="try-workflow-real-data">
        結果は実データとして残ります。
        <span className="mt-0.5 block text-[12px] font-normal text-foreground/70">
          試運転でも、書き込み・送信・レコードの作成は本当に行われます。
        </span>
      </Notice>
    </div>
  );
}

// ── 起きること ─────────────────────────────────────────────

type Line = { key: string; icon: LucideIcon; main: React.ReactNode; sub?: string };

const TARGET_NOUN: Record<Schemas["EffectTarget"], string> = { table: "テーブル", folder: "フォルダ", file: "ファイル" };

const OUTBOUND_LABEL: Record<Schemas["OutboundKind"], string> = {
  http_request: "外部のサービスに送る",
  web_search: "web で検索する（検索する語が検索サービスに送られます）",
  web_fetch: "web のページを読む",
};

const COMPUTE_LABEL: Record<Schemas["ComputeKind"], string> = {
  script: "スクリプトを実行する",
  agent_code: "AI がコードを書いて実行する",
  skill: "スキルを使う",
};

/// スキルの参照（`skill:<名前>@<版>`・保存のときの検証で形が決まる）は、エディタと同じ「名前（v版）」で出す。
const SKILL_REF_RE = /^skill:([^@]+)@(.+)$/;

function computeText(c: Schemas["ComputeEffect"]): string {
  const m = c.kind === "skill" && c.skill ? SKILL_REF_RE.exec(c.skill) : null;
  return m ? `スキル「${m[1]}」（v${m[2]}）を使う` : COMPUTE_LABEL[c.kind];
}

function writeLine(w: Schemas["WriteEffect"], i: number, places: Map<string, string | null>): Line {
  const noun = TARGET_NOUN[w.target];
  const id = w.target_id?.trim() ?? "";
  const block = blockDisplayName({ type: w.node_type, label: null });
  let main: React.ReactNode;
  let unreadable = false;
  if (w.target === "table" && w.target_name) {
    main = (
      <>
        テーブル「<b>{w.target_name}</b>」
      </>
    );
  } else if (w.target !== "table" && id && places.get(id)) {
    main = (
      <>
        {noun}「<b>{places.get(id)}</b>」
      </>
    );
  } else if (id) {
    // 名前は出さない（本人が見られないものの名前は口も返さない）。
    main = `読めない${noun}`;
    unreadable = true;
  } else if (w.decided_at_run) {
    main = `入力や前の結果で決まる${noun}`;
  } else {
    main = w.target === "folder" ? "ドライブのトップ" : `${noun}（未設定）`;
  }
  return {
    key: `w${i}`,
    icon: w.target === "table" ? Table2 : w.target === "folder" ? Folder : FileText,
    main,
    sub: unreadable ? `${block}・無いか、見る権限がありません` : block,
  };
}

function Effects({ preview, places }: { preview: TryPreview; places: Map<string, string | null> }) {
  const e = preview.effects;
  const writes = e.writes.map((w, i) => writeLine(w, i, places));
  const outbound: Line[] = e.outbound.map((o, i) => {
    const where = o.hosts.length
      ? o.hosts.join("、")
      : o.kind === "web_search"
        ? "検索サービス"
        : o.decided_at_run
          ? "実行のときに決まる送り先"
          : "送り先（未設定）";
    const sub = [
      OUTBOUND_LABEL[o.kind],
      o.kind === "http_request" && o.method ? o.method.toUpperCase() : null,
      o.secret ? `シークレット「${o.secret}」を付ける` : null,
    ]
      .filter(Boolean)
      .join("・");
    return { key: `o${i}`, icon: Globe, main: <b>{where}</b>, sub };
  });
  const compute: Line[] = e.compute.map((c, i) => ({ key: `c${i}`, icon: Terminal, main: computeText(c) }));
  // スクリプト・スキルの中の操作（`Shiki.*` の書き込み・送信など）は上の一覧に出ないので、使ってよい操作の天井
  // （宣言スコープ）を出す。スキルは、さらにスキルの宣言で狭まる。
  const hasScript = e.compute.some((c) => c.kind === "script");
  const hasSkill = e.compute.some((c) => c.kind === "skill");
  const scopes: Line[] =
    hasScript || hasSkill
      ? e.declared_scopes.map((s) => ({ key: `d${s}`, icon: ShieldCheck, main: scopeLabel(s) }))
      : [];
  const scopesNote = hasScript
    ? hasSkill
      ? "スクリプトとスキルは、あなたの権限で次の操作を行えます（スキルは、この中でスキルが決めた範囲だけ）。書く先や送り先は、中身で決まります。"
      : "スクリプトは、あなたの権限で次の操作を行えます。書く先や送り先は、スクリプトの中身で決まります。"
    : "スキルは、あなたの権限で、次の操作のうちスキルが決めた範囲を行えます。書く先や送り先は、スキルの中身で決まります。";
  const starts: Line[] = e.starts.map((s, i) => {
    const found = s.workflow_name ? preview.starts.find((t) => t.name === s.workflow_name) : undefined;
    return {
      key: `s${i}`,
      icon: Workflow,
      main: s.workflow_name ? (
        <>
          ワークフロー「<b>{s.workflow_name}</b>」
        </>
      ) : (
        "入力や前の結果で決まるワークフロー"
      ),
      sub: s.workflow_name && !found?.workflow_id ? "見つからないか、見る権限がありません" : undefined,
    };
  });
  const models: Line[] = e.models.map((m, i) => ({
    key: `m${i}`,
    icon: Sparkles,
    main: m.model ? <b>{m.model}</b> : "組織の既定のモデル",
    sub: blockDisplayName({ type: m.node_type, label: null }),
  }));
  const notices: Line[] = e.notifications.map((n, i) => ({
    key: `n${i}`,
    icon: Bell,
    main: blockDisplayName({ type: n.node_type, label: null }),
  }));
  const others: Line[] = e.others.map((o, i) => ({
    key: `x${i}`,
    icon: CircleAlert,
    main: blockDisplayName({ type: o.node_type, label: null }),
  }));
  // 中でできることは天井なので、上限で切らずに全部出す（`all`）。
  const groups: { title: string; lines: Line[]; note?: string; testId?: string; all?: boolean }[] = [
    { title: "書き込む先", lines: writes },
    { title: "外への送信", lines: outbound },
    { title: "コード・スキルの実行", lines: compute },
    { title: "スクリプト・スキルの中でできること", lines: scopes, note: scopesNote, testId: "try-workflow-scopes", all: true },
    { title: "起動するワークフロー", lines: starts },
    { title: "使う AI のモデル（費用がかかります）", lines: models },
    { title: "人へのお知らせ", lines: notices },
    { title: "ほかの操作", lines: others },
  ].filter((g) => g.lines.length > 0);
  if (groups.length === 0) {
    return <p className="text-[12px] text-muted-foreground">読み取りだけで、書き込みや外への送信はありません。</p>;
  }
  return (
    <div className="space-y-1.5" data-testid="try-workflow-effects">
      {groups.map((g) => (
        <div key={g.title} data-testid={g.testId}>
          <p className="text-[11px] font-medium text-muted-foreground">{g.title}</p>
          {g.note ? <p className="text-[11px] text-muted-foreground">{g.note}</p> : null}
          <ul className="mt-0.5 space-y-0.5">
            {(g.all ? g.lines : g.lines.slice(0, LIST_LIMIT)).map((l) => (
              <li key={l.key} className="flex items-start gap-1.5 text-[12px]">
                <l.icon className="mt-[2px] size-3.5 shrink-0 text-muted-foreground" aria-hidden />
                <span className="min-w-0 flex-1 [overflow-wrap:anywhere]">
                  <span className="text-foreground">{l.main}</span>
                  {l.sub ? <span className="text-muted-foreground">・{l.sub}</span> : null}
                </span>
              </li>
            ))}
            {!g.all && g.lines.length > LIST_LIMIT ? (
              <li className="pl-5 text-[12px] text-muted-foreground">ほか {g.lines.length - LIST_LIMIT} 件</li>
            ) : null}
          </ul>
        </div>
      ))}
    </div>
  );
}

// ── 部品 ───────────────────────────────────────────────────

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-start gap-2 px-2.5 py-1.5">
      <dt className="w-24 shrink-0 truncate text-muted-foreground" title={label}>
        {label}
      </dt>
      <dd className="min-w-0 flex-1 [overflow-wrap:anywhere]">{children}</dd>
    </div>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section aria-label={title}>
      <h4 className="mb-1 text-[12px] font-semibold text-foreground/80">{title}</h4>
      {children}
    </section>
  );
}

function Notice({
  tone,
  icon: Icon,
  testId,
  children,
}: {
  tone: "warn" | "danger";
  icon: LucideIcon;
  testId?: string;
  children: React.ReactNode;
}) {
  return (
    <div
      data-testid={testId}
      className={
        tone === "warn"
          ? "flex items-start gap-2 rounded-lg border border-amber-500/40 bg-amber-500/10 px-2.5 py-2 text-[13px] font-medium text-amber-800 dark:text-amber-200"
          : "flex items-start gap-2 rounded-lg border border-destructive/40 bg-destructive/10 px-2.5 py-2 text-[13px] font-medium text-destructive"
      }
    >
      <Icon className="mt-[2px] size-4 shrink-0" aria-hidden />
      <div className="min-w-0 flex-1 [overflow-wrap:anywhere]">{children}</div>
    </div>
  );
}

/// 読み込み中の骨組み（読み込み後の並びと同じ高さの見当）。
function SummarySkeleton() {
  return (
    <div className="space-y-2" aria-hidden>
      <Skeleton className="h-4 w-3/4" />
      <Skeleton className="h-16 w-full rounded-lg" />
      <Skeleton className="h-4 w-1/3" />
      <Skeleton className="h-10 w-full rounded-lg" />
    </div>
  );
}
