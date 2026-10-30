/// ミニアプリ SDK の単体テスト（D-117・BUG-140）。
/// ゲートウェイが断った応答の文言と理由（`code`）を GatewayError で受け取れること、AI の呼び出しが SSE を読むことを、
/// fetch を差し替えて確かめる（ゲートウェイの応答の形は crates/app-gateway の ai_it が固定する）。

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";

import { GatewayError, ShikiGateway } from "./index.ts";

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

type Seen = { url: string; init: RequestInit | undefined };

/// 次の fetch に `res` を返し、呼ばれた URL と要求を `seen` に残す。
function respond(res: Response, seen: Seen[] = []): Seen[] {
  globalThis.fetch = (async (url: string | URL | Request, init?: RequestInit) => {
    seen.push({ url: String(url), init });
    return res;
  }) as typeof fetch;
  return seen;
}

const gw = () => new ShikiGateway({ baseUrl: "https://gw.example", getToken: () => "tok" });

const ask = { messages: [{ role: "user" as const, content: [{ type: "text", text: "hi" }] }], model: "stub-n" };

test("AI のモデルを断られると、文言と理由を GatewayError で受け取れる", async () => {
  const message =
    "この組織では AI のモデルを選べないため、「stub-n」は使えません。モデルを指定しないで呼び出すと、組織の既定のモデルで動きます。";
  const seen = respond(
    Response.json({ status: 400, code: "model_choice_disabled", error: message }, { status: 400 }),
  );
  const events: string[] = [];
  const err = await gw()
    .invokeLlm(ask, (type) => events.push(type))
    .catch((e: unknown) => e);
  assert.ok(err instanceof GatewayError);
  assert.equal(err.message, message);
  assert.equal(err.status, 400);
  assert.equal(err.code, "model_choice_disabled");
  assert.deepEqual(events, []);
  // POST で本文と Bearer を送る（以前は SSE を GET でしか読めず、AI を呼べなかった）。
  assert.equal(seen[0].url, "https://gw.example/gw/ai/llm/invoke");
  assert.equal(seen[0].init?.method, "POST");
  assert.equal(JSON.parse(String(seen[0].init?.body)).model, "stub-n");
  assert.equal((seen[0].init?.headers as Record<string, string>).authorization, "Bearer tok");
});

test("AI エージェントと JSON の API も同じ形で断られる。理由の無い誤りの code は undefined", async () => {
  respond(Response.json({ code: "model_unavailable", error: "使えません" }, { status: 400 }));
  const agent = await gw()
    .invokeAgent({ prompt: "調べて", model: "x" }, () => {})
    .catch((e: unknown) => e);
  assert.ok(agent instanceof GatewayError);
  assert.equal(agent.code, "model_unavailable");

  respond(Response.json({ error: "権限がありません: この操作は許可されていません" }, { status: 403 }));
  const data = await gw()
    .listTables()
    .catch((e: unknown) => e);
  assert.ok(data instanceof GatewayError);
  assert.equal(data.message, "権限がありません: この操作は許可されていません");
  assert.equal(data.code, undefined);

  // 本文の無い応答は状態だけを言う。
  respond(new Response(null, { status: 502 }));
  const bare = await gw()
    .subscribe("/gw/events", () => {})
    .catch((e: unknown) => e);
  assert.ok(bare instanceof GatewayError);
  assert.equal(bare.message, "HTTP 502");
  assert.equal(bare.status, 502);
});

test("AI の答えは SSE のフレームごとに受け取る", async () => {
  const body = [
    'event: text\ndata: {"type":"text_delta","text":"こんにちは"}\n\n',
    'event: done\ndata: {"type":"done"}\n\n',
  ].join("");
  respond(new Response(body, { status: 200, headers: { "content-type": "text/event-stream" } }));
  const got: [string, unknown][] = [];
  await gw().invokeLlm(ask, (type, data) => got.push([type, data]));
  assert.deepEqual(got, [
    ["text", { type: "text_delta", text: "こんにちは" }],
    ["done", { type: "done" }],
  ]);
});
