import { randomUUID } from "node:crypto";

import { expect, type Page } from "@playwright/test";

import { call } from "./http";
import type { World } from "./world";

/// 権限の否定の確かめ（台帳 §0.1 (3) の 4 点目）。
/// - 別の組織の人（charlie）は、世界のどれも読めず・押せない。答えは存在しない id のときと同じ（存在を区別しない）。
///   表・行・フォルダーは 404、アーティファクト（ワークフロー・ビュー・アプリ）は権限を先に問うので 403 で返る
///   （存在しない id でも 403・どちらも存在の手がかりにはならない）。
/// - 使う人（bob）は、他人の行と run を、ミニアプリの入口から読めない。
/// - ワークフローの側から他人の interactive run を読めるのは既知の #917（D-31）なので、他人の run の否定はミニアプリの
///   入口（`/mini-apps/{id}/runs/{run}`）だけで行う（計画のレビュー 4）。

export type Probe = { method?: "GET" | "POST"; path: string; data?: unknown; what?: string };

/// どの道も `statuses`（既定 404）で返ることを確かめる（まとめて落とし、どの道かを言う）。
export async function expectStatus(page: Page, probes: Probe[], statuses: number[] = [404]): Promise<void> {
  const bad: string[] = [];
  for (const p of probes) {
    const r = await call(page, p.method ?? "GET", p.path, p.data);
    if (!statuses.includes(r.status)) bad.push(`${p.method ?? "GET"} ${p.path}${p.what ? `（${p.what}）` : ""} → ${r.status} ${r.text.slice(0, 200)}`);
  }
  expect(bad, `期待した状態コード ${statuses.join("/")} でない道`).toEqual([]);
}

const UUID = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/gi;

/// 道の最後の id（いちばん細かい対象）を、存在しない id に替えた道。
function twinOf(path: string): string {
  const ids = path.match(UUID) ?? [];
  if (ids.length === 0) return path;
  const last = ids[ids.length - 1];
  const at = path.lastIndexOf(last);
  return path.slice(0, at) + randomUUID() + path.slice(at + last.length);
}

/// どの道も読めず・押せず（403 か 404）、存在しない id に替えた道と同じ状態コードで返る（存在を区別しない）。
export async function expectDenied(page: Page, probes: Probe[]): Promise<void> {
  const bad: string[] = [];
  for (const p of probes) {
    const method = p.method ?? "GET";
    const r = await call(page, method, p.path, p.data);
    const twin = await call(page, method, twinOf(p.path), p.data);
    const where = `${method} ${p.path}${p.what ? `（${p.what}）` : ""}`;
    if (![403, 404].includes(r.status)) bad.push(`${where} → ${r.status} ${r.text.slice(0, 200)}`);
    else if (r.status !== twin.status) bad.push(`${where} → ${r.status}（存在しない id では ${twin.status}・存在が分かる）`);
  }
  expect(bad, "読めてしまう・存在が分かる道").toEqual([]);
}

/// 世界の作ったもの全部への道（表・ビュー・ワークフローと run・アプリと押す・表を読む・フォルダー・ファイル）。
/// `runs` は {ワークフローの ref: run の id} と {アプリの ref: run の id}（あれば詳細の道も足す）。
export function worldProbes(w: World, extra: { workflowRuns?: [string, string][]; appRuns?: [string, string][] } = {}): Probe[] {
  const out: Probe[] = [];
  for (const [ref, t] of Object.entries(w.tables)) {
    out.push({ path: `/data/tables/${t.id}`, what: `表 ${ref}` });
    out.push({ method: "POST", path: `/data/tables/${t.id}/query`, data: { limit: 1 }, what: `表 ${ref} の行` });
    for (const r of t.rows.slice(0, 1)) out.push({ path: `/data/tables/${t.id}/records/${String(r.id)}`, what: `表 ${ref} の行 1 件` });
  }
  for (const [ref, v] of Object.entries(w.views)) out.push({ method: "POST", path: `/data/views/${v.id}/run`, data: {}, what: `ビュー ${ref}` });
  for (const [ref, wf] of Object.entries(w.workflows)) {
    out.push({ path: `/workflows/${wf.id}`, what: `ワークフロー ${ref}` });
    out.push({ path: `/workflows/${wf.id}/runs`, what: `ワークフロー ${ref} の run` });
  }
  for (const [ref, run] of extra.workflowRuns ?? []) {
    out.push({ path: `/workflows/${w.workflows[ref].id}/runs/${run}`, what: `run ${ref}` });
  }
  for (const [ref, a] of Object.entries(w.apps)) {
    out.push({ path: `/mini-apps/${a.id}/resolved`, what: `アプリ ${ref}` });
    out.push({ path: `/mini-apps/${a.id}/runs`, what: `アプリ ${ref} の記録` });
    const [label, action] = Object.entries(a.actions)[0] ?? [];
    if (action) {
      out.push({
        method: "POST",
        path: `/mini-apps/${a.id}/ui-actions`,
        data: { version: a.version, action_id: action, params: {}, idempotency_key: `probe-${Date.now().toString(36)}` },
        what: `アプリ ${ref} の「${label}」を押す`,
      });
    }
  }
  for (const [ref, run] of extra.appRuns ?? []) out.push({ path: `/mini-apps/${w.apps[ref].id}/runs/${run}`, what: `アプリ ${ref} の run` });
  for (const [ref, f] of Object.entries(w.folders)) out.push({ path: `/nodes?parent_id=${f.id}`, what: `フォルダー ${ref}` });
  for (const [ref, f] of Object.entries(w.files)) out.push({ path: `/files/${f.id}/text`, what: `ファイル ${ref}` });
  return out;
}

/// 別の組織の人には、世界のどれも読めず・押せず、存在しない id のときと同じ答え（[`expectDenied`]）。
/// アプリの画面の表（data_grid）の道も確かめる。
export async function expectOutsiderSeesNothing(
  outsider: Page,
  w: World,
  extra: { workflowRuns?: [string, string][]; appRuns?: [string, string][] } = {},
): Promise<void> {
  const probes = worldProbes(w, extra);
  for (const [ref, a] of Object.entries(w.apps)) {
    for (const m of a.screen.matchAll(/```data_grid\n([^\n]+)\n```/g)) {
      probes.push({
        method: "POST",
        path: `/mini-apps/${a.id}/ui-actions`,
        data: { version: a.version, action_id: `data_grid:${m[1].trim()}`, params: {} },
        what: `アプリ ${ref} の表 ${m[1].trim()}`,
      });
    }
  }
  await expectDenied(outsider, probes);
}

/// 行がどれも `field` = `userId` であること（行述語で本人の分に絞れている）。
export function expectOnlyOwn(rows: { data: Record<string, unknown> }[], field: string, userId: string, what = "行"): void {
  const others = rows.filter((r) => r.data[field] !== userId);
  expect(others.map((r) => r.data), `${what}は本人（${field}=${userId}）の分だけ`).toEqual([]);
}
