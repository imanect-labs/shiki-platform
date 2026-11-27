/// ホスト支援 PKCE の SDK の単体テスト（GAP-63）。
/// シェル（`window.parent`）を差し替えて、トークンとゲートウェイの場所を受け取れること、シェル以外の知らせを読まない
/// こと、トークンを期限まで使い回すことを確かめる（シェル側は web/src/components/miniapp/miniapp-frame.tsx）。

import assert from "node:assert/strict";
import { afterEach, beforeEach, test } from "node:test";

import { hostAssistedGateway, hostAssistedToken, requestHostToken } from "./index.ts";

type Listener = (ev: MessageEvent) => void;

/// iframe の中の window を模す。`answer` はシェルへの求め 1 回ごとに、シェルが返す知らせを決める。
function fakeFrame(answer: (n: number) => unknown, stranger?: unknown) {
  const listeners = new Set<Listener>();
  const requests: unknown[] = [];
  const parent = {
    postMessage(message: unknown) {
      requests.push(message);
      const n = requests.length;
      queueMicrotask(() => {
        // シェル以外（ほかの窓）からの知らせは読まない。
        if (stranger !== undefined) deliver(stranger, {});
        deliver(answer(n), parent);
      });
    },
  };
  function deliver(data: unknown, source: unknown) {
    for (const f of [...listeners]) f({ data, source } as MessageEvent);
  }
  const win = {
    parent,
    addEventListener: (_: string, f: Listener) => listeners.add(f),
    removeEventListener: (_: string, f: Listener) => listeners.delete(f),
  };
  (globalThis as { window?: unknown }).window = win;
  return { requests, listeners };
}

const realFetch = globalThis.fetch;
const realNow = Date.now;
let now = 1_000_000;
beforeEach(() => {
  now = 1_000_000;
  Date.now = () => now;
});
afterEach(() => {
  globalThis.fetch = realFetch;
  Date.now = realNow;
  delete (globalThis as { window?: unknown }).window;
});

const token = (n: number) => ({
  type: "shiki:token",
  accessToken: `tok-${n}`,
  expiresIn: 300,
  gatewayOrigin: "https://gw.example",
});

test("シェルからトークンとゲートウェイの場所を受け取る。シェル以外の知らせは読まない", async () => {
  const forged = { type: "shiki:token", accessToken: "forged", expiresIn: 300, gatewayOrigin: "https://evil.example" };
  const frame = fakeFrame(token, forged);
  const got = await requestHostToken(["llm.invoke"]);
  assert.deepEqual(got, { accessToken: "tok-1", expiresIn: 300, gatewayOrigin: "https://gw.example" });
  assert.deepEqual(frame.requests, [{ type: "shiki:token-request", scopes: ["llm.invoke"] }]);
  assert.equal(frame.listeners.size, 0, "受け取ったら聞くのをやめる");
  // これまでどおりトークンだけを返す供給も使える。
  assert.equal(await hostAssistedToken(["llm.invoke"])(), "tok-2");
});

test("シェルが断ると、その文で失敗する", async () => {
  fakeFrame(() => ({ type: "shiki:token-error", error: "ポップアップがブロックされました。許可して再試行してください" }));
  await assert.rejects(requestHostToken([]), /ポップアップがブロックされました/);
});

test("hostAssistedGateway はシェルが渡した場所を使い、トークンを期限の 30 秒前まで使い回す", async () => {
  const frame = fakeFrame(token);
  const seen: { url: string; auth: string }[] = [];
  globalThis.fetch = (async (url: string | URL | Request, init?: RequestInit) => {
    seen.push({ url: String(url), auth: (init?.headers as Record<string, string>).authorization });
    return Response.json({ user_sub: "u", app_id: "a", granted_scopes: [] });
  }) as typeof fetch;
  const gw = await hostAssistedGateway(["data.read"]);
  await gw.whoami();
  now += 269_000; // 期限（300 秒）の 31 秒前: まだ使い回す。
  await gw.whoami();
  now += 2_000; // 29 秒前: 求め直す。
  await Promise.all([gw.whoami(), gw.whoami()]);
  assert.deepEqual(
    seen.map((s) => s.url),
    Array(4).fill("https://gw.example/gw/whoami"),
  );
  assert.deepEqual(
    seen.map((s) => s.auth),
    ["Bearer tok-1", "Bearer tok-1", "Bearer tok-2", "Bearer tok-2"],
  );
  assert.equal(frame.requests.length, 2, "同時に求めた分は 1 回にまとめる");
});

test("シェルがゲートウェイの場所を渡さなければ hostAssistedGateway は失敗する", async () => {
  fakeFrame(() => ({ type: "shiki:token", accessToken: "tok", expiresIn: 300 }));
  await assert.rejects(hostAssistedGateway([]), /ゲートウェイの場所/);
});
