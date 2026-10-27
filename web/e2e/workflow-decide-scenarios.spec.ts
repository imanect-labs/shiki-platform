import { expect, test, type Page } from "@playwright/test";

import {
  addRecord,
  cleanup,
  created,
  csrfHeaders,
  getRecord,
  instantiate,
  loadScenario,
  runsByRecord,
  shootViews,
  stepOutput,
  waitForStatus,
  type Created,
} from "./decide-scenarios";
import { loginViaKeycloak, stubDecisionModel } from "./helpers";

/// 判定の受け入れシナリオ（FEAT-1 の T8）を、テンプレート（`eval/decision/scenarios/*.json`）のまま組んで流す。
///
/// - APP-02 問い合わせの仕分け: 行が追加されると判定し、確かなものは振り分けの規則から担当を引いて「仕分け済み」、
///   確からしさがしきい値に届かないもの・判定に失敗したものは AI の推定を書いて「要確認」になる。
/// - APP-01 経費の確認: 規程（script）のフラグか AI の「経理の確認が要る」のどちらか、または確からしさ不足・
///   判定の失敗なら「経理確認」にして経理の承認を待ち、どれでもなければ上長の承認を待つ。経理が承認すると
///   上長の承認へ進む。
///
/// 判定は stub の決定器（材料に `<キー>:yes` / 選択肢のキーがあれば確からしさ 0.9・無ければ 0.4〜0.6・
/// `stub:fail` で失敗）で、実 LLM は呼ばない。判定用のモデルを組織で公開して、finally で元に戻す。
/// 作った表・状態の遷移の規則・ワークフローは finally で消す（承認を待つ run は止める）。
/// 前提: `~/.config/shiki-dev/llm-stub.toml` の decide-stub などを持つカタログ・SHIKI__WORKFLOW__ENABLED=true・
/// ログインする人（alice）が部署に 1 つ以上属する（経理・担当の部署の代わりに使う）。
/// `E2E_SHOTS_DIR` を渡すと、PC とスマホの画面を撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;

type AdminModel = { id: string; enabled: boolean; modality: string; provider_id: string; decision_via?: string | null };

async function adminModels(page: Page): Promise<AdminModel[]> {
  const res = await page.request.get("/api/org-admin/ai/models");
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { models: AdminModel[] }).models;
}

