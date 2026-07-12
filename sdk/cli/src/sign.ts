/// マニフェスト署名（Task 9.14・ed25519・crates/app-platform/src/sign.rs と同一契約）。
///
/// 署名対象は **canonical manifest digest**（キー再帰ソート JSON の sha256 hex）の UTF-8。
/// backend の `manifest_digest`（serde_json::to_value → canonical）と同じ結果を出す必要がある。
/// Node の webcrypto（Ed25519）を使う。

import { webcrypto } from "node:crypto";

const subtle = webcrypto.subtle;

/// JSON をキー再帰ソートで正準化する（backend `canonical_json` と一致させる）。
///
/// serde の `skip_serializing_if = "Option::is_none"` に合わせ、**null 値のキーは落とす**
/// （backend は None フィールドを直列化しないため）。空配列は残す（Vec フィールドは
/// skip 指定がなく `[]` として出るため）。
export function canonicalize(value: unknown): string {
  return JSON.stringify(sortKeys(value));
}

function sortKeys(v: unknown): unknown {
  if (Array.isArray(v)) return v.map(sortKeys);
  if (v && typeof v === "object") {
    const out: Record<string, unknown> = {};
    for (const k of Object.keys(v as Record<string, unknown>).sort()) {
      const val = (v as Record<string, unknown>)[k];
      if (val === null || val === undefined) continue; // skip_serializing_if(is_none) 相当
      out[k] = sortKeys(val);
    }
    return out;
  }
  return v;
}

async function sha256Hex(bytes: Uint8Array): Promise<string> {
  const digest = await subtle.digest("SHA-256", bytes);
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/// マニフェストの canonical digest（sha256 hex）。
export async function manifestDigest(manifest: unknown): Promise<string> {
  return sha256Hex(new TextEncoder().encode(canonicalize(manifest)));
}

/// digest（hex 文字列）へ ed25519 署名し hex を返す（秘密鍵は 32 バイト raw）。
export async function signManifest(manifest: unknown, secretKeyRaw: Uint8Array): Promise<string> {
  const digest = await manifestDigest(manifest);
  const key = await subtle.importKey("raw", secretKeyRaw, { name: "Ed25519" }, false, ["sign"]);
  const sig = await subtle.sign("Ed25519", key, new TextEncoder().encode(digest));
  return [...new Uint8Array(sig)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/// 新しい ed25519 鍵ペアを生成する（raw private/public の hex）。init/dev 用。
export async function generateKeypair(): Promise<{ privateHex: string; publicHex: string }> {
  const pair = (await subtle.generateKey({ name: "Ed25519" }, true, [
    "sign",
    "verify",
  ])) as CryptoKeyPair;
  const priv = new Uint8Array(await subtle.exportKey("pkcs8", pair.privateKey));
  const pub = new Uint8Array(await subtle.exportKey("raw", pair.publicKey));
  // pkcs8 末尾 32 バイトが raw seed（ed25519）。
  const seed = priv.slice(priv.length - 32);
  const hex = (b: Uint8Array) => [...b].map((x) => x.toString(16).padStart(2, "0")).join("");
  return { privateHex: hex(seed), publicHex: hex(pub) };
}

export function hexToBytes(hex: string): Uint8Array {
  const clean = hex.trim();
  const out = new Uint8Array(clean.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(clean.slice(i * 2, i * 2 + 2), 16);
  return out;
}
