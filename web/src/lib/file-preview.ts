/// ドライブのファイルをブラウザでどう見せるかの判定（#784）。
///
/// 専用ビューア（ノート・CSV・スライド・Office・PDF）を持たないファイルは `/file/[id]` で開く。
/// そこで、ブラウザが表示できる形式（テキスト・画像・動画・音声）は中身をその場で見せ、
/// それ以外は「プレビューできません」画面にする。判定は拡張子を優先し、無ければ content-type を見る
/// （アップロード時の content-type は application/octet-stream になりがちなため）。

export type PreviewKind = "text" | "image" | "video" | "audio";

/// ブラウザの `<img>` が表示できる画像（HEIC などは content-type が image/ でも外す）。
const IMAGE_EXTENSIONS = ["png", "jpg", "jpeg", "gif", "webp", "avif", "svg", "bmp", "ico"];
/// 多くのブラウザが `<img>` で表示できない画像。
const UNSUPPORTED_IMAGE_TYPES = ["image/heic", "image/heif", "image/tiff"];
const VIDEO_EXTENSIONS = ["mp4", "m4v", "webm", "mov", "ogv"];
const AUDIO_EXTENSIONS = ["mp3", "m4a", "aac", "wav", "ogg", "oga", "opus", "flac", "weba"];

/// 拡張子 → shiki の言語名。ここに無い拡張子でもテキストとして開くものは `TEXT_EXTENSIONS` に置く。
const LANGUAGES: Record<string, string> = {
  json: "json",
  jsonc: "jsonc",
  jsonl: "json",
  ndjson: "json",
  yaml: "yaml",
  yml: "yaml",
  toml: "toml",
  xml: "xml",
  html: "html",
  htm: "html",
  css: "css",
  scss: "scss",
  js: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  jsx: "jsx",
  ts: "typescript",
  mts: "typescript",
  cts: "typescript",
  tsx: "tsx",
  py: "python",
  rb: "ruby",
  rs: "rust",
  go: "go",
  java: "java",
  kt: "kotlin",
  swift: "swift",
  c: "c",
  h: "c",
  cpp: "cpp",
  cc: "cpp",
  hpp: "cpp",
  cs: "csharp",
  php: "php",
  sh: "shellscript",
  bash: "shellscript",
  zsh: "shellscript",
  ps1: "powershell",
  sql: "sql",
  graphql: "graphql",
  proto: "proto",
  tf: "hcl",
  hcl: "hcl",
  ini: "ini",
  dockerfile: "docker",
  makefile: "make",
  diff: "diff",
  patch: "diff",
  vue: "vue",
  svelte: "svelte",
  lua: "lua",
  r: "r",
  dart: "dart",
  scala: "scala",
  markdown: "markdown",
};

/// 言語を付けずにそのまま見せるテキスト。
const TEXT_EXTENSIONS = ["txt", "log", "text", "tsv", "conf", "cfg", "env", "properties", "gitignore", "editorconfig"];

/// 拡張子を持たないがテキストとして知られた名前（Dockerfile など）。
const TEXT_FILENAMES: Record<string, string> = {
  dockerfile: "docker",
  makefile: "make",
  license: "text",
  readme: "text",
};

/// テキストとして扱う content-type（text/* 以外）。
const TEXT_CONTENT_TYPES = [
  "application/json",
  "application/xml",
  "application/javascript",
  "application/x-javascript",
  "application/typescript",
  "application/x-yaml",
  "application/yaml",
  "application/toml",
  "application/x-sh",
  "application/sql",
  "application/graphql",
  "application/x-ndjson",
];

function extension(name: string): string {
  const lower = name.toLowerCase();
  const dot = lower.lastIndexOf(".");
  // 先頭のドットだけ（.gitignore など）は拡張子として扱う。
  return dot >= 0 ? lower.slice(dot + 1) : "";
}

function baseContentType(contentType?: string | null): string {
  return (contentType ?? "").split(";")[0].trim().toLowerCase();
}

/// このファイルをどう見せるか。表示できない形式は null。
export function previewKind(node: { name: string; content_type?: string | null }): PreviewKind | null {
  const ext = extension(node.name);
  // .ts は TypeScript と MPEG-TS（動画）で重なる。content-type が動画ならそちらを採る。
  if (ext === "ts" && baseContentType(node.content_type).startsWith("video/")) return "video";
  if (IMAGE_EXTENSIONS.includes(ext)) return "image";
  if (VIDEO_EXTENSIONS.includes(ext)) return "video";
  if (AUDIO_EXTENSIONS.includes(ext)) return "audio";
  if (Object.hasOwn(LANGUAGES, ext) || TEXT_EXTENSIONS.includes(ext)) return "text";
  if (Object.hasOwn(TEXT_FILENAMES, node.name.toLowerCase())) return "text";
  // 拡張子で決まらないものは content-type を見る。
  const ct = baseContentType(node.content_type);
  if (ct.startsWith("image/") && !UNSUPPORTED_IMAGE_TYPES.includes(ct)) return "image";
  if (ct.startsWith("video/")) return "video";
  if (ct.startsWith("audio/")) return "audio";
  if (ct.startsWith("text/") || TEXT_CONTENT_TYPES.includes(ct) || ct.endsWith("+json") || ct.endsWith("+xml"))
    return "text";
  return null;
}

