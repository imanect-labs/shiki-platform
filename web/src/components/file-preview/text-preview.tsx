"use client";

/// テキスト・コードのプレビューと編集（#784・#827）。
///
/// 中身を取り、行番号付きで出す。コードは shiki で色を付ける（ライト/ダークの 2 テーマを一度に出し、
/// `.dark` で切り替える）。大きいファイルは色を付けずに出し、上限を超えるものは開かない。
/// ソースは必ず文字列として描く（HTML や SVG のソースをアプリの画面で実行しない）。
/// HTML だけは例外として、既定で描画した姿を見せ、「ソース」に切り替えられる。描画はアプリの画面ではなく
/// sandbox の iframe の中で、スクリプトを動かさずに行う（`HtmlFrame`・`lib/html-preview`）。
///
/// 編集者には「編集」を出し、押すと同じ場所で CodeMirror に切り替える（エディタは押したときに読み込む）。
/// 保存は明示保存（⌘S・保存ボタン）で新しい版を作る。開いた版を添えて保存し（サーバが版を比べる）、
/// 開いた後に誰かが保存していたら 409 になるので確かめる。改行コードは開いたときのまま保つ。
///
/// 文字コードはサーバー（`GET /files/{id}/text`・textenc）が決める。画面で判定を持たない（BUG-141・D-119）。
/// 保存（`PUT /files/{id}/text`）は、元の文字コードと BOM のままサーバーが書き戻す（Shift_JIS は Shift_JIS のまま）。
/// 元の文字コードに無い文字は、サーバーの文言で断る（編集した内容は残す）。
import * as React from "react";
import { Check, Code, Copy, Eye, MonitorSmartphone, Pencil, Save, WrapText } from "lucide-react";

import { EditorLoading, EditorStatusBadge } from "@/components/shell/editor-loading";
import { ChromeIconButton, ChromePrimaryButton } from "@/components/shell/page-header-context";
import { SegmentedControl, type SegmentedOption } from "@/components/ui/segmented-control";
import { toast } from "@/components/ui/use-toast";
import { formatBytes } from "@/lib/format";
import {
  HIGHLIGHT_MAX_BYTES,
  TEXT_MAX_BYTES,
  TEXT_MAX_LINES,
  isRenderableHtml,
  languageColor,
  languageLabel,
  textLanguage,
} from "@/lib/file-preview";
import { prepareHtmlPreview, type PreparedHtmlPreview } from "@/lib/html-preview";
import { getCollabAccess } from "@/lib/notes-api";
import { getTextFile, saveTextFile, StorageApiError, type NodeResponse, type TextFileResponse } from "@/lib/storage";
import { cn } from "@/lib/utils";

import { useFileChrome } from "./file-chrome";
import { HtmlFrame } from "./html-frame";
import { DiscardDialog, ConflictDialog } from "./text-edit-dialogs";
import type { TextEditorHandle } from "./text-editor";
import { useUnsavedGuard } from "./use-unsaved-guard";

const TextEditor = React.lazy(() => import("./text-editor").then((m) => ({ default: m.TextEditor })));

export type PreviewFailure = { title?: string; description?: string };

const HTML_VIEW_OPTIONS: SegmentedOption[] = [
  { value: "rendered", label: "プレビュー", icon: MonitorSmartphone, testId: "html-view-rendered" },
  { value: "source", label: "ソース", icon: Code, testId: "html-view-source" },
];

const tooLarge: PreviewFailure = {
  title: "大きすぎるためプレビューできません",
  description: `${TEXT_MAX_BYTES / 1024 / 1024}MB を超えるテキストは、ダウンロードして開いてください。`,
};

/// 開いたファイルの中身と、表示に使う情報（文字コード・改行コード・中身の版）。
/// `encoding` はサーバーが決めた文字コード（`UTF-8`・`Shift_JIS` など）。`null` は決められなかったもの
/// （見るだけ。保存すると中身が壊れるので編集させない）。保存はサーバーが元の文字コードと BOM で書き戻す。
type Content = {
  text: string;
  encoding: string | null;
  eol: "\n" | "\r\n";
  lines: number;
  version: number;
};
type Rendered = { html: string; highlighted: boolean };