async function setEnabled(page: Page, id: string, enabled: boolean) {
  const res = await page.request.put("/api/org-admin/ai/settings", {
    headers: await csrfHeaders(page),
    data: { models: [{ id, enabled }] },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
}

type Me = { id: string; roles: string[] };

async function me(page: Page): Promise<Me> {
  return (await (await page.request.get("/api/me")).json()) as Me;
}

type Answer = { value: unknown; confidence: number; level?: string };
type DecideOutput = { answers: Record<string, Answer>; routed: { question: string; port: string } | null };

type Approval = { approval_id: string; title: string; from_state: string; record: { id: string; rev: number } };

async function inbox(page: Page): Promise<Approval[]> {
  const res = await page.request.get("/api/approvals");
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { items: Approval[] }).items;
}

/// 行の承認が受信箱に出るまで待つ（承認の停止は relay の tick で数秒かかる）。
async function approvalFor(page: Page, recordId: string, title: string): Promise<Approval> {
  let found: Approval | undefined;
  await expect
    .poll(
      async () => {
        found = (await inbox(page)).find((a) => a.record.id === recordId && a.title === title);
        return Boolean(found);
      },
      { timeout: 60_000, intervals: [1_000, 2_000] },
    )
    .toBe(true);
  return found!;
}

async function decide(page: Page, a: Approval, decision: "approve" | "reject", comment?: string) {
  const res = await page.request.post(`/api/approvals/${a.approval_id}/decision`, {
    headers: await csrfHeaders(page),
    data: { decision, comment, expected_rev: a.record.rev },
  });
  expect(res.status(), await res.text()).toBe(202);
}

test.describe("判定の受け入れシナリオ（stub の決定器）", () => {
  let decision: AdminModel | undefined;
  let roles: string[] = [];

  test.beforeEach(async ({ page }) => {
    await loginViaKeycloak(page);
    decision = await stubDecisionModel(page, await adminModels(page));
    test.skip(!decision, "判定用のモデル（stub の決定器）がカタログに無い構成");
    roles = (await me(page)).roles;
    test.skip(roles.length === 0, "ログインした人が部署に属していない（担当・経理の部署の代わりが無い）");
  });

  test("APP-02: 確かな判定は担当へ振り分け、確かでない・失敗した判定は要確認にする", async ({ page, browser }) => {
    test.setTimeout(300_000);
    const d = decision!;
    const s = loadScenario("inquiry_triage");
    const out: Created = created();
    try {
      if (!d.enabled) await setEnabled(page, d.id, true);
      const team = (i: number) => roles[i % roles.length];
      await instantiate(
        page,
        s,
        {
          "{{model.decide}}": d.id,
          "{{role.billing}}": team(0),
          "{{role.support}}": team(1),
          "{{role.sales}}": team(2),
        },
        out,
      );
      const table = out.tables.inquiries.id;
      const sure = await addRecord(page, table, {
        subject: "ログインできない",
        body: "bug の報告です。今朝からログインの画面が開きません。",
        sender: "つきみ運輸",
      });
      const unsure = await addRecord(page, table, { subject: "ご相談", body: "よく分からない内容です。", sender: "くるみ精機" });
      const failed = await addRecord(page, table, { subject: "判定の失敗", body: "stub:fail", sender: "あおい電設" });

      // 確かな判定（確からしさ 0.9 ≥ 0.6）: 振り分けの規則から種類と担当を書き、仕分け済み。
      const a = await waitForStatus(page, table, sure.id, ["仕分け済み", "要確認"]);
      expect(a.data).toMatchObject({ status: "仕分け済み", category: "不具合", assignee_team: team(1), angry: "いいえ" });
      expect(a.data.ai_guess).toBeUndefined();
      // 確かでない判定（先頭の請求を 0.4）: 推定を書いて要確認。種類と担当は書かない。
      const b = await waitForStatus(page, table, unsure.id, ["仕分け済み", "要確認"]);
      expect(b.data.status).toBe("要確認");
      expect(String(b.data.ai_guess)).toContain("種類は billing と推定（確からしさ 0.4");
      expect(b.data.category).toBeUndefined();
      expect(b.data.assignee_team).toBeUndefined();
      // 判定の失敗（on_error: continue の error の出口）: 失敗のコードを書いて要確認。
      const c = await waitForStatus(page, table, failed.id, ["仕分け済み", "要確認"]);
      expect(c.data.status).toBe("要確認");
      expect(String(c.data.ai_guess)).toMatch(/^AI が判定できませんでした（[a-z_]+）$/);

      // 実行履歴: 判定の step がどの出口へ進み、ほかの出口の先は skip された。run はどれも成功で終わる。
      const runs = await runsByRecord(page, out.workflowId!);
      const ports = (rid: string) => runs.get(rid)?.steps.find((st) => st.node_id === "triage")?.taken_ports;
      const status = (rid: string, node: string) => runs.get(rid)?.steps.find((st) => st.node_id === node)?.status;
      expect(ports(sure.id)).toEqual(["out"]);
      expect(status(sure.id, "guess")).toBe("skipped");
      expect(ports(unsure.id)).toEqual(["uncertain"]);
      expect(status(unsure.id, "assign")).toBe("skipped");
      expect(ports(failed.id)).toEqual(["error"]);
      for (const rid of [sure.id, unsure.id, failed.id]) expect(runs.get(rid)?.status, rid).toBe("succeeded");
      const triage = await stepOutput<DecideOutput>(page, out.workflowId!, runs.get(sure.id)!.run_id, "triage");
      expect(triage.answers.category).toMatchObject({ value: "bug" });
      expect(triage.routed).toEqual({ question: "category", port: "out" });

      if (SHOTS) {
        const ctx = await browser.newContext({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });
        try {
          const phone = await ctx.newPage();
          await loginViaKeycloak(phone);
          await shootViews(page, phone, SHOTS, "app02-stub", {
            workflowId: out.workflowId!,
            decideNode: "triage",
            runId: runs.get(unsure.id)!.run_id,
            tableId: table,
          });
        } finally {
          await ctx.close();
        }
      }
    } finally {
      await cleanup(page, out);
      if (!d.enabled) await setEnabled(page, d.id, false);
    }
  });

  test("APP-01: 規程・AI・確からしさ不足・判定の失敗のどれかで経理確認へ回し、経理の承認の後は上長の承認へ進む", async ({
    page,
    browser,
  }) => {
    test.setTimeout(360_000);
    const d = decision!;
    const s = loadScenario("expense_policy");
    const out: Created = created();
    try {
      if (!d.enabled) await setEnabled(page, d.id, true);
      const self = await me(page);
      await instantiate(page, s, { "{{model.decide}}": d.id, "{{role.accounting}}": roles[0] }, out);
      const table = out.tables.expense_claims.id;
      const claim = (over: Record<string, unknown>) => ({
        title: "交通費 本社〜客先 往復",
        applicant: "山田 太郎",
        department: "営業部",
        used_on: "2026-10-07",
        category: "交通費",
        amount: 1040,
        payee: "鉄道（IC カード）",
        item: "本社〜ことり堂 往復",
        purpose: "ことり堂 様への訪問 needs_review:no",
        receipt: "なし",
        approver: self.id,
        ...over,
      });
      const after = ["経理確認", "上長承認待ち"];
      // 規程にも AI にも引っかからない（stub は needs_review:no を確からしさ 0.9 で「いいえ」）→ 上長の承認。
      const ok = await addRecord(page, table, claim({}));
      const okRow = await waitForStatus(page, table, ok.id, after);
      expect(okRow.data).toMatchObject({ status: "上長承認待ち", ai_check: "問題なし", rule_flags: [] });
      // 同じ申請者・利用日・金額の申請（重複の疑い）は、AI が「いいえ」でも経理確認（ok の自動の分が済んでから足す）。
      const dup = await addRecord(page, table, claim({ purpose: "ことり堂 様への訪問（再提出） needs_review:no" }));
      // 規程: 会議費の 1 人あたり 6,500 円（上限 5,000 円）。
      const rule = await addRecord(
        page,
        table,
        claim({
          title: "会議費 昼食",
          category: "会議費",
          amount: 26000,
          people: 4,
          attendees: "社内 4 名",
          payee: "レストラン",
          item: "昼食",
          purpose: "新製品の企画の会議 needs_review:no",
          receipt: "あり",
        }),
      );
      // AI: 経理の確認が要る（確からしさ 0.9）。
      const ai = await addRecord(page, table, claim({ amount: 1960, purpose: "客先への訪問 needs_review:yes" }));
      // 確からしさ不足（手がかりなし → 「いいえ」を 0.6・しきい値 0.7）。
      const unsure = await addRecord(page, table, claim({ amount: 2860, purpose: "業界の展示会の視察" }));
      // 判定の失敗（error の出口）。
      const failed = await addRecord(page, table, claim({ amount: 3300, purpose: "stub:fail" }));

      const rows = {
        dup: await waitForStatus(page, table, dup.id, after),
        rule: await waitForStatus(page, table, rule.id, after),
        ai: await waitForStatus(page, table, ai.id, after),
        unsure: await waitForStatus(page, table, unsure.id, after),
        failed: await waitForStatus(page, table, failed.id, after),
      };
      expect(rows.dup.data).toMatchObject({
        status: "経理確認",
        ai_check: "問題なし",
        rule_flags: ["同じ申請者・利用日・金額の申請が既にある（重複の疑い）"],
      });
      expect(rows.rule.data).toMatchObject({
        status: "経理確認",
        ai_check: "問題なし",
        rule_flags: ["会議費の 1 人あたりの上限（5,000 円）を超える"],
      });
      expect(rows.ai.data).toMatchObject({ status: "経理確認", ai_check: "要確認", rule_flags: [] });
      expect(rows.unsure.data).toMatchObject({ status: "経理確認", ai_check: "確からしさ不足" });
      expect(String(rows.unsure.data.ai_note)).toContain("確からしさ 0.6");
      expect(rows.failed.data).toMatchObject({ status: "経理確認", ai_check: "判定の失敗" });

      // 判定の出口: 規程と AI の判定は out から分岐へ、確からしさ不足は uncertain、失敗は error。
      const runs = await runsByRecord(page, out.workflowId!);
      const ports = (rid: string, node: string) => runs.get(rid)?.steps.find((st) => st.node_id === node)?.taken_ports;
      expect(ports(rule.id, "judge")).toEqual(["out"]);
      expect(ports(rule.id, "need")).toEqual(["true"]);
      expect(ports(ok.id, "need")).toEqual(["false"]);
      expect(ports(unsure.id, "judge")).toEqual(["uncertain"]);
      expect(ports(failed.id, "judge")).toEqual(["error"]);
      // 判定の材料は評価データと同じ書き方（利用日に曜日・規程のフラグは材料に入れない）。
      const rules = await stepOutput<{ state: Record<string, unknown> }>(page, out.workflowId!, runs.get(rule.id)!.run_id, "rules");
      expect(rules.state).toMatchObject({ 利用日: "2026-10-07（水）", 費目: "会議費", 金額: 26000, 人数: 4 });

      // 受信箱: 経理確認は経理の部署へ、それ以外は上長（承認者の列）へ。
      const accountingTitle = "経費の申請の経理確認";
      const managerTitle = "経費の申請の承認";
      const forOk = await approvalFor(page, ok.id, managerTitle);
      expect(forOk.from_state).toBe("上長承認待ち");
      const forRule = await approvalFor(page, rule.id, accountingTitle);
      expect(forRule.from_state).toBe("経理確認");
      for (const r of [dup, ai, unsure, failed]) await approvalFor(page, r.id, accountingTitle);

      if (SHOTS) {
        const ctx = await browser.newContext({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });
        try {
          const phone = await ctx.newPage();
          await loginViaKeycloak(phone);
          await shootViews(page, phone, SHOTS, "app01-stub", {
            workflowId: out.workflowId!,
            decideNode: "judge",
            runId: runs.get(rule.id)!.run_id,
            inbox: true,
            tableId: table,
          });
        } finally {
          await ctx.close();
        }
      }

      // 経理が承認すると、行は上長承認待ちへ進み、上長の承認が受信箱に出る（承認の後の合流）。
      await decide(page, forRule, "approve");
      await waitForStatus(page, table, rule.id, ["上長承認待ち"], 60_000);
      const next = await approvalFor(page, rule.id, managerTitle);
      expect(next.from_state).toBe("上長承認待ち");
      // 上長が承認すると、行は承認になり run は成功で終わる。
      await decide(page, forOk, "approve");
      expect((await waitForStatus(page, table, ok.id, ["承認"], 60_000)).data.status).toBe("承認");
      await expect
        .poll(async () => (await runsByRecord(page, out.workflowId!)).get(ok.id)?.status, { timeout: 60_000 })
        .toBe("succeeded");
      expect((await getRecord(page, table, failed.id)).data.status).toBe("経理確認");
    } finally {
      await cleanup(page, out);
      if (!d.enabled) await setEnabled(page, d.id, false);
    }
  });
});
