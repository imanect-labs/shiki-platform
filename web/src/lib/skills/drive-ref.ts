/// スキル本文のドライブ参照（#919）。
///
/// 本文（SKILL.md）には Markdown リンク `[パス](shiki://drive/file/<id>)` として保存する。
/// スキルを使う AI はリンク先の id をそのまま doc_read 等に渡せ、エディタはリンクを
/// パスのラベル（driveRef ノード）として描く。リンクの文字列は挿入時のパスで、表示は
/// 閲覧者本人の権限で引き直す（読めなければ「アクセスできない項目」として出す）。

export type DriveRefKind = "file" | "folder";

export type DriveRef = { kind: DriveRefKind; id: string; label: string };

const SCHEME = "shiki://drive/";
/// node id は UUID（大文字小文字を問わない）。それ以外の形はリンクとして扱わない。
const HREF_RE =
  /^shiki:\/\/drive\/(file|folder)\/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$/i;

const NODE_ID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/// storage node id（UUID）の形か。HTML の貼り付けなど外から来た id はこれで確かめてから使う
/// （リンク先や API のパスへそのまま埋めると、文字や別のパスを差し込まれる）。
export function isNodeId(id: string): boolean {
  return NODE_ID_RE.test(id);
}

export function driveRefHref(kind: DriveRefKind, id: string): string {
  return `${SCHEME}${kind}/${id}`;
}

/// リンク先がドライブ参照なら種別と id を返す（それ以外は null）。
export function parseDriveRefHref(href: string): { kind: DriveRefKind; id: string } | null {
  const m = HREF_RE.exec(href.trim());
  if (!m) return null;
  return { kind: m[1].toLowerCase() as DriveRefKind, id: m[2].toLowerCase() };
}
