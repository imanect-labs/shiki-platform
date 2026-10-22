"use client";

/// テキスト・コードのプレビューと編集（#784・#827）。
///
/// 中身を取り、行番号付きで出す。コードは shiki で色を付ける（ライト/ダークの 2 テーマを一度に出し、
/// `.dark` で切り替える）。大きいファイルは色を付けずに出し、上限を超えるものは開かない。
/// 中身は必ず文字列として描く（HTML や SVG のソースを実行しない）。
///
/// 編集者には「編集」を出し、押すと同じ場所で CodeMirror に切り替える（エディタは押したときに読み込む）。
/// 保存は明示保存（⌘S・保存ボタン）で新しい版を作る。開いた版を添えて保存し（サーバが版を比べる）、
/// 開いた後に誰かが保存していたら 409 になるので確かめる。改行コードと BOM は開いたときのまま保つ
/// （Shift_JIS は UTF-8 で保存する）。
import * as React from "react";
import { Check, Copy, Eye, Pencil, Save, WrapText } from "lucide-react";

import { EditorLoading, EditorStatusBadge } from "@/components/shell/editor-loading";
import { ChromeIconButton, ChromePrimaryButton } from "@/components/shell/page-header-context";
import { toast } from "@/components/ui/use-toast";
import { formatBytes } from "@/lib/format";
import {
  HIGHLIGHT_MAX_BYTES,
  TEXT_MAX_BYTES,
  TEXT_MAX_LINES,
  languageColor,
  languageLabel,
  looksBinary,
  textLanguage,
} from "@/lib/file-preview";
import { getCollabAccess } from "@/lib/notes-api";
import { downloadUrl, StorageApiError, uploadFile, type NodeResponse } from "@/lib/storage";
import { cn } from "@/lib/utils";

import { useFileChrome } from "./file-chrome";
import { DiscardDialog, ConflictDialog } from "./text-edit-dialogs";
import type { TextEditorHandle } from "./text-editor";
import { useUnsavedGuard } from "./use-unsaved-guard";

const TextEditor = React.lazy(() => import("./text-editor").then((m) => ({ default: m.TextEditor })));

export type PreviewFailure = { title?: string; description?: string };

const tooLarge: PreviewFailure = {
  title: "大きすぎるためプレビューできません",
  description: `${TEXT_MAX_BYTES / 1024 / 1024}MB を超えるテキストは、ダウンロードして開いてください。`,
};

/// 開いたファイルの中身と、保存のときに元へ戻すための情報（文字コード・BOM・改行コード）。
/// `unknown` は UTF-8 でも Shift_JIS でもないもの（見るだけ。保存すると中身が壊れるので編集させない）。
type Content = {
  text: string;
  encoding: "utf-8" | "shift_jis" | "unknown";
  bom: boolean;
  eol: "\n" | "\r\n";
  lines: number;
};
type Rendered = { html: string; highlighted: boolean };

/// 文字コードを決めて文字列にする。UTF-8 として壊れていれば Shift_JIS を試す（日本の古い CSV・ログに多い）。
/// どちらでも読めなければ、置換文字入りの UTF-8 として見せるだけにする。
function decode(bytes: Uint8Array): Pick<Content, "text" | "encoding" | "bom"> {
  const bom = bytes.length >= 3 && bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf;
  try {
    // BOM は取り除かれる（保存のときに付け直す）。
    return { text: new TextDecoder("utf-8", { fatal: true }).decode(bytes), encoding: "utf-8", bom };
  } catch {
    try {
      return { text: new TextDecoder("shift_jis", { fatal: true }).decode(bytes), encoding: "shift_jis", bom: false };
    } catch {
      return { text: new TextDecoder("utf-8").decode(bytes), encoding: "unknown", bom: false };
    }
  }
}

