import { createHash } from "node:crypto";

import { expect, type Page } from "@playwright/test";

import {
  cancelActiveRunsApi,
  cleanupAll,
  createFolderApi,
  deleteArtifactApi,
  deleteTableApi,
  pinStubOrgDefault,
  stubChatModelId,
  stubDecisionModel,
  stubLlmDecisionModel,
  trashFileApi,
  trashFolderApi,
  writeFgaTuples,
  type FgaTuple,
} from "../helpers";
import { call, ok, type Json } from "./http";
import type { People, PersonName } from "./people";

/// APP の「世界」（表・部署・フォルダー・ファイル・ビュー・ワークフロー・アプリ・組織の AI 設定）をデータで書き、
/// alice の画面から 1 度に組み、片付ける（decide-scenarios.ts の instantiate を一般化したもの・そちらは変えない）。
///
/// 定義の中の文字列には置き換え記号を書ける（組んだ順に値が決まる。前に組んだものだけを指せる）:
/// - `{{table.x}}`・`{{view.x}}`・`{{folder.x}}`・`{{file.x}}`・`{{app.x}}` … 作ったものの id
/// - `{{wf.x}}` … ワークフローの名前（IR の name・`workflow.start` で使う）、`{{wfid.x}}` … その id
/// - `{{role.x}}` … spec ごとに一意な部署（FGA だけの tag・`has_role`・状態の進め方・承認の担当に使う）。
///   role_ref の列と表の部署への共有には使えない（ディレクトリに載っていない・GAP-76）。人の realm の部署を使う。
/// - `{{user.alice}}`・`{{user.bob}}`・`{{user.charlie}}` … その人の id
/// - `{{model.chat}}`（stub の chat モデル）・`{{model.decide}}`（stub の決定器）・`{{model.llmdecide}}`（stub の chat に
///   判定を真似させる決定器）… `ai` で頼んだものだけ
/// - `{{uniq}}` … この組み立ての一意な印（CI の流し直しでぶつからないキーに使う）
///
/// 組む順: 組織の AI 設定 → 部署 → フォルダー → ファイル → 表（全部作ってから、状態の進め方・行・共有）→ ビュー →
/// ワークフロー（作成 → 同意計画の照合 → 有効化）→ アプリ（作成 → 公開 → 共有）。
/// 片付けの順: 動いている run を止める → アプリ → ワークフロー → モデルの設定 → ビュー → 状態の進め方 → 表 →
/// ファイルとフォルダー → タプル（どれかが落ちても残りを走らせる）。途中で落ちても、作った分だけ片付けてから投げ直す。

export type ShareRole = "viewer" | "editor";
export type TableShareRole = "viewer" | "editor" | "submitter";

export type FsmTransitionDef = { from: string; to: string; actor: unknown };

export type FsmDef = {
  status_field: string;
  /// 先頭が最初の状態（状態の列の選択肢もこの順になる）。
  states: string[];
  transitions: FsmTransitionDef[];
  /// 「最初の状態を過ぎたら申請者は書き換えられない」決まりの申請者の列（メンバーの列）。
  applicant_write_lock?: string;
  /// 付け方（既定 `table` = `PUT /data/tables/{id}/fsm`・applicant_write_lock を付けられる。`artifact` =
  /// `POST /data/fsms` ＋ スキーマの fsm_ref・テンプレートから組む APP-01・02 用）。
  via?: "table" | "artifact";
};

export type TableDef = {
  ref: string;
  /// 表示名（一意な印を後ろに足して作る）。
  name: string;
  /// スキーマ（fields・status_field・row_policy・field_policy・aggregate_min_rows・rag）。
  schema: Json;
  fsm?: FsmDef;
  /// 作る行（状態の進め方を付けた後に、持ち主が作る。状態の列は最初の状態で作る）。
  rows?: Json[];
  shares?: { user: PersonName; role: TableShareRole }[];
};

export type RoleDef = { ref: string; members: PersonName[] };

export type FolderDef = {
  ref: string;
  name: string;
  /// 親のフォルダーの ref（省略でドライブのトップ）。
  parent?: string;
  shares?: { user: PersonName; role: ShareRole }[];
};

export type FileDef = { ref: string; folder: string; name: string; content: string; contentType?: string };

