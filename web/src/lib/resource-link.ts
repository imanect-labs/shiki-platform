/// リソースのディープリンク URL 解決（共有リンクのポインタ・#338）。
///
/// 「リンクをコピー」はここで解決したページ URL をコピーするだけ（トークンは焼き込まない）。
/// 押下時の認可は通常の ReBAC チェックで、一般アクセス（組織内/全員）か明示共有の対象だけが開ける。
///
/// マッピングは `drive-browser.tsx` の open ディスパッチ（拡張子→ルート）と一致させる。

/// リンク解決に必要なノードの最小形（id・name・kind）。
export type LinkableNode = {
  id: string;
  name: string;
  /// "file" | "folder"（生成型では string。folder のみ判定に使う）。
  kind?: string;
  /// 非プレビューファイルのフォールバック（親フォルダを開く）に使う。
  parent_id?: string | null;
};

const OFFICE_EXTENSIONS = [".docx", ".xlsx", ".pptx", ".odt", ".ods", ".odp"];

function isOfficeFile(name: string): boolean {
  const lower = name.toLowerCase();
  return OFFICE_EXTENSIONS.some((ext) => lower.endsWith(ext));
}

/// ノードを開くための**パス**（origin なし）。フォルダやプレビュー不可ファイルも扱う。
export function resourcePath(node: LinkableNode): string {
  if (node.kind === "folder") return `/drive?folder=${node.id}`;
  const lower = node.name.toLowerCase();
  if (lower.endsWith(".md")) return `/notes/${node.id}`;
  if (lower.endsWith(".csv")) return `/csv/${node.id}`;
  if (lower.endsWith(".slide")) return `/slides/${node.id}`;
  if (isOfficeFile(node.name)) return `/office/${node.id}`;
  // 専用ページを持たないファイル（画像・PDF 等）は格納フォルダを開く（無ければドライブ直下）。
  return node.parent_id ? `/drive?folder=${node.parent_id}` : "/drive";
}

/// ノードを開くための**絶対 URL**（クリップボードにコピーする値）。
/// `unlock` を渡すとパスワード解錠プロンプトを促す UX ヒント `?unlock=1` を付す（認可には非依存）。
export function resourceUrl(node: LinkableNode, opts?: { unlock?: boolean }): string {
  const origin = typeof window !== "undefined" ? window.location.origin : "";
  const path = resourcePath(node);
  if (!opts?.unlock) return `${origin}${path}`;
  const sep = path.includes("?") ? "&" : "?";
  return `${origin}${path}${sep}unlock=1`;
}
