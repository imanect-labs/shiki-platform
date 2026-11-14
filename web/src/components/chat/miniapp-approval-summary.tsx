"use client";

/// ミニアプリを直す道具（`miniapp_update`・D-10 の T08）の承認カードの中身。
///
/// 生の JSON の代わりに、どのアプリのどの版を直すか・公開中の版と使う人への影響・説明と画面（ボタンの実行先・入力の
/// キー・表）の変わり方・束ねる部品の版の変わり方を読める形で出す。今の版は承認する人本人の権限で読む（読めなければ
/// 差を出さない）。表示のための読み取りで、直してよいかはサーバが保存のときに改めて確かめる（編集できる人だけ・版の
/// 楽観ロック・部品の viewer）。画面の読み方は組み立てと同じ（`splitScreen`）。

import { Loader2 } from "lucide-react";
import * as React from "react";

import { getArtifact, resolveMiniApp, type MiniAppBody } from "@/lib/artifact-api";
import { splitScreen } from "@/lib/miniapp/screen-blocks";

type Obj = Record<string, unknown>;
type Pin = MiniAppBody["workflows"][number];

const isObj = (v: unknown): v is Obj => Boolean(v) && typeof v === "object" && !Array.isArray(v);
const asStr = (v: unknown): string | null => (typeof v === "string" && v.trim() ? v : null);
const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
/// 一覧ごとに出す上限（超えた分は「ほか N 件」）。
const LIST_LIMIT = 8;

/// 画面から読んだボタン（`ラベル → 実行先`）・入力のキー・表の別名。
type ScreenParts = { buttons: string[]; inputs: string[]; grids: string[] };

function screenParts(screen: string): ScreenParts {
  const out: ScreenParts = { buttons: [], inputs: [], grids: [] };
  for (const b of splitScreen(screen)) {
    if (b.type === "grid") out.grids.push(b.alias);
    if (b.type !== "app") continue;
    for (const s of b.figure.sections) {
      for (const f of s.fields) out.inputs.push(`${f.key}${f.required ? "（必須）" : ""}`);
      for (const a of s.actions) out.buttons.push(`${a.label} → ${a.target.kind === "workflow" ? a.target.name : "会話"}`);
    }
  }
  return out;
}

/// 前と後の差（足したもの・外したもの）。
function diff(before: string[], after: string[]) {
  return { added: after.filter((x) => !before.includes(x)), removed: before.filter((x) => !after.includes(x)) };
}

/// 入力の部品 1 つ（`{alias, workflow|view, version?}`）。
type PinInput = { alias: string; target: string; version: number | null };

function pinsOf(v: unknown, key: "workflow" | "view"): PinInput[] | null {
  if (!Array.isArray(v)) return null;
  return v.filter(isObj).map((p) => ({
    alias: String(p.alias ?? ""),
    target: String(p[key] ?? ""),
    version: typeof p.version === "number" ? p.version : null,
  }));
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="space-y-1">
      <h4 className="text-[11px] font-medium text-muted-foreground">{title}</h4>
      {children}
    </section>
  );
}

function Lines({ items, tone }: { items: string[]; tone?: "add" | "remove" }) {
  if (!items.length) return null;
  const mark = tone === "add" ? "＋" : tone === "remove" ? "－" : "・";
  const color =
    tone === "add" ? "text-emerald-700 dark:text-emerald-300" : tone === "remove" ? "text-rose-700 dark:text-rose-300" : "";
  return (
    <ul className="space-y-0.5 text-[12px]">
      {items.slice(0, LIST_LIMIT).map((x, i) => (
        <li key={`${x}-${i}`} className={`[overflow-wrap:anywhere] ${color}`}>
          {mark} {x}
        </li>
      ))}
      {items.length > LIST_LIMIT ? <li className="text-muted-foreground">ほか {items.length - LIST_LIMIT} 件</li> : null}
    </ul>
  );
}

/// 束ねる部品の変わり方（今のピンと入力を比べる）。名前は承認する人に見えるものだけ引く。
function PinChanges({ title, current, next }: { title: string; current: Pin[]; next: PinInput[] }) {
  const [names, setNames] = React.useState<Record<string, string>>({});
  const ids = React.useMemo(
    () => [...new Set([...current.map((p) => p.artifact_id), ...next.map((p) => p.target).filter((t) => UUID_RE.test(t))])],
    [current, next],
  );
  React.useEffect(() => {
    let active = true;
    void Promise.all(ids.map((id) => getArtifact(id).then((a) => [id, a.name] as const, () => null))).then((rows) => {
      if (active) setNames(Object.fromEntries(rows.filter((r): r is readonly [string, string] => r !== null)));
    });
    return () => {
      active = false;
    };
  }, [ids]);
  const label = (target: string) => names[target] ?? (UUID_RE.test(target) ? "（名前を読めない部品）" : target);
  const rows = next.map((p) => {
    const before = current.find((c) => c.artifact_id === p.target || names[c.artifact_id] === p.target);
    const v = p.version ?? before?.version ?? null;
    const where = `「${p.alias}」（${label(p.target)}）`;
    if (!before) return { text: `${where} を新しく束ねる${v != null ? `（v${v}）` : ""}`, tone: "add" as const };
    if (v != null && v !== before.version) return { text: `${where} を v${before.version} → v${v}`, tone: undefined };
    if (before.alias !== p.alias) return { text: `「${before.alias}」の別名を「${p.alias}」に変える`, tone: undefined };
    return null;
  });
  const removed = current
    .filter((c) => !next.some((p) => p.target === c.artifact_id || p.target === names[c.artifact_id]))
    .map((c) => `「${c.alias}」（${label(c.artifact_id)}）を外す`);
  const changed = rows.filter((r): r is NonNullable<typeof r> => r !== null);
  if (!changed.length && !removed.length) return null;
  return (
    <Section title={title}>
      <Lines items={changed.filter((r) => r.tone === "add").map((r) => r.text)} tone="add" />
      <Lines items={changed.filter((r) => !r.tone).map((r) => r.text)} />
      <Lines items={removed} tone="remove" />
    </Section>
  );
}

