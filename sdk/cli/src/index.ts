#!/usr/bin/env node
/// shiki ミニアプリ CLI（Task 9.14）: `shiki app init | dev | publish`。
///
/// - init: 雛形（manifest.json＋src/app.ts＋src/server.ts）を作る
/// - dev: フロントを esbuild で単一 HTML に固め、dev サーバ（BFF 同一オリジン）へ bundle upload
/// - publish: フロント＋サーバを固め、sha256 でマニフェストへ焼き、ed25519 署名して registry へ
///   import（オフライン）or manifest publish（オンライン）
///
/// 「我々（shiki チーム）が実装→簡単デプロイ」を CLI のみで完結させる（DoD）。

import { readFile, writeFile, mkdir } from "node:fs/promises";
import { existsSync } from "node:fs";
import { join } from "node:path";

import { manifestDigest, signManifest, generateKeypair, hexToBytes } from "./sign.ts";

const HELP = `shiki app <command>

  init [name]           雛形とマニフェストを作成
  dev [--api URL]       フロントを固めて dev BFF に bundle upload（manifest 更新）
  publish [--api URL] [--key HEX] [--offline]
                        フロント/サーバを固め・署名して registry へ登録

環境:
  SHIKI_API   BFF/サーバのベース URL（既定 http://localhost:8080）
  SHIKI_COOKIE  セッション Cookie（shiki_session=...; shiki_csrf=...）
  SHIKI_SIGNING_KEY  ed25519 秘密鍵（hex 64）。未指定は init 生成鍵を使う
`;

interface Manifest {
  name: string;
  version: string;
  description?: string;
  requested_scopes: string[];
  tools: string[];
  tables: unknown[];
  workflows: string[];
  budget: Record<string, unknown>;
  frontend: { bundle_key: string; sha256: string } | null;
  server: {
    code_sha256?: string;
    functions: string[];
    egress_allowlist: string[];
    events: string[];
    cron: { function: string; expr: string }[];
  } | null;
  trust_tier: "first_party" | "in_house" | "marketplace";
}

function apiBase(argv: Record<string, string | boolean>): string {
  return (argv.api as string) ?? process.env.SHIKI_API ?? "http://localhost:8080";
}

function parseArgs(args: string[]): { positional: string[]; flags: Record<string, string | boolean> } {
  const positional: string[] = [];
  const flags: Record<string, string | boolean> = {};
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (a.startsWith("--")) {
      const key = a.slice(2);
      const next = args[i + 1];
      if (next && !next.startsWith("--")) {
        flags[key] = next;
        i++;
      } else {
        flags[key] = true;
      }
    } else {
      positional.push(a);
    }
  }
  return { positional, flags };
}

async function csrfHeaders(): Promise<Record<string, string>> {
  const cookie = process.env.SHIKI_COOKIE ?? "";
  const csrf = /shiki_csrf=([^;]+)/.exec(cookie)?.[1] ?? "";
  return { cookie, "x-csrf-token": csrf };
}

