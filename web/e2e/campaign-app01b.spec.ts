import { expect, test } from "@playwright/test";

import { addClaim, app01bWorld, claim, grantItems, POST_GRANTS, TITLE, waitRunForRecord } from "./campaign/apps/app01";
import { ok } from "./campaign/http";
import { people, type People } from "./campaign/people";
import { withMinuteCron } from "./campaign/time";
import { approvals, cmp, expectSingleRow, rows, runs, type Approval, type RunDetail } from "./campaign/truth";
import { buildWorld, cleanupWorld, enableWithPlan, type World } from "./campaign/world";

/// APP-01 経費精算の 01b（B の計上・台帳 §7.13 の APP-01）。世界は campaign/apps/app01.ts。
///
/// - §7 の (3) の B の経路: 計上の D（承認への遷移のきっかけ）を止めたまま承認した申請を、拾い直しの B を毎分にした
///   写し（本物のスケジュール・経理の alice の委譲・trigger_kind=schedule）が拾い、台帳に 1 行書いて計上済みにする。
///   元の `*/15 * * * *` の次の発火が 15 分以内であることも、エディタと同じ見通しの API で見る。
/// - (4) 再実行・同時実行・二重の判断でも台帳に重複が出ない: D を有効にしてから、同じ承認への判断を 2 本同時に送り、
///   B の手動の実行を 2 本同時に起こし（D と競う）、D の run を新しい実行でやり直す。台帳の申請ごとの行数はいつも 1 で、
///   負けた側の遷移は B の map の errors か D の error の出口に残る。判断は片方だけが通る。
/// 長い待ちは毎分の cron の 1 つ（最大 150 秒）。申請は bob が表に直接足す（追加だけ・アプリは 01a で確かめる）。

test.describe.configure({ mode: "serial" });

let P: People;
let W: World;

test.beforeAll(async ({ browser }) => {
  test.setTimeout(300_000);
  P = await people(browser);
  W = await buildWorld(P, app01bWorld());
});

test.afterAll(async () => {
  test.setTimeout(240_000);
  try {
    if (W) await cleanupWorld(W);
  } finally {
    await P?.close();
  }
});

const alice = () => P.alice.page;
const bob = () => P.bob.page;
const claims = () => W.tables.claims.id;
const ledger = () => W.tables.ledger.id;

/// bob が申請を足し、C が承認待ちにしたら、上長（alice）が受信箱で承認する。承認の判断までして承認を返す。
async function submitted(title: string, amount: number): Promise<{ id: string; approval: Approval }> {
  const made = await addClaim(bob(), claims(), claim({ title, amount, used_on: "2026-10-30" }));
  await rows.waitForStatus(alice(), claims(), made.id, ["承認待ち"], { timeout: 60_000 });
  return { id: made.id, approval: await approvals.approvalFor(alice(), made.id, TITLE.mgr) };
}

/// B の run の map で、申請ごとに計上済みにできた（items）・できなかった（errors の code）を数える。
async function batchOutcome(runId: string, wfId: string): Promise<{ won: string[]; lost: [string, string][] }> {
  const list = await runs.stepOutput<{ items: { id: string }[] }>(alice(), wfId, runId, "list");
  const map = await runs.stepOutput<{ items: ({ id?: string } | null)[]; errors?: { index: number; error: { code: string } }[] }>(
    alice(),
    wfId,
    runId,
    "each",
  );
  const failed = new Set((map.errors ?? []).map((e) => e.index));
  return {
    won: list.items.filter((_, i) => !failed.has(i)).map((x) => x.id),
    lost: (map.errors ?? []).map((e) => [list.items[e.index].id, e.error.code]),
  };
}