export type ViewDef = { ref: string; name: string; table: string; query: Json; display?: Json };

/// 有効化の委譲 1 件（`target` は置き換え記号か id）。
export type GrantDef = { scope: string; kind: "data_table" | "folder" | "file" | "workflow" | "secret"; target: string; relation: string };

export type WorkflowDef = {
  ref: string;
  /// IR（name は作るときに `campaign-<key>-<ref>-<uniq>` に置き換える・display_name はそのまま）。
  ir: Json;
  /// 有効化する（スケジュール・きっかけで動くもの）。同意計画が `grants` と食い違えば落とす。
  enable?: { grants: GrantDef[] };
};

export type AppDef = {
  ref: string;
  name: string;
  description?: string;
  /// 画面（Markdown ＋ ```ui ＋ ```data_grid）。
  screen: string;
  workflows: { alias: string; wf: string }[];
  views?: { alias: string; view: string }[];
  /// 公開する（既定 true・使う人が押せるのは公開した版だけ）。
  publish?: boolean;
  shares?: { user: PersonName; role: ShareRole }[];
};

export type AiDef = {
  /// stub の決定器（`{{model.decide}}`）を組織で使えるようにする（無効なら有効にして、片付けで戻す）。
  decide?: boolean;
  /// stub の chat に判定を真似させる決定器（`{{model.llmdecide}}`）。
  llmDecide?: boolean;
  /// 組織の既定のモデルを stub にする（`{{model.chat}}` も引く）。
  chat?: boolean;
};

export type WorldDef = {
  /// spec の印（`app11` など・名前と部署の tag に入る）。
  key: string;
  /// 一意な印（省略時は作る）。
  uniq?: string;
  ai?: AiDef;
  roles?: RoleDef[];
  folders?: FolderDef[];
  files?: FileDef[];
  tables?: TableDef[];
  views?: ViewDef[];
  workflows?: WorkflowDef[];
  apps?: AppDef[];
};

export type BuiltTable = { id: string; name: string; schema: Json; schemaVersion: number; fsmId?: string; rows: Record<string, unknown>[] };
export type BuiltWorkflow = { id: string; name: string; version: number; ir: Json; grants: GrantItem[]; enabled: boolean };
export type BuiltApp = {
  id: string;
  name: string;
  version: number;
  screen: string;
  /// ボタンの文言 → action_id（`{ブロックの識別子}:a{n}`）。同じ文言が 2 つあれば先の 1 つ。
  actions: Record<string, string>;
};

/// 有効化の本文の委譲 1 件。
export type GrantItem = { scope: string; object_type: string; object_id: string; relation: string };

export type World = {
  key: string;
  uniq: string;
  people: People;
  values: Record<string, string>;
  roles: Record<string, string>;
  folders: Record<string, { id: string; name: string }>;
  files: Record<string, { id: string; name: string }>;
  tables: Record<string, BuiltTable>;
  views: Record<string, { id: string; version: number; name: string }>;
  workflows: Record<string, BuiltWorkflow>;
  apps: Record<string, BuiltApp>;
  models: { chat?: string; decide?: string; llmDecide?: string };
  /// 置き換え記号を今の値で埋める。
  fill: <T>(v: T) => T;
  /// spec が足した片付け（cron の写しなど）。片付けの最初に、積んだ逆の順で走る。
  defer: (step: () => Promise<unknown>) => void;
  /// 内部: 片付けの材料。
  _undo: Undo;
  /// 内部: 組み立てを止める印（beforeAll の時間切れの後に画面を閉じるとき・[`buildWorld`]）。
  _abort: boolean;
};

type Undo = {
  deferred: (() => Promise<unknown>)[];
  models: (() => Promise<unknown>)[];
  tuples: (() => Promise<unknown>)[];
  fsms: string[];
};

const PLACEHOLDER = /\{\{(?:(table|view|wf|wfid|role|user|model|folder|file|app)\.([A-Za-z0-9_-]+)|uniq)\}\}/g;

