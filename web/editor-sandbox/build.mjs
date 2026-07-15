/// スライドエディタ砂箱バンドルのビルド（Task 11.2）。
///
/// GrapesJS＋ブリッジを **単一 self-contained HTML** に焼き込み、app-gateway の
/// 第3リスナ（apps オリジン）から配信する。外部リソース参照ゼロ
/// （CSP default-src 'none' で動く・エアギャップ配布可・PIT-33 と同型）。
///
/// 使い方: `pnpm build:editor-sandbox`（web/）。出力: editor-sandbox/dist/slide-editor.html

import { build } from "esbuild";
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { createHash } from "node:crypto";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));

const js = await build({
  entryPoints: [join(here, "src/main.ts")],
  bundle: true,
  minify: true,
  format: "iife",
  write: false,
  loader: { ".woff": "dataurl", ".woff2": "dataurl", ".svg": "dataurl" },
  logLevel: "warning",
});

const grapesCss = readFileSync(join(here, "../node_modules/grapesjs/dist/css/grapes.min.css"), "utf8");

// GrapesJS の UI アイコン（font-awesome 4）。CSP default-src 'none' で外部 CDN は
// 遮断されるため、woff2 を data URL に焼き込んで同梱する（エアギャップ配布可）。
const faDir = join(here, "../node_modules/font-awesome");
const faWoff2 = readFileSync(join(faDir, "fonts/fontawesome-webfont.woff2")).toString("base64");
// 後勝ちの @font-face で外部フォント参照を data URL に差し替える（元 CSS には触らない）。
const faCss =
  readFileSync(join(faDir, "css/font-awesome.min.css"), "utf8") +
  `\n@font-face{font-family:'FontAwesome';src:url(data:font/woff2;base64,${faWoff2}) format('woff2');font-weight:normal;font-style:normal}`;

// エディタ chrome の最小スタイル（GrapesJS の UI を全画面に敷く）。
const SHELL_CSS = `
  html, body { margin: 0; height: 100%; overflow: hidden;
    font-family: "Hiragino Sans", "Noto Sans JP", system-ui, sans-serif; }
  #gjs { height: 100%; border: 0; }
  .gjs-one-bg { background-color: #fafaf9; }
  .gjs-two-color { color: #44403c; }
  .gjs-three-bg { background-color: #1c1917; color: #fafaf9; }
  .gjs-four-color, .gjs-four-color-h:hover { color: #16a34a; }
`;

const html = `<!doctype html>
<html lang="ja">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>shiki スライドエディタ</title>
<style>${faCss}</style>
<style>${grapesCss}</style>
<style>${SHELL_CSS}</style>
</head>
<body>
<div id="gjs"></div>
<script>${js.outputFiles[0].text}</script>
</body>
</html>
`;

mkdirSync(join(here, "dist"), { recursive: true });
const out = join(here, "dist/slide-editor.html");
writeFileSync(out, html);
const sha = createHash("sha256").update(html).digest("hex");
writeFileSync(join(here, "dist/slide-editor.sha256"), `${sha}\n`);
console.log(`built: ${out}\nsha256: ${sha}\nbytes: ${Buffer.byteLength(html)}`);
