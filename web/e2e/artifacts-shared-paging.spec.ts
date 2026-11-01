import { expect, test, type Page, type Route } from "@playwright/test";

import { cleanupQueue, deleteArtifactApi, loginAs, loginViaKeycloak, uniqueName } from "./helpers";

/// アーティファクトの一覧（GET /artifacts）の共有された分が、続きの位置をたどって読める（BUG-162）:
/// - 共有された分が `limit` を超えると `shared_next_cursor` が付き、`shared_cursor` に渡すと共有された分の続きだけが
///   返る（重ならず新しい順）。位置は不透明（id を含まない）で、読めない位置は 400（cursor_invalid）。
/// - 画面（スキルの一覧）は、共有された分が 0 件のまま続きの位置だけが付いて返っても、続きをたどって出す（応答は
///   route で差し替える）。
/// 作ったもの（アーティファクト・ブラウザーの文脈）は最後に片付ける（失敗したテストでも走る）。

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrf(page: Page): Promise<Record<string, string>> {
  const token = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": token, "content-type": "application/json" };
}

type ListBody = { items: { id: string; owner: string }[]; shared_next_cursor?: string | null };

test("共有された分が多いと続きの位置が付き、shared_cursor で共有された分の続きだけを読める（BUG-162）", async ({
  page,
  browser,
}) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const alice = (await (await page.request.get("/api/me")).json()) as { id: string };
  const bobCtx = await browser.newContext();
  cleanup.add(() => bobCtx.close());
  const bob = await bobCtx.newPage();
  await loginAs(bob, "bob");
  // bob が script を 3 つ作り、alice に閲覧で共有する（後から作ったものほど新しい）。
  const shared: string[] = [];
  for (let i = 0; i < 3; i += 1) {
    const res = await bob.request.post("/api/artifacts", {
      headers: await csrf(bob),
      data: { kind: "script", name: uniqueName(`e2e-共有の続き-${i}`), body: { source: "export default 1;" } },
    });
    expect(res.status(), await res.text()).toBe(201);
    const id = ((await res.json()) as { id: string }).id;
    cleanup.add(() => deleteArtifactApi(bob, id));
    const s = await bob.request.put(`/api/artifacts/${id}/shares`, {
      headers: await csrf(bob),
      data: { target: { type: "user", id: alice.id }, role: "viewer" },
    });
    expect(s.status(), await s.text()).toBe(204);
    shared.push(id);
  }

  const first = (await (await page.request.get("/api/artifacts?kind=script&limit=1")).json()) as ListBody;
  const cursor0 = first.shared_next_cursor;
  expect(cursor0, "共有された分に続きがある").toBeTruthy();
  for (const id of shared) expect(cursor0).not.toContain(id);
  const seen = first.items.filter((a) => a.owner !== alice.id).map((a) => a.id);
  let cursor = cursor0 ?? null;
  for (let round = 0; cursor && round < 10; round += 1) {
    const p = (await (
      await page.request.get(`/api/artifacts?kind=script&limit=1&shared_cursor=${encodeURIComponent(cursor)}`)
    ).json()) as ListBody;
    // 続きは共有された分だけ（自分のものは返さない）。
    expect(p.items.every((a) => a.owner !== alice.id)).toBe(true);
    seen.push(...p.items.map((a) => a.id));
    cursor = p.shared_next_cursor ?? null;
  }
  expect(seen.filter((id) => shared.includes(id))).toEqual([...shared].reverse());
  expect(new Set(seen).size).toBe(seen.length);

  // 読めない位置は 400（cursor_invalid）。所有分の続き（before_*）と同時には使えない。
  const bad = await page.request.get(`/api/artifacts?kind=script&shared_cursor=${encodeURIComponent(`${cursor0}x`)}`);
  expect(bad.status()).toBe(400);
  expect(((await bad.json()) as { code?: string }).code).toBe("cursor_invalid");
  const both = await page.request.get(
    `/api/artifacts?kind=script&shared_cursor=${encodeURIComponent(cursor0 ?? "")}` +
      `&before_updated_at=${encodeURIComponent(new Date().toISOString())}&before_id=${shared[0]}`,
  );
  expect(both.status()).toBe(400);
});

const LIST = /\/api\/artifacts\?kind=skill(&.*)?$/;

test("スキルの一覧: 共有された分が 0 件のまま続きの位置が付いても、続きをたどって出す（BUG-162）", async ({ page }) => {
  test.setTimeout(90_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const asked: string[] = [];
  const skill = {
    id: "00000000-0000-4000-8000-000000000162",
    kind: "skill",
    name: "続きで見つかったスキル",
    owner: "someone-else",
    current_version: 1,
    created_at: "2026-10-30T00:00:00Z",
    updated_at: "2026-10-30T00:00:00Z",
  };
  const handler = async (route: Route) => {
    if (route.request().method() !== "GET") return route.fallback();
    const cursor = new URL(route.request().url()).searchParams.get("shared_cursor") ?? "";
    asked.push(cursor);
    const body =
      cursor === "" ? { items: [], shared_next_cursor: "s1" } : cursor === "s1" ? { items: [], shared_next_cursor: "s2" } : { items: [skill], shared_next_cursor: null };
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) });
  };
  await page.route(LIST, handler);
  try {
    await page.goto("/skills");
    await expect(page.getByText("続きで見つかったスキル")).toBeVisible({ timeout: 15_000 });
    expect(asked).toEqual(["", "s1", "s2"]);
  } finally {
    await page.unroute(LIST, handler);
  }
});