/// 文字列の中の置き換え記号を `values` で埋める（値の全体でも一部でも・値の無い記号は止める）。
export function fillWith<T>(v: T, values: Record<string, string>): T {
  if (typeof v === "string") {
    return v.replace(PLACEHOLDER, (p) => {
      const x = values[p];
      if (x === undefined) throw new Error(`値の無い置き換え記号: ${p}（定義の順か ai の指定を確かめる）`);
      return x;
    }) as T;
  }
  if (Array.isArray(v)) return v.map((x) => fillWith(x, values)) as T;
  if (v && typeof v === "object") {
    return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, fillWith(x, values)])) as T;
  }
  return v;
}

/// 一意な印（小文字の英数字・ワークフロー名と FGA の tag に使える）。
export function uniqTag(): string {
  return `${Date.now().toString(36)}${Math.floor(Math.random() * 1296).toString(36).padStart(2, "0")}`;
}

/// ワークフロー名（`^[a-z][a-z0-9-]{0,63}$`）。
function workflowName(key: string, ref: string, uniq: string): string {
  const clean = (s: string) => s.toLowerCase().replace(/[^a-z0-9-]/g, "-");
  return `campaign-${clean(key)}-${clean(ref)}-${uniq}`.slice(0, 64);
}

/// 画面の ```ui ブロックのソース（フェンスの中・出てくる順）。
export function uiBlocks(screen: string): string[] {
  const out: string[] = [];
  const re = /(^|\n)```ui[^\n]*\n([\s\S]*?)\n```/g;
  for (let m = re.exec(screen); m; m = re.exec(screen)) out.push(m[2]);
  return out;
}

/// ブロックの識別子（crates/yui の block_hash と同じ規則: CRLF を LF にし、末尾の空白を落とした SHA-256 の先頭 16 桁）。
export function blockHash(source: string): string {
  return createHash("sha256").update(source.replace(/\r\n/g, "\n").trimEnd()).digest("hex").slice(0, 16);
}

const BUTTON = /^(?:[-*+]\s+)?(?:button|ボタン)\s+(.+?)\s*(?:->|→|=>)\s*(.+)$/i;

/// 画面のボタンの文言 → action_id（`{ブロックの識別子}:a{n}`・n はブロックの中のボタンの通し番号）。
/// 実際の id は spec が 1 度は画面から押して照らす（press.ts の pressInApp が送った id を返す）。
export function screenActions(screen: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const src of uiBlocks(screen)) {
    const hash = blockHash(src);
    let n = 0;
    for (const line of src.split("\n")) {
      const m = BUTTON.exec(line.trim());
      if (!m) continue;
      n += 1;
      const label = m[1].trim();
      if (!(label in out)) out[label] = `${hash}:a${n}`;
    }
  }
  return out;
}

type AdminModel = { id: string; enabled: boolean; modality: string; provider_id: string; decision_via?: string | null };

async function adminModels(page: Page): Promise<AdminModel[]> {
  return (await ok<{ models: AdminModel[] }>(page, "GET", "/org-admin/ai/models")).models;
}

/// 判定用のモデルを組織で使えるようにし、戻す関数を返す（既に使えるなら何もしない）。
async function enableModel(page: Page, m: AdminModel): Promise<() => Promise<void>> {
  if (m.enabled) return async () => {};
  await ok(page, "PUT", "/org-admin/ai/settings", { models: [{ id: m.id, enabled: true }] });
  return async () => {
    await ok(page, "PUT", "/org-admin/ai/settings", { models: [{ id: m.id, enabled: false }] });
  };
}

/// 有効化の委譲を、同意計画（`consent-plan`）と照らしてから有効化する。期待の委譲がどれも計画にあり（対象を選ぶ
/// 委譲は種類と関係が同じ）、計画にある確定した対象がどれも期待にあり、有効化を止める理由が無いこと。
export async function enableWithPlan(page: Page, workflowId: string, version: number, grants: GrantItem[]): Promise<void> {
  type Suggested = { scope: string; object_kind: string; object_id: string | null; relation: string; needs_user_pick: boolean };
  const plan = await ok<{ grants: Suggested[]; blockers: unknown[] }>(
    page,
    "GET",
    `/workflows/${workflowId}/versions/${version}/consent-plan`,
  );
  expect(plan.blockers, "有効化を止める理由が無い").toEqual([]);
  for (const g of grants) {
    const hit = plan.grants.some(
      (s) =>
        s.scope === g.scope &&
        s.object_kind === g.object_type &&
        s.relation === g.relation &&
        (s.object_id === g.object_id || s.needs_user_pick),
    );
    expect(hit, `同意計画に無い委譲: ${JSON.stringify(g)}（計画: ${JSON.stringify(plan.grants)}）`).toBe(true);
  }
  for (const s of plan.grants.filter((x) => x.object_id)) {
    const hit = grants.some((g) => g.scope === s.scope && g.object_id === s.object_id && g.relation === s.relation);
    expect(hit, `計画にあって委譲していない対象: ${JSON.stringify(s)}`).toBe(true);
  }
  await ok(page, "POST", `/workflows/${workflowId}/enable`, { version, grants, consent: true });
}

