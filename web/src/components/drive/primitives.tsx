"use client";

import * as React from "react";
import {
  ArrowDown,
  ArrowUp,
  ChevronDown,
  ChevronRight,
  File as FileIcon,
  FileArchive,
  FileAudio,
  FileCode,
  FileImage,
  FileSpreadsheet,
  FileText,
  FileVideo,
  Folder,
  Home,
  Loader2,
  Presentation,
} from "lucide-react";

import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { previewKind, textLanguage } from "@/lib/file-preview";
import { formatBytes } from "@/lib/format";
import type { CrumbResponse, NodeResponse, SortField } from "@/lib/storage";
import { cn } from "@/lib/utils";

/// 一覧の列レイアウト（ヘッダーと各行で共有）。OneDrive 風に
/// 名前（可変）｜更新日時｜更新者｜サイズ｜共有｜操作。
/// 段階的に列を出す: 最小=名前+操作 / `@xl`=更新日時+サイズ追加 / `@4xl`=更新者+共有も。
/// セルの DOM 順は常に固定し、隠す列は hidden で外す（grid 自動配置が崩れない）。
///
/// 閾値は**ビューポートではなくコンテナ（一覧自身）の幅**で判定する
///（drive-browser の `@container`）。ビューポート基準だと、サイドバーが出現する 768px と
/// 列が増える 1024px の 2 箇所で名前列だけが 60px 台まで潰れていた（#466）。
///
/// 付加列を足した後も名前列に 230px 以上残るよう閾値を選ぶ:
///   - `@xl`（36rem=576px）で 更新日時 ＋ サイズ → 名前 = 576 − 340 = 236px
///   - `@4xl`（56rem=896px）で 更新者 ＋ 共有   → 名前 = 896 − 588 = 308px
/// （固定幅 ＋ gap-3 ＋ 行の px-3 を差し引いた値。列を増やす時はこの計算も更新する）
export const LIST_GRID =
  "grid items-center gap-3 grid-cols-[minmax(0,1fr)_40px] " +
  "@xl:grid-cols-[minmax(0,1fr)_9.5rem_5.5rem_40px] " +
  "@4xl:grid-cols-[minmax(0,1fr)_9.5rem_8rem_5.5rem_6rem_40px]";

