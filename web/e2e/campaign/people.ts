import type { Browser, BrowserContext, Page } from "@playwright/test";

import { loginAs } from "../helpers";
import { ok } from "./http";

/// キャンペーンの登場人物（compose の seed ユーザー・deploy/keycloak/shiki-realm.json）。
/// - alice: a-corp の組織の管理者。表・ワークフロー・アプリを作る人（部署の役は spec ごとの FGA の tag で付ける）。
/// - bob: alice と同じ組織の一般の利用者（アプリを使う人）。
/// - charlie: 別の組織の人（どれにも 404 であることを確かめる）。
/// 3 人目の seed ユーザーは足さない（realm を変えると既存の spec に響く）。

export type PersonName = "alice" | "bob" | "charlie";

export type Person = {
  name: PersonName;
  ctx: BrowserContext;
  /// ログイン済みの画面（PC 幅 1440px）。API もこの画面のセッションで呼ぶ。
  page: Page;
  /// `/api/me` の id（メンバーの列・共有の相手・FGA の user に使う）。
  id: string;
  tenant: string;
  org: string;
  /// IdP が宣言した部署（ディレクトリに載る realm の部署・role_ref の列に使える）。
  roles: string[];
};

export type People = Record<PersonName, Person> & {
  /// 全員の画面を閉じる（afterAll の最後に呼ぶ）。
  close: () => Promise<void>;
};

type Me = { id: string; tenant_id: string; org: string; roles: string[] };

const PC = { width: 1440, height: 900 };

/// 1 人分の画面を作ってログインする（失敗したら作った画面を閉じてから落とす）。
export async function person(browser: Browser, name: PersonName): Promise<Person> {
  const ctx = await browser.newContext({ viewport: PC, ignoreHTTPSErrors: true, locale: "ja-JP", timezoneId: "Asia/Tokyo" });
  try {
    const page = await ctx.newPage();
    await loginAs(page, name);
    const me = await ok<Me>(page, "GET", "/me");
    return { name, ctx, page, id: me.id, tenant: me.tenant_id, org: me.org, roles: me.roles ?? [] };
  } catch (e) {
    await ctx.close().catch(() => undefined);
    throw e;
  }
}

/// alice・bob・charlie の画面を作る（beforeAll の `browser` から・page の fixture は beforeAll で使えない）。
/// bob は一度ログインしてディレクトリに載る（共有の相手・メンバーの列に入れられる）。
export async function people(browser: Browser, names: PersonName[] = ["alice", "bob", "charlie"]): Promise<People> {
  const made: Partial<Record<PersonName, Person>> = {};
  const close = async () => {
    for (const p of Object.values(made)) await p?.ctx.close().catch(() => undefined);
  };
  try {
    for (const n of names) made[n] = await person(browser, n);
  } catch (e) {
    await close();
    throw e;
  }
  return { ...(made as Record<PersonName, Person>), close };
}

/// 同じ人のスマホの画面（390px・セッションを持ち込む）。呼び出し側が閉じる（`page.context().close()`）。
export async function phoneOf(browser: Browser, p: Person): Promise<Page> {
  const ctx = await browser.newContext({
    storageState: await p.ctx.storageState(),
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
    deviceScaleFactor: 2,
    ignoreHTTPSErrors: true,
    locale: "ja-JP",
    timezoneId: "Asia/Tokyo",
  });
  return ctx.newPage();
}