/// 止める（委譲も取り消す）。もう無ければ何もしない。
export async function disableWorkflow(page: Page, workflowId: string): Promise<void> {
  const r = await call(page, "POST", `/workflows/${workflowId}/disable`, { revoke_delegations: true });
  expect([200, 204, 404], `disable ${workflowId} → ${r.status} ${r.text}`).toContain(r.status);
}

/// ワークフローを片付ける: 止める → 動いている run を止める → 消す。
export async function removeWorkflow(page: Page, workflowId: string): Promise<void> {
  await cleanupAll(
    () => disableWorkflow(page, workflowId),
    () => cancelActiveRunsApi(page, workflowId),
    () => deleteArtifactApi(page, workflowId),
  );
}

/// 消し損ねて動き続けている `campaign-` のスケジュール（前の流しの残り）を止めて消す。Playwright の流しは 1 本ずつ
/// （stack.lock）なので、組み始めの時点で動いている `campaign-` のスケジュールはどれも残りものとして扱ってよい。
export async function sweepCampaignSchedules(page: Page): Promise<string[]> {
  type Summary = { id: string; name: string; trigger_kinds: string[]; enabled_status: string; updated_at: string };
  const swept: string[] = [];
  let before = "";
  for (let i = 0; i < 20; i++) {
    const r = await ok<{ items: Summary[]; owned_next?: { before_updated_at: string; before_id: string } | null }>(
      page,
      "GET",
      `/workflows?limit=100${before}`,
    );
    for (const w of r.items) {
      if (w.name.startsWith("campaign-") && w.trigger_kinds.includes("schedule") && w.enabled_status === "enabled") {
        await removeWorkflow(page, w.id);
        swept.push(w.name);
      }
    }
    const next = r.owned_next;
    if (!next) break;
    before = `&before_updated_at=${encodeURIComponent(next.before_updated_at)}&before_id=${next.before_id}`;
  }
  return swept;
}

/// 置くファイル。
export type DriveFile = { name: string; content: string | Buffer; mimeType?: string };

/// ドライブの画面からファイルをまとめて置き（ファイルの選択で複数を選ぶのと同じ・drive-reindex.spec.ts と同じ道）、
/// 名前 → id を返す。開き直した直後のドライブは、画面の準備（hydration）の前に選んだファイルを黙って落とすことが
/// ある（続けて置いた 2 つ目が出なかった・APP-04）ので、画面の準備を待ってから選び、出なければ置き直す（3 回まで）。
/// 置き直すのは、API で見てその名前のファイルがまだ無いものだけ（二重に置かない）。
export async function placeFilesViaDrive(page: Page, folderId: string, files: DriveFile[]): Promise<Record<string, string>> {
  const ids: Record<string, string> = {};
  const look = async () => {
    const r = await call<{ items?: { id: string; name: string }[] }>(page, "GET", `/nodes?parent_id=${folderId}&limit=200`);
    for (const c of r.body?.items ?? []) if (files.some((f) => f.name === c.name)) ids[c.name] = c.id;
    return files.every((f) => ids[f.name]);
  };
  for (let attempt = 0; attempt < 3; attempt++) {
    // 前の回の置き方が遅れて届いていれば、その分は置き直さない。
    if (attempt > 0 && (await look())) break;
    const missing = files.filter((f) => !ids[f.name]);
    await page.goto(`/drive?folder=${folderId}`);
    await page.waitForLoadState("networkidle", { timeout: 15_000 }).catch(() => undefined);
    const input = page.locator('input[type="file"][multiple]');
    await expect(input).toBeAttached({ timeout: 30_000 });
    await input.setInputFiles(
      missing.map((f) => ({
        name: f.name,
        mimeType: f.mimeType ?? "text/plain",
        buffer: typeof f.content === "string" ? Buffer.from(f.content, "utf8") : f.content,
      })),
    );
    const done = await expect
      .poll(look, { timeout: 30_000, intervals: [500, 1_000, 2_000] })
      .toBe(true)
      .then(() => true)
      .catch(() => false);
    if (done) break;
  }
  const lost = files.filter((f) => !ids[f.name]).map((f) => f.name);
  if (lost.length > 0) throw new Error(`ドライブの画面から置けなかった（3 回）: ${lost.join("・")}`);
  return ids;
}

