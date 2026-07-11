/// CLI 署名ユーティリティの単体テスト（Task 9.14）。
/// canonical の決定性（キーソート・null 落とし）と ed25519 往復を検証する。

import assert from "node:assert/strict";
import { test } from "node:test";

import { canonicalize, manifestDigest, signManifest, generateKeypair, hexToBytes } from "./sign.ts";

test("canonicalize はキーをソートし null を落とす", () => {
  const a = canonicalize({ b: 1, a: 2, z: null, nested: { y: 1, x: 2 } });
  assert.equal(a, '{"a":2,"b":1,"nested":{"x":2,"y":1}}');
  // キー順が違っても同じ canonical。
  assert.equal(canonicalize({ a: 2, b: 1, nested: { x: 2, y: 1 } }), a);
  // 空配列は残す（serde の Vec は skip されない）。
  assert.equal(canonicalize({ arr: [] }), '{"arr":[]}');
});

test("manifestDigest は決定的（同じ内容→同じ hex）", async () => {
  const m = { name: "x", version: "1.0.0", requested_scopes: ["data.read"], frontend: null };
  const d1 = await manifestDigest(m);
  const d2 = await manifestDigest({ frontend: null, requested_scopes: ["data.read"], version: "1.0.0", name: "x" });
  assert.equal(d1, d2);
  assert.match(d1, /^[0-9a-f]{64}$/);
});

test("ed25519 署名往復と改竄検知", async () => {
  const { privateHex, publicHex } = await generateKeypair();
  const manifest = { name: "expense", version: "1.0.0", requested_scopes: ["data.read"] };
  const sig = await signManifest(manifest, hexToBytes(privateHex));
  assert.match(sig, /^[0-9a-f]{128}$/);

  // webcrypto で検証（backend の ed25519-dalek と同じ raw 公開鍵で検証できる）。
  const { webcrypto } = await import("node:crypto");
  const key = await webcrypto.subtle.importKey(
    "raw",
    hexToBytes(publicHex),
    { name: "Ed25519" },
    false,
    ["verify"],
  );
  const digest = await manifestDigest(manifest);
  const ok = await webcrypto.subtle.verify(
    "Ed25519",
    key,
    hexToBytes(sig),
    new TextEncoder().encode(digest),
  );
  assert.equal(ok, true);
  // 改竄マニフェストは検証失敗。
  const badDigest = await manifestDigest({ ...manifest, name: "evil" });
  const bad = await webcrypto.subtle.verify(
    "Ed25519",
    key,
    hexToBytes(sig),
    new TextEncoder().encode(badDigest),
  );
  assert.equal(bad, false);
});