/// 書けない文字があると保存できない文字コード（Unicode でないもの）。編集中に注記を出す。
function narrowEncoding(encoding: string | null): boolean {
  return encoding !== null && !/^UTF-/i.test(encoding);
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
/// テキストでない・大きすぎる中身は、サーバーが断った文言をそのまま出す。
async function fetchContent(node: NodeResponse): Promise<Content | PreviewFailure> {
  let decoded: TextFileResponse;
  try {
    decoded = await getTextFile(node.id);
  } catch (e) {
    if (e instanceof StorageApiError && e.status === 400) return { description: e.message };
    throw e;
  }
  const lines = countLines(decoded.text);
  if (lines > TEXT_MAX_LINES) {
    return {
      title: "行数が多すぎるためプレビューできません",
      description: `${TEXT_MAX_LINES.toLocaleString("ja-JP")} 行を超えるテキストは、ダウンロードして開いてください。`,
    };
  }
  // 1 つでも CRLF があれば CRLF のファイルとして扱う（保存で LF に変えない。LF の行が混ざっていれば CRLF に揃う）。
  const eol = decoded.text.includes("\r\n") ? "\r\n" : "\n";
  return { text: decoded.text, encoding: decoded.encoding ?? null, eol, lines, version: decoded.version };
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
  // HTML は描画した姿を既定にする（ソースはトグルで見る）。
  const renderable = isRenderableHtml(node);
  const [showSource, setShowSource] = React.useState(false);
  // 外部の画像を読み込むと閲覧者が選んだ文書（ソースや編集に切り替えて戻っても保つ）。
  const [allowedDoc, setAllowedDoc] = React.useState<PreparedHtmlPreview | null>(null);
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
        if ("text" in c) {
          setContent(c);
          setBaseVersion(c.version);
        } else failRef.current(c);
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

  // 描画用の文書は中身ごとに 1 回だけ解析・掃除する（ソースや編集から戻るたびにやり直さない）。
  // 想定外の入力で解析に失敗したら、描画せずにソースで見せる（画面ごと落とさない）。
  const text = content?.text;
  const prepared = React.useMemo(() => {
    if (!renderable || text == null) return null;
    try {
      return prepareHtmlPreview(text);
    } catch {
      return null;
    }
  }, [renderable, text]);
  // 外部の画像を読み込む同意は、同意したときの中身にだけ効かせる（読み直した新しい版の参照には効かせない）。
  const allowExternal = prepared != null && allowedDoc === prepared;
  const onAllowExternal = React.useCallback(() => setAllowedDoc(prepared), [prepared]);

  // 閲覧の表示。まず色なしで出し、色付けが終わったら差し替える（大きめのファイルでも待たせない）。
  // HTML を描画して見せている間はソースの表示を作らない（shiki の読み込み・色付けを走らせない）。
  const sourceShown = prepared == null || showSource;
  React.useEffect(() => {
    if (text == null) return;
    if (!sourceShown) {
      setRendered(null);
      return;
    }
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
  }, [text, lang, sourceShown]);

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
        setBaseVersion(c.version);
        // 新しい版の文字コードを決められなければ、保存で中身が壊れるので閲覧のみに留める。
        if (c.encoding === null) {
          toast({
            variant: "destructive",
            title: "このファイルは編集できません",
            description: "新しい版の文字コードが分からないため、見ることだけできます。",
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
        // 文字コードと BOM はサーバーが元のまま書き戻す（Shift_JIS は Shift_JIS のまま・D-119）。
        const saved = await saveTextFile(node.id, { text: snap.text, base_version: opts?.base ?? baseVersion });
        if (!mounted.current) return;
        setBaseVersion(saved.version);
        setSize(saved.size_bytes);
        setContent({ ...content, text: snap.text, lines: countLines(snap.text), version: saved.version });
        editor.markSaved(snap.doc);
        toast({ title: "保存しました", description: `版 ${saved.version} として保存しました。` });
      } catch (e) {
        // 開いた後に他の保存が入っていた（サーバが版を比べて断った）。
        if (e instanceof StorageApiError && e.status === 409) {
          if (mounted.current) setConflict(true);
          return;
        }
        // 元の文字コードに無い文字など、サーバーが理由を言って断った（編集した内容はエディタに残っている）。
        if (e instanceof StorageApiError && e.status === 400) {
          toast({ variant: "destructive", title: "保存できませんでした", description: e.message });
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
      const c = await fetchContent(node);
      if (!mounted.current) return;
      if (!("text" in c)) {
        failRef.current(c);
        return;
      }
      setContent(c);
      setBaseVersion(c.version);
    } catch {
      if (mounted.current) failRef.current({ title: "ファイルを読み込めませんでした", description: "時間をおいてもう一度開くか、ダウンロードしてください。" });
    }
  }, [node]);

  const editing = mode === "edit";
  // 編集はソースで行う。編集を終えると、切り替えておいた表示（描画かソース）に戻る。
  const rendering = prepared != null && !showSource && !editing;
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
      ) : canEdit && content && content.encoding !== null ? (
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

  if (!content) return <EditorLoading kind="doc" message="ファイルを開いています…" />;
  const gutter = String(content.lines).length;
  const meta = [
    `${content.lines.toLocaleString("ja-JP")} 行`,
    formatBytes(size),
    content.encoding === null ? "文字コード不明" : content.encoding === "UTF-8" ? null : content.encoding,
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
        {prepared != null && !editing ? (
          <SegmentedControl
            aria-label="表示の切り替え"
            size="sm"
            className="mr-1"
            value={showSource ? "source" : "rendered"}
            onValueChange={(v) => setShowSource(v === "source")}
            options={HTML_VIEW_OPTIONS}
          />
        ) : null}
        {!rendering ? (
          <HeaderButton label="折り返して表示" pressed={wrap} onClick={() => setWrap((w) => !w)} testId="text-wrap-toggle">
            <WrapText aria-hidden />
            <span className="max-sm:hidden">折り返し</span>
          </HeaderButton>
        ) : null}
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
          {narrowEncoding(content.encoding) ? (
            <p className="shrink-0 border-b border-border/70 bg-season-autumn/10 px-4 py-2 text-xs text-foreground/80" data-testid="text-encoding-note">
              {content.encoding} のファイルです。保存しても {content.encoding} のままで、{content.encoding}{" "}
              にない文字（絵文字など）は保存できません。
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

  if (rendering) {
    // 描画した HTML は iframe の中でスクロールする。カードは画面の高さいっぱいに置く。
    return (
      <div className="flex h-full min-h-0 flex-col bg-muted/40 md:px-6 md:py-5" data-testid="text-preview" data-mode="rendered">
        <div className="mx-auto flex min-h-0 w-full max-w-6xl flex-1 flex-col bg-card md:overflow-hidden md:rounded-xl md:border md:border-border/70 md:shadow-sm">
          {header}
          {prepared ? (
            <HtmlFrame prepared={prepared} allowExternal={allowExternal} onAllowExternal={onAllowExternal} title={node.name} />
          ) : null}
        </div>
        {dialogs}
      </div>
    );
  }

  // 編集はエディタが、描画は iframe が出すので、ソースの表示（rendered）を待つのはここだけ。
  if (!rendered) return <EditorLoading kind="doc" message="ファイルを開いています…" />;
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