/// ドライブの画面からファイルを 1 つ置き、id を返す（[`placeFilesViaDrive`]）。
export async function uploadFile(
  page: Page,
  folderId: string,
  name: string,
  content: string | Buffer,
  mimeType = "text/plain",
): Promise<string> {
  return (await placeFilesViaDrive(page, folderId, [{ name, content, mimeType }]))[name];
}

function emptyWorld(P: People, key: string, uniq: string): World {
  const values: Record<string, string> = { "{{uniq}}": uniq };
  for (const n of ["alice", "bob", "charlie"] as PersonName[]) {
    if (P[n]) values[`{{user.${n}}}`] = P[n].id;
  }
  const undo: Undo = { deferred: [], models: [], tuples: [], fsms: [] };
  const w: World = {
    key,
    uniq,
    people: P,
    values,
    roles: {},
    folders: {},
    files: {},
    tables: {},
    views: {},
    workflows: {},
    apps: {},
    models: {},
    fill: <T>(v: T) => fillWith(v, values),
    defer: (step) => undo.deferred.push(step),
    _undo: undo,
    _abort: false,
  };
  return w;
}

/// 組み立てを止める印が付いていれば止める（作った分は buildWorld が片付ける）。
function checkAbort(w: World): void {
  if (w._abort) throw new Error(`世界 ${w.key}-${w.uniq} の組み立てを止めた（beforeAll の時間切れの後に画面を閉じる）`);
}

/// 組み立てを止めてから片付けが終わるのを待つ上限（固まった呼び出しで afterAll の時間を使い切らない）。
const ABORT_WAIT_MS = 120_000;

/// 世界を組む（alice の画面で）。落ちたら作った分を片付けてから投げ直す。
///
/// beforeAll が組み立ての途中で時間切れになると、spec の `W` には何も入らず、afterAll の `if (W) cleanupWorld(W)` は
/// 片付けない（組み立ては裏で続き、画面を閉じた後に落ちて、閉じた画面での片付けも黙って失敗する）。そこで、画面を
/// 閉じる前の片付け（`People.onClose`）に、組み立てを止めて片付けが終わるのを待つ手順を足しておく。組み立てが
/// 終われば外す（それからは spec の afterAll が片付ける）。
export async function buildWorld(P: People, def: WorldDef): Promise<World> {
  const uniq = def.uniq ?? uniqTag();
  const w = emptyWorld(P, def.key, uniq);
  const run = (async () => {
    try {
      await build(w, def);
      return w;
    } catch (e) {
      await cleanupWorld(w).catch(() => undefined);
      throw e;
    }
  })();
  const unhook = P.onClose(async () => {
    w._abort = true;
    const settled = run.then(
      () => cleanupWorld(w).catch(() => undefined),
      () => undefined,
    );
    let timer: ReturnType<typeof setTimeout> | undefined;
    const limit = new Promise((resolve) => {
      timer = setTimeout(resolve, ABORT_WAIT_MS);
    });
    await Promise.race([settled, limit]);
    clearTimeout(timer);
  });
  try {
    return await run;
  } finally {
    unhook();
  }
}