test("(3) D を止めたまま承認した申請を、B の毎分の写し（本物のスケジュール）が拾って台帳に 1 行・計上済みにする", async () => {
  test.setTimeout(420_000);
  // 元のスケジュール（15 分ごと）の次の発火は 15 分以内（エディタの「次の実行」と同じ読み方）。
  const preview = await ok<{ next: string[] }>(alice(), "POST", "/workflows/schedule-preview", {
    cron: "*/15 * * * *",
    tz: "Asia/Tokyo",
    count: 2,
  });
  const [n1, n2] = preview.next.map((x) => Date.parse(x));
  expect(n1 - Date.now(), "次の発火は 15 分以内").toBeLessThanOrEqual(15 * 60_000);
  expect(n2 - n1).toBe(15 * 60_000);

  const a = await submitted("出張の電車代（拾い直し 1）", 2310);
  const b = await submitted("出張の電車代（拾い直し 2）", 2420);
  for (const x of [a, b]) await approvals.decide(alice(), x.approval, "approve");
  for (const x of [a, b]) await rows.waitForStatus(alice(), claims(), x.id, ["承認"], { timeout: 30_000 });
  // D は有効にしていないので、承認のまま残る。
  expect((await runs.list(alice(), W.workflows.post.id)).length).toBe(0);

  const batch = W.workflows.batch;
  await withMinuteCron(alice(), { ir: batch.ir, name: batch.name, grants: grantItems(W, POST_GRANTS) }, async ({ workflowId, run, detail }) => {
    expect(run.trigger_kind).toBe("schedule");
    expect(detail.status, JSON.stringify(detail.failure)).toBe("succeeded");
    const first = await batchOutcome(run.run_id, workflowId);
    expect(first.won.sort()).toEqual([a.id, b.id].sort());
    expect(first.lost).toEqual([]);
    const today = await runs.stepOutput<{ date: string }>(alice(), workflowId, run.run_id, "today");
    for (const [x, amount] of [
      [a, 2310],
      [b, 2420],
    ] as const) {
      expect((await rows.get(alice(), claims(), x.id)).data.status).toBe("計上済み");
      const line = await expectSingleRow(alice(), ledger(), cmp("claim_id", "eq", x.id), "台帳の行");
      expect(line.data).toMatchObject({ claim_id: x.id, amount, account: "旅費交通費", posted_on: today.date });
      // 自動実行（スケジュール・経理の委譲）が作った行の作成者は、いまは委譲元の alice（GAP-58・docs はワークフローと書く）。
      expect((line as { owner?: string }).owner).toBe(P.alice.id);
    }
  });
  // 止めた後も、台帳は申請ごとに 1 行のまま（2 回目の発火が入っても、承認の申請はもう無い）。
  for (const x of [a, b]) await expectSingleRow(alice(), ledger(), cmp("claim_id", "eq", x.id), "台帳の行");
});