/// テキストの言語（shiki の言語名）。分からなければ "text"。
export function textLanguage(name: string): string {
  const lower = name.toLowerCase();
  // 辞書は自身のキーだけを見る（`__proto__` という名前のファイルで Object.prototype を拾わない）。
  if (Object.hasOwn(TEXT_FILENAMES, lower)) return TEXT_FILENAMES[lower];
  const ext = extension(name);
  return Object.hasOwn(LANGUAGES, ext) ? LANGUAGES[ext] : "text";
}

/// 中身を取りに行くテキストの上限。これを超えるものは開かずに「大きすぎる」と出す。
/// 全行を DOM に置く（仮想化しない）ので、数十万行で描画が固まらない大きさに抑える。
export const TEXT_MAX_BYTES = 2 * 1024 * 1024;
export const HIGHLIGHT_MAX_BYTES = 256 * 1024;
/// 行数の上限。短い行（改行だけなど）が大量にあると、2MB 未満でも行の要素が数百万になって固まる。
export const TEXT_MAX_LINES = 50_000;

/// 先頭に NUL 文字があるものはバイナリとみなす（拡張子が .txt でも中身が違うことがある）。
export function looksBinary(bytes: Uint8Array): boolean {
  const n = Math.min(bytes.length, 8000);
  for (let i = 0; i < n; i++) if (bytes[i] === 0) return true;
  return false;
}

/// 見出しに出す言語名。
const LANGUAGE_LABELS: Record<string, string> = {
  javascript: "JavaScript",
  typescript: "TypeScript",
  jsx: "JSX",
  tsx: "TSX",
  python: "Python",
  ruby: "Ruby",
  rust: "Rust",
  go: "Go",
  java: "Java",
  kotlin: "Kotlin",
  swift: "Swift",
  c: "C",
  cpp: "C++",
  csharp: "C#",
  php: "PHP",
  shellscript: "Shell",
  powershell: "PowerShell",
  sql: "SQL",
  graphql: "GraphQL",
  proto: "Protocol Buffers",
  hcl: "HCL",
  json: "JSON",
  jsonc: "JSON",
  yaml: "YAML",
  toml: "TOML",
  xml: "XML",
  html: "HTML",
  css: "CSS",
  scss: "SCSS",
  ini: "INI",
  docker: "Dockerfile",
  make: "Makefile",
  diff: "Diff",
  vue: "Vue",
  svelte: "Svelte",
  lua: "Lua",
  r: "R",
  dart: "Dart",
  scala: "Scala",
  markdown: "Markdown",
};

export function languageLabel(lang: string, name: string): string {
  if (lang !== "text") return Object.hasOwn(LANGUAGE_LABELS, lang) ? LANGUAGE_LABELS[lang] : lang;
  const ext = extension(name);
  if (ext === "log") return "ログ";
  if (ext === "tsv") return "TSV";
  return "テキスト";
}

/// 言語の目印の色（GitHub の言語色に近いもの。無いものは灰色）。
const LANGUAGE_COLORS: Record<string, string> = {
  javascript: "#f1e05a",
  jsx: "#f1e05a",
  typescript: "#3178c6",
  tsx: "#3178c6",
  python: "#3572a5",
  ruby: "#701516",
  rust: "#dea584",
  go: "#00add8",
  java: "#b07219",
  kotlin: "#a97bff",
  swift: "#f05138",
  c: "#555555",
  cpp: "#f34b7d",
  csharp: "#178600",
  php: "#4f5d95",
  shellscript: "#89e051",
  sql: "#e38c00",
  json: "#cbcb41",
  jsonc: "#cbcb41",
  yaml: "#cb171e",
  toml: "#9c4221",
  html: "#e34c26",
  css: "#563d7c",
  scss: "#c6538c",
  vue: "#41b883",
  svelte: "#ff3e00",
  dart: "#00b4ab",
  markdown: "#083fa1",
};

export function languageColor(lang: string): string {
  return Object.hasOwn(LANGUAGE_COLORS, lang) ? LANGUAGE_COLORS[lang] : "var(--muted-foreground)";
}