async function build(w: World, def: WorldDef): Promise<void> {
  const page = w.people.alice.page;
  const tenant = w.people.alice.tenant;
  await sweepCampaignSchedules(page);
  checkAbort(w);

  if (def.ai?.chat) {
    w._undo.models.push(await pinStubOrgDefault(page));
    w.models.chat = await stubChatModelId(page);
    w.values["{{model.chat}}"] = w.models.chat;
  }
  if (def.ai?.decide || def.ai?.llmDecide) {
    const models = await adminModels(page);
    if (def.ai.decide) {
      const m = await stubDecisionModel(page, models);
      if (!m) throw new Error("stub の決定器（判定用のモデル）がカタログに無い構成（BUG-86 の F3 を確かめる）");
      w._undo.models.push(await enableModel(page, m));
      w.models.decide = m.id;
      w.values["{{model.decide}}"] = m.id;
    }
    if (def.ai.llmDecide) {
      const m = await stubLlmDecisionModel(page, models);
      if (!m) throw new Error("stub の chat に判定を真似させる決定器がカタログに無い構成");
      w._undo.models.push(await enableModel(page, m));
      w.models.llmDecide = m.id;
      w.values["{{model.llmdecide}}"] = m.id;
    }
  }

  // 部署: spec ごとに一意な tag の FGA の member タプル（approvals-many-roles.spec.ts と同じ形）。
  const tuples: FgaTuple[] = [];
  for (const r of def.roles ?? []) {
    const name = `c${def.key}${w.uniq}-${r.ref}`.toLowerCase().replace(/[^a-z0-9-]/g, "-");
    w.roles[r.ref] = name;
    w.values[`{{role.${r.ref}}}`] = name;
    for (const m of r.members) {
      tuples.push({ user: `user:${tenant}|${w.people[m].id}`, relation: "member", object: `role:${tenant}|${name}` });
    }
  }
  if (tuples.length > 0) {
    w._undo.tuples.push(await writeFgaTuples(page, tuples));
    await verifyRoles(w, def.roles ?? []);
  }

  for (const f of def.folders ?? []) {
    checkAbort(w);
    const parent = f.parent ? w.folders[f.parent]?.id : null;
    if (f.parent && !parent) throw new Error(`フォルダー ${f.ref} の親 ${f.parent} が先に無い`);
    const name = `${f.name}-${w.uniq}`;
    const id = await createFolderApi(page, name, parent ?? null);
    w.folders[f.ref] = { id, name };
    w.values[`{{folder.${f.ref}}}`] = id;
    for (const s of f.shares ?? []) {
      await ok(page, "PUT", `/nodes/${id}/shares`, { target: { type: "user", id: w.people[s.user].id }, role: s.role });
    }
  }
  for (const f of def.files ?? []) {
    checkAbort(w);
    const folder = w.folders[f.folder];
    if (!folder) throw new Error(`ファイル ${f.ref} のフォルダー ${f.folder} が無い`);
    const id = await uploadFile(page, folder.id, f.name, w.fill(f.content), f.contentType);
    w.files[f.ref] = { id, name: f.name };
    w.values[`{{file.${f.ref}}}`] = id;
  }

  await buildTables(w, def.tables ?? []);

  for (const v of def.views ?? []) {
    checkAbort(w);
    const table = w.tables[v.table];
    if (!table) throw new Error(`ビュー ${v.ref} の表 ${v.table} が無い`);
    const name = `${v.name}-${w.uniq}`;
    const body: Json = { name, table_id: table.id, query: w.fill(v.query) };
    if (v.display) body.display = w.fill(v.display);
    const made = await ok<{ id: string; version?: number }>(page, "POST", "/data/views", body);
    w.views[v.ref] = { id: made.id, version: made.version ?? 1, name };
    w.values[`{{view.${v.ref}}}`] = made.id;
  }

  for (const d of def.workflows ?? []) {
    checkAbort(w);
    const name = workflowName(def.key, d.ref, w.uniq);
    w.values[`{{wf.${d.ref}}}`] = name;
    const ir = { ...w.fill(d.ir), name };
    const made = await ok<{ id: string; version?: number }>(page, "POST", "/workflows", { ir });
    const version = made.version ?? 1;
    const grants: GrantItem[] = (d.enable?.grants ?? []).map((g) => ({
      scope: g.scope,
      object_type: g.kind,
      object_id: w.fill(g.target),
      relation: g.relation,
    }));
    w.workflows[d.ref] = { id: made.id, name, version, ir, grants, enabled: false };
    w.values[`{{wfid.${d.ref}}}`] = made.id;
    if (d.enable) {
      await enableWithPlan(page, made.id, version, grants);
      w.workflows[d.ref].enabled = true;
    }
  }

  for (const a of def.apps ?? []) {
    checkAbort(w);
    await buildApp(w, a);
  }
}