test("(4) 再実行・同時実行・二重の判断でも、台帳は申請 1 件に 1 行で、負けた側は errors か error の出口に残る", async () => {
  test.setTimeout(420_000);
  const post = W.workflows.post;
  const batch = W.workflows.batch;
  // 後半は D（承認への遷移のきっかけ）を有効にする。
  await enableWithPlan(alice(), post.id, post.version, grantItems(W, POST_GRANTS));
  const list = [await submitted("学会の参加費（同時 1）", 3510), await submitted("学会の参加費（同時 2）", 3620), await submitted("学会の参加費（同時 3）", 3730)];
  const [c1, c2, c3] = list;

  // 同じ承認への判断を 2 本同時に送る: 片方だけが通り、もう片方は 409（決まった・判断中・rev の不一致）か 403。
  const twice = await Promise.all([
    approvals.decideStatus(alice(), c1.approval, "approve"),
    approvals.decideStatus(alice(), c1.approval, "approve"),
  ]);
  expect(twice.filter((s) => s === 202), `判断の答え ${twice.join(",")}`).toHaveLength(1);
  expect([409, 403], `負けた判断 ${twice.join(",")}`).toContain(twice.find((s) => s !== 202));
  for (const x of [c2, c3]) await approvals.decide(alice(), x.approval, "approve");
  for (const x of list) await rows.waitForStatus(alice(), claims(), x.id, ["承認", "計上済み"], { timeout: 30_000 });
  // D がきっかけで動くのと競うように、B を手動で 2 本同時に起こす（どちらも承認の申請を拾う）。
  const started = await Promise.all([0, 1].map(() => ok<{ run_id: string }>(alice(), "POST", `/workflows/${batch.id}/runs`, { input: {} }, [202])));
  const batchRuns: RunDetail[] = [];
  for (const s of started) batchRuns.push(await runs.waitEnd(alice(), batch.id, s.run_id, 120_000));
  for (const d of batchRuns) expect(d.status, JSON.stringify(d.failure)).toBe("succeeded");
  const postRuns: RunDetail[] = [];
  for (const x of list) {
    const d0 = await waitRunForRecord(alice(), post.id, x.id);
    postRuns.push(await runs.waitEnd(alice(), post.id, d0.run_id, 120_000));
  }

  // 不変条件: どの申請も計上済みで、台帳はちょうど 1 行。計上済みにできたのは、D と B を合わせて申請ごとに 1 回だけ。
  const won = new Map<string, number>();
  const lost: [string, string, string][] = [];
  for (const d of batchRuns) {
    const o = await batchOutcome(d.run_id, batch.id);
    for (const id of o.won) won.set(id, (won.get(id) ?? 0) + 1);
    for (const [id, code] of o.lost) lost.push(["B", id, code]);
  }
  for (const [i, d] of postRuns.entries()) {
    expect(d.status, JSON.stringify([d.failure, d.steps.map((x) => [x.node_id, x.status, x.attempt, x.taken_ports, x.error])])).toBe("succeeded");
    const ports = runs.takenPorts(d, "done");
    if (ports?.[0] === "out") won.set(list[i].id, (won.get(list[i].id) ?? 0) + 1);
    else {
      expect(ports).toEqual(["error"]);
      lost.push(["D", list[i].id, String(runs.errorCode(d, "done"))]);
    }
  }
  for (const x of list) {
    expect((await rows.get(alice(), claims(), x.id)).data.status).toBe("計上済み");
    await expectSingleRow(alice(), ledger(), cmp("claim_id", "eq", x.id), "台帳の行");
    expect(won.get(x.id), `申請 ${x.id} を計上済みにできた実行の数`).toBe(1);
  }
  // 負けた側（同じ申請を後から計上済みにしようとした実行）の遷移は、rev の不一致（409）か、計上済みからは進めない（422）。
  test.info().annotations.push({ type: "負けた側（実行・申請・code）", description: JSON.stringify(lost) });
  for (const [, , code] of lost) expect(["conflict", "invalid", "fsm_invalid"], `負けた側の code ${code}`).toContain(code);

  // D の run を新しい実行でやり直しても（同じきっかけの内容）、台帳は増えず、遷移は error の出口に残る。
  const before = await rows.count(alice(), ledger());
  const retry = await ok<{ run_id: string }>(alice(), "POST", `/workflows/${post.id}/runs/${postRuns[0].run_id}/retry`, { mode: "new" }, [202]);
  const again = await runs.waitEnd(alice(), post.id, retry.run_id, 120_000);
  expect(again.status, JSON.stringify(again.failure)).toBe("succeeded");
  expect(runs.takenPorts(again, "done")).toEqual(["error"]);
  expect(await rows.count(alice(), ledger())).toBe(before);
  await expectSingleRow(alice(), ledger(), cmp("claim_id", "eq", c1.id), "台帳の行");
  // B をもう一度動かしても、承認の申請はもう無いので何も書かない。
  const rerun = await ok<{ run_id: string }>(alice(), "POST", `/workflows/${batch.id}/runs`, { input: {} }, [202]);
  const r = await runs.waitEnd(alice(), batch.id, rerun.run_id, 120_000);
  expect(r.status).toBe("succeeded");
  expect((await batchOutcome(rerun.run_id, batch.id)).won).toEqual([]);
  expect(await rows.count(alice(), ledger())).toBe(before);
});