/// 拡張子/Content-Type からファイル種別アイコンと色を決める（OneDrive 風）。
/// 種別色はファイルアイコンの慣習色（PDF=赤・Word=青・Excel=緑・PowerPoint=橙…）に合わせる。
function fileIcon(name: string, contentType?: string | null): { Icon: typeof FileIcon; color: string } {
  const ext = name.toLowerCase().split(".").pop() ?? "";
  const ct = (contentType ?? "").toLowerCase();
  const is = (...exts: string[]) => exts.includes(ext);

  if (ext === "pdf" || ct === "application/pdf") return { Icon: FileText, color: "text-red-500" };
  if (is("doc", "docx", "rtf") || ct.includes("word")) return { Icon: FileText, color: "text-blue-600" };
  if (is("xls", "xlsx", "csv") || ct.includes("sheet") || ct.includes("excel"))
    return { Icon: FileSpreadsheet, color: "text-green-600" };
  if (is("ppt", "pptx") || ct.includes("presentation") || ct.includes("powerpoint"))
    return { Icon: Presentation, color: "text-orange-500" };
  if (is("png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "ico") || ct.startsWith("image/"))
    return { Icon: FileImage, color: "text-purple-500" };
  if (is("zip", "tar", "gz", "rar", "7z") || ct.includes("zip"))
    return { Icon: FileArchive, color: "text-amber-600" };
  if (is("json", "js", "ts", "tsx", "jsx", "py", "rs", "go", "java", "html", "css", "yml", "yaml", "toml", "sh"))
    return { Icon: FileCode, color: "text-sky-600" };
  if (is("mp3", "wav", "flac", "m4a", "aac") || ct.startsWith("audio/"))
    return { Icon: FileAudio, color: "text-pink-500" };
  if (is("mp4", "mov", "avi", "mkv", "webm") || ct.startsWith("video/"))
    return { Icon: FileVideo, color: "text-rose-500" };
  if (is("md", "markdown", "txt", "log")) return { Icon: FileText, color: "text-foreground/60" };
  return { Icon: FileIcon, color: "text-muted-foreground" };
}

/// ノード種別アイコン。フォルダは OneDrive 風の黄色、ファイルは種別アイコン＋慣習色。
export function NodeIcon({
  kind,
  name = "",
  contentType,
  className,
}: {
  kind: string;
  name?: string;
  contentType?: string | null;
  className?: string;
}) {
  if (kind === "folder") {
    // 塗りつぶしの黄色フォルダ（OneDrive 風）。
    return <Folder className={cn("size-6 fill-amber-400 text-amber-500", className)} aria-hidden />;
  }
  const { Icon, color } = fileIcon(name, contentType);
  return <Icon className={cn("size-6", color, className)} aria-hidden />;
}

/// 種類の短い呼び名（行の 2 行目・アクションシートのメタ情報）。開く画面の判定
/// （drive-browser の open）と同じ拡張子で、利用者が知っている言葉にする。
export function nodeKindLabel(node: Pick<NodeResponse, "kind" | "name" | "content_type">): string {
  if (node.kind === "folder") return "フォルダ";
  const ext = node.name.toLowerCase().split(".").pop() ?? "";
  const ct = (node.content_type ?? "").toLowerCase();
  if (ext === "md") return "ノート";
  if (ext === "slide") return "スライド";
  if (ext === "csv") return "CSV";
  if (ext === "pdf" || ct === "application/pdf") return "PDF";
  if (["doc", "docx", "odt", "rtf"].includes(ext)) return "ドキュメント";
  if (["xls", "xlsx", "ods"].includes(ext)) return "スプレッドシート";
  if (["ppt", "pptx", "odp"].includes(ext)) return "プレゼンテーション";
  if (["zip", "tar", "gz", "rar", "7z"].includes(ext)) return "圧縮ファイル";
  // ブラウザで表示できる形式は、プレビュー画面の判定（拡張子優先）と同じ呼び名にする。
  const kind = previewKind(node);
  if (kind === "image" || ct.startsWith("image/")) return "画像";
  if (kind === "video") return "動画";
  if (kind === "audio") return "音声";
  if (kind === "text") return textLanguage(node.name) === "text" ? "テキスト" : "コード";
  return "ファイル";
}

const timeFormatter = new Intl.DateTimeFormat("ja-JP", { hour: "2-digit", minute: "2-digit" });

/// 狭い幅向けの短い日時（今日=時刻、今年=月/日、それ以前=年/月/日）。
export function formatShortDate(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const now = new Date();
  if (d.toDateString() === now.toDateString()) return timeFormatter.format(d);
  if (d.getFullYear() === now.getFullYear()) return `${d.getMonth() + 1}/${d.getDate()}`;
  return `${d.getFullYear()}/${d.getMonth() + 1}/${d.getDate()}`;
}

/// 共有中か（shared フラグはバックエンドが後続で提供。未提供は false に倒す）。
export function isShared(node: NodeResponse): boolean {
  return Boolean((node as { shared?: boolean }).shared);
}

/// 行の 2 行目に出すメタ情報（種類・更新日・サイズ・共有中）を「・」区切りで並べる。
export function nodeMetaParts(node: NodeResponse): string[] {
  return [
    nodeKindLabel(node),
    `${formatShortDate(node.updated_at)} 更新`,
    node.kind === "folder" ? null : formatBytes(node.size_bytes),
    isShared(node) ? "共有中" : null,
  ].filter((p): p is string => Boolean(p));
}

/// パンくず（root→自身）。各要素クリックでそのフォルダへ遷移する（PC）。
export function Breadcrumbs({
  crumbs,
  onNavigate,
}: {
  crumbs: CrumbResponse[];
  /// `null` でルートへ。
  onNavigate: (id: string | null) => void;
}) {
  return (
    <nav aria-label="パンくず" className="flex min-w-0 items-center gap-1 text-sm">
      <button
        type="button"
        onClick={() => onNavigate(null)}
        className="flex shrink-0 items-center gap-1 rounded-md px-1.5 py-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
      >
        <Home className="size-4" aria-hidden />
        <span>ドライブ</span>
      </button>
      {crumbs.map((c, i) => {
        const last = i === crumbs.length - 1;
        return (
          <React.Fragment key={c.id}>
            <ChevronRight className="size-4 shrink-0 text-muted-foreground/60" aria-hidden />
            <button
              type="button"
              onClick={() => onNavigate(c.id)}
              aria-current={last ? "page" : undefined}
              className={cn(
                "truncate rounded-md px-1.5 py-1 transition-colors hover:bg-accent",
                last ? "font-medium text-foreground" : "text-muted-foreground hover:text-foreground",
              )}
            >
              {c.name}
            </button>
          </React.Fragment>
        );
      })}
    </nav>
  );
}

/// スマホ見出しの上に置く短いパンくず。深い階層を横に並べると読めない幅になるため、
/// 「親フォルダへ戻る」＋（祖父母より上があれば）… のドロップダウンにまとめる。
/// 現在のフォルダ名は見出し本体（MobilePageTitle の title）が出すのでここには含めない。
export function MobileBreadcrumbs({
  crumbs,
  onNavigate,
}: {
  crumbs: CrumbResponse[];
  onNavigate: (id: string | null) => void;
}) {
  if (crumbs.length === 0) return null;
  // crumbs は root→自身。親は末尾の 1 つ手前（無ければドライブ直下）。
  const parent = crumbs.length >= 2 ? crumbs[crumbs.length - 2] : null;
  // 親より上の祖先（ドライブ直下 → … → 祖父母）。
  const ancestors = crumbs.slice(0, Math.max(0, crumbs.length - 2));
  return (
    <nav aria-label="現在の場所" className="-ml-1.5 flex min-w-0 items-center gap-0.5">
      {ancestors.length > 0 ? (
        <>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <button
                type="button"
                aria-label="上の階層を選ぶ"
                className="relative flex h-7 shrink-0 items-center gap-0.5 rounded-md px-1.5 text-muted-foreground transition-colors hover:bg-accent pointer-coarse:after:absolute pointer-coarse:after:-inset-y-2 pointer-coarse:after:inset-x-0 pointer-coarse:after:content-['']"
              >
                …
                <ChevronDown className="size-3.5" aria-hidden />
              </button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="start" className="max-w-[min(20rem,calc(100vw-2rem))]">
              <DropdownMenuItem onSelect={() => onNavigate(null)}>
                <Home aria-hidden />
                ドライブ
              </DropdownMenuItem>
              {ancestors.map((c) => (
                <DropdownMenuItem key={c.id} onSelect={() => onNavigate(c.id)}>
                  <Folder className="fill-amber-400 text-amber-500" aria-hidden />
                  <span className="truncate">{c.name}</span>
                </DropdownMenuItem>
              ))}
            </DropdownMenuContent>
          </DropdownMenu>
          <ChevronRight className="size-3.5 shrink-0 text-muted-foreground/60" aria-hidden />
        </>
      ) : null}
      <button
        type="button"
        onClick={() => onNavigate(parent ? parent.id : null)}
        className="relative flex h-7 min-w-0 items-center gap-1 rounded-md px-1.5 text-muted-foreground transition-colors hover:bg-accent pointer-coarse:after:absolute pointer-coarse:after:-inset-y-2 pointer-coarse:after:inset-x-0 pointer-coarse:after:content-['']"
      >
        {parent ? null : <Home className="size-3.5 shrink-0" aria-hidden />}
        <span className="truncate">{parent ? parent.name : "ドライブ"}</span>
      </button>
    </nav>
  );
}

/// クリックでソートする列見出し（OneDrive 風）。非アクティブ列はホバーで ⌄ を見せ、
/// アクティブ列は太字＋方向矢印（↑/↓）で現在の並び順を示す。
function SortLabel({
  label,
  field,
  sort,
  desc,
  onSort,
  className,
}: {
  label: string;
  field: SortField;
  sort: SortField;
  desc: boolean;
  onSort: (field: SortField) => void;
  className?: string;
}) {
  const active = sort === field;
  return (
    <button
      type="button"
      onClick={() => onSort(field)}
      className={cn(
        "group/sort flex items-center gap-1 rounded-md py-0.5 transition-colors hover:text-foreground",
        active ? "text-foreground" : "text-muted-foreground",
        className,
      )}
    >
      <span className={cn("truncate", active && "font-semibold")}>{label}</span>
      {active ? (
        desc ? (
          <ArrowDown className="size-3.5 shrink-0" aria-hidden />
        ) : (
          <ArrowUp className="size-3.5 shrink-0" aria-hidden />
        )
      ) : (
        // マウスではホバーで ⌄ を見せる。タッチはホバーが無いので常に淡く出す。
        <ChevronDown
          className="size-3 shrink-0 opacity-40 transition-opacity pointer-fine:opacity-0 pointer-fine:group-hover/sort:opacity-50"
          aria-hidden
        />
      )}
    </button>
  );
}

/// 一覧の列見出し行（OneDrive 風）。名前/更新日時/サイズはクリックでソート、
/// 更新者/共有は表示のみ（サーバ側ソート未対応）。列の出し入れは LIST_GRID と揃える。
/// 名前列しか無い狭い幅（`@xl` 未満）では出さない: 列見出しが「名前」1 つだけになり、
/// メタ情報は各行の 2 行目に出る。並べ替えはツールバー（スマホは見出し横）から行う。
export function ListHeader({
  sort,
  desc,
  onSort,
}: {
  sort: SortField;
  desc: boolean;
  onSort: (field: SortField) => void;
}) {
  return (
    <div
      className={cn(
        LIST_GRID,
        "shiki-dash-bottom hidden px-3 pb-2 text-[13px] font-medium text-muted-foreground @xl:grid",
      )}
    >
      <SortLabel label="名前" field="name" sort={sort} desc={desc} onSort={onSort} />
      <SortLabel
        label="更新日時"
        field="updated"
        sort={sort}
        desc={desc}
        onSort={onSort}
        className="hidden @xl:flex"
      />
      <span className="hidden truncate @4xl:block">更新者</span>
      <SortLabel
        label="サイズ"
        field="size"
        sort={sort}
        desc={desc}
        onSort={onSort}
        className="hidden @xl:flex"
      />
      <span className="hidden truncate @4xl:block">共有</span>
      <span aria-hidden />
    </div>
  );
}

/// 一覧フッタのローディング行（無限スクロールの sentinel と併用）。
export function LoadingRow({ label = "読み込み中…" }: { label?: string }) {
  return (
    <div className="flex items-center justify-center gap-2 py-4 text-sm text-muted-foreground">
      <Loader2 className="size-4 animate-spin" aria-hidden />
      <span>{label}</span>
    </div>
  );
}