/// 直接書いた部署のタプルが、サーバーの行の決まり（`has_role`）に効いていることを確かめる。タプルはサーバーと同じ
/// OpenFGA の store に書く必要があり、store の名前は `E2E_OPENFGA_STORE`（既定 `shiki`）で決まる。名前が食い違うと
/// タプルは別の store に入り、部署の判定がどれも黙って不成立になる（行が見えない・遷移が断られる）ので、組む前に止める。
/// 部署ごとに「その部署の人にだけ見える行」を 1 行置いた仮の表を作り、最初の member の権限で読んで確かめ、消す。
async function verifyRoles(w: World, roles: RoleDef[]): Promise<void> {
  const page = w.people.alice.page;
  const probe = roles.filter((r) => r.members.length > 0);
  if (probe.length === 0) return;
  const t = await ok<{ id: string }>(page, "POST", "/data/tables", {
    name: `campaign-role-probe-${w.uniq}`,
    schema: {
      fields: [{ key: "k", display_name: "部署", type: "text", indexed: true }],
      row_policy: {
        read: {
          any: probe.map((r) => ({
            all: [{ field_cmp: { field: "k", op: "eq", value: { lit: r.ref } } }, { has_role: { role: w.roles[r.ref], subtree: true } }],
          })),
        },
        write: "public",
      },
    },
  });
  try {
    for (const r of probe) await ok(page, "POST", `/data/tables/${t.id}/records`, { data: { k: r.ref } }, [201]);
    const shared = new Set<PersonName>();
    for (const r of probe) {
      const m = r.members[0];
      if (m !== "alice" && !shared.has(m)) {
        await ok(page, "PUT", `/data/tables/${t.id}/shares`, { target: { type: "user", id: w.people[m].id }, role: "viewer" }, [204]);
        shared.add(m);
      }
      const seen = await ok<{ items?: { data: { k?: string } }[] }>(w.people[m].page, "POST", `/data/tables/${t.id}/query`, { limit: 50 });
      expect(
        (seen.items ?? []).some((x) => x.data.k === r.ref),
        `部署 ${r.ref}（${w.roles[r.ref]}）のタプルがサーバーに効いていない。E2E_OPENFGA_STORE がサーバーの store の名前と` +
          `同じか確かめる（verify のスタックは shiki-verify・いま ${process.env.E2E_OPENFGA_STORE ?? "shiki（既定）"}）`,
      ).toBe(true);
    }
  } finally {
    await deleteTableApi(page, t.id);
  }
}

async function buildTables(w: World, defs: TableDef[]): Promise<void> {
  const page = w.people.alice.page;
  for (const t of defs) {
    checkAbort(w);
    const name = `${t.name}-${w.uniq}`;
    const made = await ok<{ id: string; schema: Json; schema_version: number }>(page, "POST", "/data/tables", {
      name,
      schema: w.fill(t.schema),
    });
    w.tables[t.ref] = { id: made.id, name, schema: made.schema, schemaVersion: made.schema_version, rows: [] };
    w.values[`{{table.${t.ref}}}`] = made.id;
  }
  for (const t of defs) {
    const table = w.tables[t.ref];
    if (t.fsm) await attachFsm(w, table, t);
    for (const row of t.rows ?? []) {
      checkAbort(w);
      const rec = await ok<Record<string, unknown>>(page, "POST", `/data/tables/${table.id}/records`, { data: w.fill(row) }, [201]);
      table.rows.push(rec);
    }
    for (const s of t.shares ?? []) {
      await ok(page, "PUT", `/data/tables/${table.id}/shares`, { target: { type: "user", id: w.people[s.user].id }, role: s.role }, [204]);
    }
  }
}