type Loaded =
  | { loading: true }
  | { loading: false; name: string | null; latest: number | null; published: number | null; body: MiniAppBody | null };

/// ミニアプリを直す（`miniapp_update`）承認カードの中身。
export function MiniappUpdateSummary({ input }: { input: unknown }) {
  const p = isObj(input) ? input : {};
  const appId = asStr(p.app);
  const version = typeof p.expected_version === "number" ? p.expected_version : null;
  const [state, setState] = React.useState<Loaded>({ loading: true });
  React.useEffect(() => {
    let active = true;
    void (async () => {
      const id = appId && UUID_RE.test(appId) ? appId : null;
      const meta = id ? await getArtifact(id).catch(() => null) : null;
      const app = id && version != null ? await resolveMiniApp(id, version).catch(() => null) : null;
      if (active)
        setState({
          loading: false,
          name: meta?.name ?? null,
          latest: meta?.currentVersion ?? null,
          published: meta?.publishedVersion ?? app?.published_version ?? null,
          body: app?.body ?? null,
        });
    })();
    return () => {
      active = false;
    };
  }, [appId, version]);
  if (state.loading) {
    return (
      <p className="mb-2 flex items-center gap-1.5 text-[12px] text-muted-foreground" aria-busy="true">
        <Loader2 className="size-3.5 animate-spin" aria-hidden />
        今のアプリを読み込んでいます…
      </p>
    );
  }
  const body = state.body;
  const description = typeof p.description === "string" ? p.description.trim() : null;
  const screen = typeof p.screen === "string" ? p.screen : null;
  const workflows = pinsOf(p.workflows, "workflow");
  const views = pinsOf(p.data_views, "view");
  const before = body ? screenParts(body.screen) : null;
  const after = screen != null ? screenParts(screen) : null;
  const buttons = before && after ? diff(before.buttons, after.buttons) : null;
  const inputs = before && after ? diff(before.inputs, after.inputs) : null;
  const grids = before && after ? diff(before.grids, after.grids) : null;
  const screenStructChanged = [buttons, inputs, grids].some((d) => d && (d.added.length || d.removed.length));
  return (
    <div className="mb-2 space-y-2 text-[13px]" data-testid="miniapp-update-summary">
      <p className="text-foreground [overflow-wrap:anywhere]">
        {state.name ? (
          <>
            ミニアプリ「<b>{state.name}</b>」を直します{version != null ? `（v${version} の次の版）` : ""}。
          </>
        ) : (
          "ミニアプリを直します。"
        )}
      </p>
      <p className="text-[11px] text-muted-foreground">
        {state.published != null
          ? `保存は下書きになります。使う人に出ているのは公開中の v${state.published} のままで、アプリの画面で公開するまで変わりません。`
          : "保存は下書きになります。まだ公開していないので、使えるのは編集できる人だけです。"}
      </p>
      {!body ? (
        <p className="text-[11px] text-muted-foreground">
          今のアプリを読めなかったため、今の版との差は表示していません（直せるのは編集できる人だけです）。
        </p>
      ) : version !== null && state.latest !== null && version !== state.latest ? (
        <p className="text-[11px] text-amber-700 dark:text-amber-300">
          AI が読んだ後にアプリが変わっています（今は v{state.latest}）。承認しても保存されず、読み直しになります。
        </p>
      ) : null}
      {description != null && description !== body?.description ? (
        <Section title="説明">
          <p className="text-[12px] [overflow-wrap:anywhere]">
            {body ? <span className="text-muted-foreground line-through">{body.description}</span> : null}
            {body ? " → " : ""}
            {description}
          </p>
        </Section>
      ) : null}
      {screen != null && screen !== body?.screen ? (
        <Section title="画面">
          {buttons ? (
            <>
              <Lines items={buttons.added.map((b) => `ボタン ${b}`)} tone="add" />
              <Lines items={buttons.removed.map((b) => `ボタン ${b}`)} tone="remove" />
            </>
          ) : null}
          {inputs ? (
            <>
              <Lines items={inputs.added.map((x) => `入力 ${x}`)} tone="add" />
              <Lines items={inputs.removed.map((x) => `入力 ${x}`)} tone="remove" />
            </>
          ) : null}
          {grids ? (
            <>
              <Lines items={grids.added.map((g) => `表 ${g}`)} tone="add" />
              <Lines items={grids.removed.map((g) => `表 ${g}`)} tone="remove" />
            </>
          ) : null}
          {!body ? (
            <p className="text-[12px]">画面を書き換えます。</p>
          ) : !screenStructChanged ? (
            <p className="text-[12px]">文言・並びだけを変えます（ボタンの実行先・入力・表は同じ）。</p>
          ) : null}
        </Section>
      ) : null}
      {workflows ? (
        <PinChanges title="束ねるワークフロー（ボタンの実行先）" current={body?.workflows ?? []} next={workflows} />
      ) : null}
      {views ? <PinChanges title="束ねる保存ビュー（表）" current={body?.data_views ?? []} next={views} /> : null}
    </div>
  );
}