/// 保存する中身（UTF-8・開いたときに BOM があれば付ける）。
function encode(text: string, bom: boolean): Uint8Array<ArrayBuffer> {
  const body = new TextEncoder().encode(text);
  const prefix = bom ? 3 : 0;
  const out = new Uint8Array(new ArrayBuffer(body.length + prefix));
  if (bom) out.set([0xef, 0xbb, 0xbf]);
  out.set(body, prefix);
  return out;
}

/// 保存するときの content-type。文字コードの指定があれば UTF-8 に直す（Shift_JIS も UTF-8 で保存するため）。
function saveContentType(contentType: string | null | undefined): string {
  const ct = (contentType ?? "").trim();
  if (!ct || ct.toLowerCase().startsWith("application/octet-stream")) return "text/plain; charset=utf-8";
  return /charset=/i.test(ct) ? ct.replace(/charset=[^;]*/i, "charset=utf-8") : ct;
}

function countLines(text: string): number {
  if (text.length === 0) return 1;
  let n = 1;
  for (let i = text.indexOf("\n"); i !== -1; i = text.indexOf("\n", i + 1)) n++;
  return n;
}

function escapeHtml(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

/// 色なしの行（shiki の出力と同じ `<span class="line">` の並びにして、見た目と行番号を揃える）。
function plainHtml(text: string): string {
  return `<pre class="shiki"><code>${text
    .split(/\r?\n/)
    .map((l) => `<span class="line">${escapeHtml(l)}</span>`)
    .join("")}</code></pre>`;
}

async function highlight(text: string, lang: string): Promise<string> {
  const { codeToHtml } = await import("shiki");
  const html = await codeToHtml(text, {
    lang,
    themes: { light: "github-light", dark: "github-dark" },
    defaultColor: "light",
  });
  // 行は display:block で並べるので、行の間の改行文字は空行になる。取り除く。
  return html.replace(/<\/span>\n<span class="line"/g, '</span><span class="line"');
}

/// 中身を取りに行って、表示・編集できるか確かめる。できなければ理由を返す。
async function fetchContent(node: NodeResponse): Promise<Content | PreviewFailure> {
  const { url } = await downloadUrl(node.id);
  const res = await fetch(url);
  if (!res.ok) throw new Error(`取得に失敗しました（${res.status}）`);
  const bytes = new Uint8Array(await res.arrayBuffer());
  // メタ情報のサイズが無い・古い場合に備えて、実際に取れた大きさでも確かめる。
  if (bytes.length > TEXT_MAX_BYTES) return tooLarge;
  if (looksBinary(bytes)) {
    return { description: "テキストではないため表示できません。ダウンロードして、お使いのアプリで開いてください。" };
  }
  const decoded = decode(bytes);
  const lines = countLines(decoded.text);
  if (lines > TEXT_MAX_LINES) {
    return {
      title: "行数が多すぎるためプレビューできません",
      description: `${TEXT_MAX_LINES.toLocaleString("ja-JP")} 行を超えるテキストは、ダウンロードして開いてください。`,
    };
  }
  // 1 つでも CRLF があれば CRLF のファイルとして扱う（保存で LF に変えない。LF の行が混ざっていれば CRLF に揃う）。
  const eol = decoded.text.includes("\r\n") ? "\r\n" : "\n";
  return { ...decoded, eol, lines };
}

export function TextPreview({
  node,
  onDownload,
  onUnsupported,
}: {
  node: NodeResponse;
  onDownload: () => void;
  onUnsupported: (why: PreviewFailure) => void;
}) {
  const lang = textLanguage(node.name);
  const [content, setContent] = React.useState<Content | null>(null);
  const [rendered, setRendered] = React.useState<Rendered | null>(null);
  // 散文（言語なし）とスマホでは折り返し、PC のコードは横にスクロールさせる。
  const [wrap, setWrap] = React.useState(
    () => lang === "text" || (typeof window !== "undefined" && window.matchMedia("(max-width: 767px)").matches),
  );
  const [copied, setCopied] = React.useState(false);
  const failRef = React.useRef(onUnsupported);
  failRef.current = onUnsupported;

  // 編集。`baseVersion` は今の中身の元になった版（保存の直前にこれと比べる）。
  // 中身はメタ情報の後に取っているので、node.version は中身の版より新しくならない（衝突の見逃しは無い）。
  const [mode, setMode] = React.useState<"view" | "edit">("view");
  const [canEdit, setCanEdit] = React.useState<boolean | null>(null);
  const [baseVersion, setBaseVersion] = React.useState(node.version);
  const [size, setSize] = React.useState(node.size_bytes);
  const [dirty, setDirty] = React.useState(false);
  const [saving, setSaving] = React.useState(false);
  const [opening, setOpening] = React.useState(false);
  const [conflict, setConflict] = React.useState(false);
  const [discardThen, setDiscardThen] = React.useState<(() => void) | null>(null);
  const editorRef = React.useRef<TextEditorHandle>(null);
  const dirtyRef = React.useRef(dirty);
  dirtyRef.current = dirty;
  const mounted = React.useRef(true);
  React.useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  React.useEffect(() => {
    if ((node.size_bytes ?? 0) > TEXT_MAX_BYTES) {
      failRef.current(tooLarge);
      return;
    }
    let active = true;
    fetchContent(node)
      .then((c) => {
        if (!active) return;
        if ("text" in c) setContent(c);
        else failRef.current(c);
      })
      .catch(() => {
        if (active) failRef.current({ title: "ファイルを読み込めませんでした", description: "時間をおいてもう一度開くか、ダウンロードしてください。" });
      });
    return () => {
      active = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- node は id で入れ替わる（key=node.id）
  }, [node.id]);

  // 編集できるか（editor か）。分からなければ編集を出さない（書込の強制はサーバ側）。
  React.useEffect(() => {
    let active = true;
    getCollabAccess(node.id)
      .then((a) => active && setCanEdit(a?.mode === "editor"))
      .catch(() => active && setCanEdit(false));
    return () => {
      active = false;
    };
  }, [node.id]);

  // 閲覧の表示。まず色なしで出し、色付けが終わったら差し替える（大きめのファイルでも待たせない）。
  const text = content?.text;
  React.useEffect(() => {
    if (text == null) return;
    setRendered({ html: plainHtml(text), highlighted: false });
    if (lang === "text" || text.length > HIGHLIGHT_MAX_BYTES) return;
    let active = true;
    highlight(text.replace(/\r\n/g, "\n"), lang)
      .then((html) => active && setRendered({ html, highlighted: true }))
      // 未知の言語・shiki の失敗は色なしのまま。
      .catch(() => undefined);
    return () => {
      active = false;
    };
  }, [text, lang]);

  const copyAll = React.useCallback(async () => {
    if (!content) return;
    try {
      await navigator.clipboard.writeText(content.text);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      toast({ variant: "destructive", title: "コピーできませんでした" });
    }
  }, [content]);

  // 編集を始める。開いた後に新しい版が保存されていれば、最新の中身を読み直してから編集する。
  const startEdit = React.useCallback(async () => {
    if (opening) return;
    setOpening(true);
    try {
      const access = await getCollabAccess(node.id);
      if (!mounted.current) return;
      if (access?.mode !== "editor") {
        setCanEdit(false);
        toast({ variant: "destructive", title: "このファイルを編集する権限がありません" });
        return;
      }
      if (access.version !== baseVersion) {
        const c = await fetchContent(node);
        if (!mounted.current) return;
        if (!("text" in c)) {
          failRef.current(c);
          return;
        }
        setContent(c);
        setBaseVersion(access.version);
        // 新しい版が UTF-8・Shift_JIS 以外なら、保存で中身が壊れるので閲覧のみに留める。
        if (c.encoding === "unknown") {
          toast({
            variant: "destructive",
            title: "このファイルは編集できません",
            description: "新しい版の文字コードを判別できないため、閲覧のみになります。",
          });
          return;
        }
        toast({ title: "最新の内容を読み込みました", description: "開いた後に新しい版が保存されていました。" });
      }
      setMode("edit");
    } catch {
      toast({ variant: "destructive", title: "編集を始められませんでした", description: "時間をおいてもう一度お試しください。" });
    } finally {
      if (mounted.current) setOpening(false);
    }
  }, [opening, node, baseVersion]);

  /// `base` を渡すと、その版の上に保存する（衝突の確認で「このまま保存」を選んだとき）。
  const save = React.useCallback(
    async (opts?: { base?: number }) => {
      const editor = editorRef.current;
      // 変更が無ければ版を増やさない（⌘S の押しっぱなし・連打）。
      if (!editor || !content || saving || !dirtyRef.current) return;
      const snap = editor.snapshot();
      setSaving(true);
      try {
        const file = new File([encode(snap.text, content.bom)], node.name, { type: saveContentType(node.content_type) });
        const saved = await uploadFile({ file, targetNodeId: node.id, baseVersion: opts?.base ?? baseVersion });
        if (!mounted.current) return;
        setBaseVersion(saved.version);
        setSize(saved.size_bytes);
        setContent({ ...content, text: snap.text, encoding: "utf-8", lines: countLines(snap.text) });
        editor.markSaved(snap.doc);
        toast({ title: "保存しました", description: `版 ${saved.version} として保存しました。` });
      } catch (e) {
        // 開いた後に他の保存が入っていた（サーバが版を比べて断った）。
        if (e instanceof StorageApiError && e.status === 409) {
          if (mounted.current) setConflict(true);
          return;
        }
        const denied = e instanceof StorageApiError && e.status === 403;
        toast({
          variant: "destructive",
          title: denied ? "このファイルを編集する権限がありません" : "保存できませんでした",
          description: denied ? undefined : "時間をおいてもう一度お試しください。編集した内容はそのまま残っています。",
        });
      } finally {
        if (mounted.current) setSaving(false);
      }
    },
    [content, saving, node, baseVersion],
  );
  // 衝突の確認で「このまま保存」: 今の版を取り直し、その上に保存する（その間にまた保存が入れば再び確認）。
  const overwrite = React.useCallback(async () => {
    setConflict(false);
    try {
      const latest = await getCollabAccess(node.id);
      if (!latest) throw new Error("access not found");
      await save({ base: latest.version });
    } catch {
      toast({ variant: "destructive", title: "保存できませんでした", description: "時間をおいてもう一度お試しください。編集した内容はそのまま残っています。" });
    }
  }, [node.id, save]);
  const saveRef = React.useRef(save);
  saveRef.current = save;
  const onEditorSave = React.useCallback(() => void saveRef.current(), []);

  // 編集中は、エディタの外にフォーカスがあっても ⌘S で保存する（ブラウザの「ページを保存」を出さない）。
  React.useEffect(() => {
    if (mode !== "edit") return;
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || !(e.metaKey || e.ctrlKey) || e.key.toLowerCase() !== "s") return;
      e.preventDefault();
      void saveRef.current();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [mode]);

  // 未保存の編集があれば、離れる前に確かめる。
  const guard = React.useCallback(
    (go: () => void) => {
      // 保存の途中で離れると、終わったかどうか分からなくなる。終わるまで待ってもらう。
      if (saving) toast({ title: "保存しています", description: "保存が終わってからもう一度お試しください。" });
      else if (mode === "edit" && dirty) setDiscardThen(() => go);
      else go();
    },
    [mode, dirty, saving],
  );
  useUnsavedGuard({ editing: mode === "edit", dirty: mode === "edit" && (dirty || saving), confirm: guard });

  const stopEdit = React.useCallback(() => {
    guard(() => {
      setMode("view");
      setDirty(false);
    });
  }, [guard]);

  // 他の人の保存の後に開き直す（編集は破棄する）。読み込み中はエディタを閉じて、打った字が消えないようにする。
  const reloadLatest = React.useCallback(async () => {
    setConflict(false);
    setMode("view");
    setDirty(false);
    setContent(null);
    try {
      const access = await getCollabAccess(node.id);
      const c = await fetchContent(node);
      if (!mounted.current) return;
      if (!("text" in c)) {
        failRef.current(c);
        return;
      }
      setContent(c);
      if (access) setBaseVersion(access.version);
    } catch {
      if (mounted.current) failRef.current({ title: "ファイルを読み込めませんでした", description: "時間をおいてもう一度開くか、ダウンロードしてください。" });
    }
  }, [node]);

  const editing = mode === "edit";
  useFileChrome(
    node,
    onDownload,
    {
      subtitle: editing ? (
        dirty ? (
          <EditorStatusBadge tone="draft">未保存の編集あり</EditorStatusBadge>
        ) : (
          <EditorStatusBadge tone="info">編集中</EditorStatusBadge>
        )
      ) : canEdit === false ? (
        <EditorStatusBadge icon={Eye}>閲覧のみ</EditorStatusBadge>
      ) : undefined,
      primary: editing ? (
        <ChromePrimaryButton
          onClick={() => void save()}
          disabled={!dirty || saving}
          aria-busy={saving || undefined}
          title="保存（⌘S）"
          data-testid="text-save"
        >
          <Save aria-hidden />
          {saving ? "保存中…" : "保存"}
        </ChromePrimaryButton>
      ) : canEdit && content && content.encoding !== "unknown" ? (
        <ChromePrimaryButton onClick={() => void startEdit()} disabled={opening} data-testid="text-edit">
          <Pencil aria-hidden />
          編集
        </ChromePrimaryButton>
      ) : undefined,
      actions: editing ? (
        <ChromeIconButton label="編集を終える" onClick={stopEdit} data-testid="text-stop-edit">
          <Eye aria-hidden />
        </ChromeIconButton>
      ) : undefined,
      guardBack: guard,
    },
    [editing, dirty, saving, canEdit, content, opening, save, startEdit, stopEdit, guard],
  );

  if (!content || !rendered) return <EditorLoading kind="doc" message="ファイルを開いています…" />;
  const gutter = String(content.lines).length;
  const meta = [
    `${content.lines.toLocaleString("ja-JP")} 行`,
    formatBytes(size),
    content.encoding === "shift_jis" ? "Shift_JIS" : content.encoding === "unknown" ? "文字コード不明" : null,
    content.eol === "\r\n" ? "CRLF" : null,
  ].filter(Boolean);

  const header = (
    <div className="sticky top-0 z-[1] flex shrink-0 items-center gap-2 border-b border-border/70 bg-card/95 py-1.5 pl-4 pr-1.5 backdrop-blur max-md:pl-3">
      <span className="inline-flex items-center gap-1.5 rounded-md bg-muted px-2 py-0.5 text-xs font-medium text-foreground/80">
        <span className="size-2 rounded-full" style={{ background: languageColor(lang) }} aria-hidden />
        {languageLabel(lang, node.name)}
      </span>
      <span className="min-w-0 truncate text-xs text-muted-foreground tabular-nums">{meta.join("・")}</span>
      {/* スマホはバーに状態を出さないので、ここに出す。 */}
      {editing ? (
        <span className="md:hidden">
          {dirty ? <EditorStatusBadge tone="draft">未保存</EditorStatusBadge> : <EditorStatusBadge tone="info">編集中</EditorStatusBadge>}
        </span>
      ) : canEdit === false ? (
        <span className="md:hidden">
          <EditorStatusBadge icon={Eye}>閲覧のみ</EditorStatusBadge>
        </span>
      ) : null}
      <span className="ml-auto flex shrink-0 items-center gap-0.5">
        <HeaderButton label="折り返して表示" pressed={wrap} onClick={() => setWrap((w) => !w)} testId="text-wrap-toggle">
          <WrapText aria-hidden />
          <span className="max-sm:hidden">折り返し</span>
        </HeaderButton>
        {!editing ? (
          <HeaderButton label={copied ? "コピーしました" : "すべてコピー"} onClick={() => void copyAll()} testId="text-copy">
            {copied ? <Check aria-hidden /> : <Copy aria-hidden />}
            <span className="max-sm:hidden">{copied ? "コピーしました" : "コピー"}</span>
          </HeaderButton>
        ) : null}
      </span>
    </div>
  );

  const dialogs = (
    <>
      <ConflictDialog
        open={conflict}
        onClose={() => setConflict(false)}
        onOverwrite={() => void overwrite()}
        onReload={() => void reloadLatest()}
      />
      <DiscardDialog
        open={discardThen != null}
        onClose={() => setDiscardThen(null)}
        onDiscard={() => {
          const go = discardThen;
          setDiscardThen(null);
          setDirty(false);
          go?.();
        }}
      />
    </>
  );

  if (editing) {
    // エディタが自分でスクロールする（検索パネルを上に残すため）。カードは画面の高さいっぱいに置く。
    return (
      <div className="flex h-full min-h-0 flex-col bg-muted/40 md:px-6 md:py-5" data-testid="text-preview" data-mode="edit">
        <div className="mx-auto flex min-h-0 w-full max-w-6xl flex-1 flex-col bg-card md:overflow-hidden md:rounded-xl md:border md:border-border/70 md:shadow-sm">
          {header}
          {content.encoding === "shift_jis" ? (
            <p className="shrink-0 border-b border-border/70 bg-season-autumn/10 px-4 py-2 text-xs text-foreground/80" data-testid="text-encoding-note">
              Shift_JIS のファイルです。保存すると文字コードは UTF-8 になります。
            </p>
          ) : null}
          <div className="min-h-0 flex-1 [&_.cm-editor]:h-full [&_.text-editor]:h-full">
            <React.Suspense fallback={<EditorLoading kind="doc" message="エディタを開いています…" />}>
              <TextEditor
                ref={editorRef}
                initial={content.text}
                fileName={node.name}
                wrap={wrap}
                eol={content.eol}
                onDirtyChange={setDirty}
                onSave={onEditorSave}
              />
            </React.Suspense>
          </div>
        </div>
        {dialogs}
      </div>
    );
  }

  return (
    <div className="h-full min-h-0 overflow-auto bg-muted/40 md:px-6 md:py-5" data-testid="text-preview" data-mode="view" tabIndex={0}>
      <div className="mx-auto flex min-h-full w-full max-w-6xl flex-col bg-card md:min-h-0 md:overflow-hidden md:rounded-xl md:border md:border-border/70 md:shadow-sm">
        {header}
        <div className="overflow-x-auto overscroll-x-contain">
          <div
            className={cn("file-code min-w-max py-2 font-mono text-[13px] leading-[1.65]", wrap && "file-code-wrap min-w-0")}
            style={{ ["--gutter" as string]: `${gutter}ch` }}
            data-highlighted={rendered.highlighted ? "" : undefined}
            // 中身は shiki か escapeHtml を通した文字列だけ（ファイルの HTML をそのまま差し込まない）。
            dangerouslySetInnerHTML={{ __html: rendered.html }}
          />
        </div>
      </div>
      {dialogs}
    </div>
  );
}

function HeaderButton({
  label,
  pressed,
  onClick,
  testId,
  children,
}: {
  label: string;
  pressed?: boolean;
  onClick: () => void;
  testId?: string;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      aria-pressed={pressed}
      title={label}
      onClick={onClick}
      data-testid={testId}
      className={cn(
        "inline-flex h-7 items-center gap-1.5 rounded-md px-2 text-xs font-medium outline-none transition-colors",
        "text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring",
        "pointer-coarse:h-10 pointer-coarse:px-3 [&_svg]:size-3.5",
        pressed && "bg-accent text-foreground",
      )}
    >
      {children}
    </button>
  );
}