async function attachFsm(w: World, table: BuiltTable, t: TableDef): Promise<void> {
  const page = w.people.alice.page;
  const fsm = t.fsm!;
  const body = { states: fsm.states, transitions: w.fill(fsm.transitions) };
  if ((fsm.via ?? "table") === "table") {
    const saved = await ok<{ schema: Json & { fsm_ref?: { artifact_id: string } }; schema_version: number }>(
      page,
      "PUT",
      `/data/tables/${table.id}/fsm`,
      {
        status_field: fsm.status_field,
        body,
        applicant_write_lock: fsm.applicant_write_lock ?? null,
        expected_schema_version: table.schemaVersion,
      },
    );
    table.schema = saved.schema;
    table.schemaVersion = saved.schema_version;
    table.fsmId = saved.schema.fsm_ref?.artifact_id;
  } else {
    const f = await ok<{ id: string; version?: number }>(page, "POST", "/data/fsms", {
      name: `${t.name}の状態-${w.uniq}`,
      table_id: table.id,
      body,
    });
    const saved = await ok<{ schema: Json; schema_version: number }>(page, "PUT", `/data/tables/${table.id}/schema`, {
      schema: { ...table.schema, fsm_ref: { artifact_id: f.id, version: f.version ?? 1 } },
      expected_schema_version: table.schemaVersion,
    });
    table.schema = saved.schema;
    table.schemaVersion = saved.schema_version;
    table.fsmId = f.id;
  }
  if (table.fsmId) w._undo.fsms.push(table.fsmId);
}

async function buildApp(w: World, a: AppDef): Promise<void> {
  const page = w.people.alice.page;
  const screen = w.fill(a.screen);
  const workflows = a.workflows.map((x) => {
    const wf = w.workflows[x.wf];
    if (!wf) throw new Error(`アプリ ${a.ref} のワークフロー ${x.wf} が無い`);
    return { alias: x.alias, artifact_id: wf.id, version: wf.version };
  });
  const views = (a.views ?? []).map((x) => {
    const v = w.views[x.view];
    if (!v) throw new Error(`アプリ ${a.ref} のビュー ${x.view} が無い`);
    return { alias: x.alias, artifact_id: v.id, version: v.version };
  });
  const name = `${a.name}-${w.uniq}`;
  const made = await ok<{ id: string; version: number; warnings: unknown[]; input_warnings: unknown[] }>(
    page,
    "POST",
    "/mini-apps",
    { name, body: { description: a.description ?? a.name, screen, workflows, data_views: views } },
  );
  w.apps[a.ref] = { id: made.id, name, version: made.version, screen, actions: screenActions(screen) };
  w.values[`{{app.${a.ref}}}`] = made.id;
  // 画面の書き損じ・ワークフローの入力との食い違いは、定義の誤りとして止める（公開も止まる）。
  expect(made.warnings, `アプリ ${a.ref} の画面の書き損じ`).toEqual([]);
  expect(made.input_warnings, `アプリ ${a.ref} の画面とワークフローの入力の食い違い`).toEqual([]);
  if (a.publish ?? true) await ok(page, "POST", `/mini-apps/${made.id}/publish`, { version: made.version });
  for (const s of a.shares ?? []) {
    await ok(page, "PUT", `/artifacts/${made.id}/shares`, { target: { type: "user", id: w.people[s.user].id }, role: s.role }, [204]);
  }
}

/// 世界を片付ける（どれかが落ちても残りを走らせ、最初の誤りを最後に投げる）。
export async function cleanupWorld(w: World): Promise<void> {
  const page = w.people.alice.page;
  const undo = w._undo;
  const wfs = Object.values(w.workflows);
  await cleanupAll(
    ...undo.deferred.reverse().map((s) => s),
    ...wfs.map((x) => () => cancelActiveRunsApi(page, x.id)),
    ...Object.values(w.apps).map((x) => () => deleteArtifactApi(page, x.id)),
    ...wfs.map((x) => () => removeWorkflow(page, x.id)),
    ...undo.models.reverse(),
    ...Object.values(w.views).map((x) => () => deleteArtifactApi(page, x.id)),
    ...undo.fsms.map((id) => () => deleteArtifactApi(page, id)),
    ...Object.values(w.tables).map((x) => () => deleteTableApi(page, x.id)),
    ...Object.values(w.files).map((x) => () => trashFileApi(page, x.id)),
    ...Object.values(w.folders)
      .reverse()
      .map((x) => () => trashFolderApi(page, x.id)),
    ...undo.tuples.reverse(),
  );
}
