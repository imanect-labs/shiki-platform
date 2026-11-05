import { deflateSync } from "node:zlib";

import { test, expect, type Request } from "@playwright/test";

/// 回答の画像の出所チェックと代理取得（/reference/answer-images・#1005・#1044・PIT-77）。
/// 認証・バックエンド・LLM 不要（middleware は /reference を除外）＝ web 単体で回せる。
///
/// 守ること:
/// - 出典（ツール結果）に現れた画像だけを読み込み、LLM が組み立てた URL は読み込まない。
/// - 出典の画像も**ブラウザから外部ホストへは 1 度もリクエストを送らない**。shiki-server の代理取得
///   （`/api/threads/{id}/messages/{message_id}/image?u=`）だけを通し、`u` は出典に書かれていた字句。
/// - 代理取得が失敗した画像は、読み込まなかった画像と同じリンクに落とす。
/// DOM に出ないことではなく、通信が起きないことを見る。`SHOTS_DIR` を渡すとライト/ダークの PNG を保存する。
const SHOTS = process.env.SHOTS_DIR;

const PAGE_IMAGE = "https://images.example.test/market/saas-share-2026.png";
const CHART_IMAGE = "https://charts.example.test/render_(v2).png?series=41|27|32";

/// 単色の PNG（代理取得の応答。サーバはラスタ画像しか返さない）。
function solidPng(width: number, height: number): Buffer {
  const crcTable = Array.from({ length: 256 }, (_, n) => {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    return c >>> 0;
  });
  const crc = (buf: Buffer) => {
    let c = 0xffffffff;
    for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
    return (c ^ 0xffffffff) >>> 0;
  };
  const chunk = (type: string, data: Buffer) => {
    const len = Buffer.alloc(4);
    len.writeUInt32BE(data.length);
    const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
    const sum = Buffer.alloc(4);
    sum.writeUInt32BE(crc(body));
    return Buffer.concat([len, body, sum]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr.set([8, 2, 0, 0, 0], 8);
  const row = Buffer.concat([Buffer.from([0]), Buffer.alloc(width * 3, 0)]);
  for (let x = 0; x < width; x++) row.set([0x2f, 0x6b, 0xff], 1 + x * 3);
  const raw = Buffer.concat(Array.from({ length: height }, () => row));
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw)),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

const FIGURE = solidPng(640, 300);
const PROXY_PATH = "/api/threads/demo-thread/messages/demo-answer/image";

test.use({ deviceScaleFactor: 2 });

for (const theme of ["light", "dark"] as const) {
  test(`出典の画像だけを shiki-server 経由で読み、外部へは通信しない（${theme}）`, async ({ page }) => {
    await page.emulateMedia({ colorScheme: theme });
    const external: string[] = [];
    const proxied: string[] = [];
    page.on("request", (r: Request) => {
      if (new URL(r.url()).hostname.endsWith(".test")) external.push(r.url());
    });
    // 代理取得の応答。グラフだけは取得先が失敗した（502）ことにして、リンクへ落ちるかを見る。
    await page.route(new RegExp(`${PROXY_PATH.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\?`), async (route) => {
      const u = new URL(route.request().url()).searchParams.get("u") ?? "";
      proxied.push(u);
      if (u === CHART_IMAGE) return route.fulfill({ status: 502, body: "" });
      await route.fulfill({ status: 200, contentType: "image/png", body: FIGURE });
    });
    // 外部ホストへ出たら失敗させる（念のため。上の external で検出する）。
    await page.route(/\.test\//, (route) => route.abort());

    await page.goto("/reference/answer-images");
    const timeline = page.getByTestId("answer-images-timeline");

    // 出典の画像は代理取得の URL で描く。`u` は出典に書かれていた字句（回答側の `%61` ではない）。
    const all = timeline.getByTestId("answer-image");
    await expect(all).toHaveCount(2);
    const shown = all.first();
    await expect(shown).toHaveAttribute("src", `${PROXY_PATH}?u=${encodeURIComponent(PAGE_IMAGE)}`);
    await expect(shown).toHaveAttribute("referrerpolicy", "no-referrer");
    await expect(shown).toHaveAttribute("alt", "SaaS 市場の分野別シェア");
    await expect.poll(() => shown.evaluate((img: HTMLImageElement) => img.complete && img.naturalWidth)).toBe(640);
    // 描いた画像には出典のホストを添える（どこから来た画像かを読み手が判断できる）。
    await expect(timeline.getByTestId("answer-image-caption").first()).toHaveText(
      "SaaS 市場の分野別シェア · images.example.test",
    );
    await expect(all.nth(1)).toHaveAttribute("alt", "再掲");
    await expect(all.nth(1)).toHaveAttribute("src", `${PROXY_PATH}?u=${encodeURIComponent(PAGE_IMAGE)}`);

    // 出典の URL に何かを足したもの・出典に無いホストは読み込まずリンクにする。代理取得が失敗した
    // グラフも同じリンクに落ちる。
    const blocked = timeline.getByTestId("answer-image-blocked");
    await expect(blocked).toHaveCount(5);
    await expect(blocked.nth(0)).toContainText("外部の画像「補足の図」");
    await expect(blocked.nth(1)).toHaveText(/^外部の画像\s*collector\.attacker\.test$/);
    await expect(blocked.nth(1)).toHaveAttribute("href", "https://collector.attacker.test/p?d=Q3%20revenue%2012.4B");
    await expect(blocked.nth(2)).toHaveText(/^外部の画像「内訳グラフ」\s*charts\.example\.test$/);
    await expect(blocked.nth(2)).toHaveAttribute("title", /読み込めませんでした/);
    // モデル自身が書いた文字列（シェルの出力・封筒の外の案内）にだけ現れる URL は出所に数えない。
    await expect(blocked.nth(3)).toHaveText(/^外部の画像\s*collector\.attacker\.test$/);
    await expect(blocked.nth(3)).toHaveAttribute("href", "https://collector.attacker.test/e?d=Q3%20margin%2018%25");
    // リンクの中の画像は、外側のリンクを残してラベルだけにする（<a> を入れ子にしない）。
    await expect(blocked.nth(4)).toHaveText(/^外部の画像「build」\s*badge\.example\.test$/);
    await expect(timeline.locator("a a")).toHaveCount(0);
    await expect(timeline.locator("img")).toHaveCount(2);

    // 代理取得の経路が無い本文では、出典の画像も読み込まない（ブラウザから直接は読まない）。
    const noProxy = page.getByTestId("answer-images-no-proxy");
    await expect(noProxy.locator("img")).toHaveCount(0);
    await expect(noProxy.getByTestId("answer-image-blocked")).toHaveCount(7);

    await page.waitForLoadState("networkidle");
    // 外部ホストへは 1 度も要求していない。代理取得に渡したのは出典の字句そのものだけ。
    expect(external).toEqual([]);
    expect([...new Set(proxied)].sort()).toEqual([CHART_IMAGE, PAGE_IMAGE]);

    if (SHOTS) await timeline.screenshot({ path: `${SHOTS}/answer-images-${theme}.png` });
  });
}