async function apiJson(base: string, method: string, path: string, body?: unknown): Promise<unknown> {
  const res = await fetch(`${base}/api${path}`, {
    method,
    headers: {
      "content-type": "application/json",
      ...(await csrfHeaders()),
    },
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) throw new Error(`${method} ${path}: HTTP ${res.status} ${await res.text().catch(() => "")}`);
  return res.status === 204 ? undefined : res.json();
}

/// フロント src/app.ts を単一 self-contained HTML に固める（esbuild・IIFE・インライン）。
async function bundleFrontend(dir: string): Promise<string | null> {
  const entry = join(dir, "src/app.ts");
  if (!existsSync(entry)) return null;
  const esbuild = await import("esbuild");
  const result = await esbuild.build({
    entryPoints: [entry],
    bundle: true,
    format: "iife",
    minify: true,
    write: false,
    platform: "browser",
  });
  const js = result.outputFiles[0].text;
  return `<!doctype html><html><head><meta charset="utf-8"></head><body><div id="app"></div><script>${js}</script></body></html>`;
}

/// サーバ src/server.ts を単一 JS に固める（esbuild・function main(input) を export 前提）。
async function bundleServer(dir: string): Promise<string | null> {
  const entry = join(dir, "src/server.ts");
  if (!existsSync(entry)) return null;
  const esbuild = await import("esbuild");
  const result = await esbuild.build({
    entryPoints: [entry],
    bundle: true,
    format: "iife",
    globalName: "__app",
    write: false,
    platform: "neutral",
  });
  // script-runtime は function main(input) を呼ぶ。globalName の main を露出する。
  return `${result.outputFiles[0].text}\nfunction main(input){return __app.main(input);}`;
}

async function cmdInit(name: string): Promise<void> {
  const dir = name;
  await mkdir(join(dir, "src"), { recursive: true });
  const { privateHex, publicHex } = await generateKeypair();
  const manifest: Manifest = {
    name,
    version: "0.1.0",
    description: `${name} ミニアプリ`,
    requested_scopes: ["data.read", "data.write"],
    tools: [],
    tables: [],
    workflows: [],
    budget: {},
    frontend: null,
    server: null,
    trust_tier: "in_house",
  };
  await writeFile(join(dir, "manifest.json"), JSON.stringify(manifest, null, 2));
  await writeFile(
    join(dir, "src/app.ts"),
    `// ${name} フロント（B1）。ホスト支援 PKCE でゲートウェイを叩く。\n` +
      `const el = document.getElementById("app");\nif (el) el.textContent = "hello ${name}";\n`,
  );
  await writeFile(
    join(dir, "src/server.ts"),
    `// ${name} サーバ関数（B2）。Shiki.* はホストがゲートウェイへ委譲する。\n` +
      `export function main(input: { function: string; payload: unknown }) {\n` +
      `  return { ok: true, echo: input.payload };\n}\n`,
  );
  await writeFile(join(dir, ".shiki-key"), `${privateHex}\n`);
  await writeFile(join(dir, ".shiki-pubkey"), `${publicHex}\n`);
  console.log(`初期化しました: ${dir}/`);
  console.log(`  署名鍵: .shiki-key（秘匿・公開鍵は .shiki-pubkey）`);
  console.log(`  管理者に公開鍵を /admin/trusted-keys へ登録してもらうと first-party 化できます`);
}

async function loadManifest(dir: string): Promise<Manifest> {
  return JSON.parse(await readFile(join(dir, "manifest.json"), "utf8")) as Manifest;
}

/// フロント/サーバを固め、sha256 をマニフェストへ焼き、bundle を upload する。
async function packAndUpload(
  base: string,
  dir: string,
  manifest: Manifest,
  artifactId: string,
): Promise<Manifest> {
  const front = await bundleFrontend(dir);
  if (front) {
    const sha = await uploadBundle(base, artifactId, front, "text/html");
    manifest.frontend = { bundle_key: sha, sha256: sha };
  }
  const server = await bundleServer(dir);
  if (server) {
    const sha = await uploadBundle(base, artifactId, server, "text/plain");
    manifest.server = {
      code_sha256: sha,
      functions: manifest.server?.functions ?? ["main"],
      egress_allowlist: manifest.server?.egress_allowlist ?? [],
      events: manifest.server?.events ?? [],
      cron: manifest.server?.cron ?? [],
    };
  }
  return manifest;
}

async function uploadBundle(
  base: string,
  artifactId: string,
  content: string,
  contentType: string,
): Promise<string> {
  const res = await fetch(`${base}/api/apps/manifests/${artifactId}/bundle`, {
    method: "POST",
    headers: { "content-type": contentType, ...(await csrfHeaders()) },
    body: content,
  });
  if (!res.ok) throw new Error(`bundle upload: HTTP ${res.status}`);
  return ((await res.json()) as { sha256: string }).sha256;
}

async function cmdDev(dir: string, flags: Record<string, string | boolean>): Promise<void> {
  const base = apiBase(flags);
  let manifest = await loadManifest(dir);
  // artifact を作成（or 既存 id 未管理なので毎回作る＝dev は使い捨て）。
  const created = (await apiJson(base, "POST", "/apps/manifests", { manifest })) as { id: string };
  manifest = await packAndUpload(base, dir, manifest, created.id);
  await apiJson(base, "PUT", `/apps/manifests/${created.id}`, {
    manifest,
    expected_version: 1,
  });
  console.log(`dev: アップロードしました（artifact ${created.id}・v2）`);
  console.log(`  ゲートウェイ経由で起動: /apps/${created.id}`);
}

async function cmdPublish(dir: string, flags: Record<string, string | boolean>): Promise<void> {
  const base = apiBase(flags);
  let manifest = await loadManifest(dir);
  const created = (await apiJson(base, "POST", "/apps/manifests", { manifest })) as { id: string };
  manifest = await packAndUpload(base, dir, manifest, created.id);
  await apiJson(base, "PUT", `/apps/manifests/${created.id}`, { manifest, expected_version: 1 });

  const keyHex =
    (flags.key as string) ??
    process.env.SHIKI_SIGNING_KEY ??
    (existsSync(join(dir, ".shiki-key"))
      ? (await readFile(join(dir, ".shiki-key"), "utf8")).trim()
      : undefined);

  if (flags.offline) {
    if (!keyHex) throw new Error("offline publish には署名鍵が必要です（--key / SHIKI_SIGNING_KEY）");
    const keyId = (flags["key-id"] as string) ?? "cli-key";
    const signatureHex = await signManifest(manifest, hexToBytes(keyHex));
    const entry = await apiJson(base, "POST", "/apps/registry/import", {
      manifest,
      signature_hex: signatureHex,
      key_id: keyId,
    });
    console.log(`publish（オフライン import・署名検証済み）:`, JSON.stringify(entry));
  } else {
    const entry = await apiJson(base, "POST", `/apps/manifests/${created.id}/publish`, {});
    console.log(`publish（オンライン）:`, JSON.stringify(entry));
    if (keyHex) {
      const digest = await manifestDigest(manifest);
      console.log(`  manifest digest: ${digest}`);
    }
  }
}

async function main(): Promise<void> {
  const argv = process.argv.slice(2);
  // `shiki app <cmd>` と `shiki <cmd>` 両対応。
  const args = argv[0] === "app" ? argv.slice(1) : argv;
  const { positional, flags } = parseArgs(args.slice(1));
  const cmd = args[0];
  try {
    switch (cmd) {
      case "init":
        await cmdInit(positional[0] ?? "my-app");
        break;
      case "dev":
        await cmdDev(positional[0] ?? ".", flags);
        break;
      case "publish":
        await cmdPublish(positional[0] ?? ".", flags);
        break;
      default:
        console.log(HELP);
        process.exitCode = cmd ? 1 : 0;
    }
  } catch (e) {
    console.error(`エラー: ${e instanceof Error ? e.message : String(e)}`);
    process.exitCode = 1;
  }
}

void main();
