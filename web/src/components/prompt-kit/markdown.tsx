// prompt-kit (https://www.prompt-kit.com) 由来の Markdown/Response を本リポジトリ向けに実装。
// MIT License。react-markdown + remark-gfm で LLM 応答を描画する。コードブロックは CodeBlock（```ui は図・#530）、
// 装飾はすべてセマンティックトークンで行う（生色禁止）。
"use client";

import * as React from "react";
import dynamic from "next/dynamic";
import Link from "next/link";
import ReactMarkdown, { type Components, type ExtraProps } from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkCjkFriendly from "remark-cjk-friendly";
import { EyeOff } from "lucide-react";

import { cn } from "@/lib/utils";
import { citeNumberFromHref } from "@/lib/citation";
import { decideAnswerImage, type ImageProxy, type ImageSources } from "@/lib/answer-images";
import { decideAnswerLink, type LinkSources } from "@/lib/answer-links";
import { CitationMarker } from "@/components/chat/citations/citation-marker";
import { resolveKind } from "@imanect/yui";
import { isYuiLanguage } from "@imanect/yui/react";

import { CodeBlock } from "./code-block";
import { MathTex, mathKind, mayHaveMath, useMathPlugins } from "./markdown-math";
// 引用・箇条書きの中の図はボタンを出さない（重い図の描画部とは別に、軽いコンテキストだけ読む）。
import { YuiNestedContext } from "./yui-actions";
import { UnverifiedLink, UnverifiedLinkDialog, useUnverifiedOpen } from "./unverified-link";
import { DataGridHostContext } from "@/components/data/data-grid-host";

/// 画面の表（```data_grid）はミニアプリの画面でだけ使うので、グリッド本体は遅延読み込みする
/// （会話・ノートの Markdown の初回読み込みに glide-data-grid を載せない）。
const MiniAppDataGrid = React.lazy(() =>
  import("@/components/data/miniapp-data-grid").then((m) => ({ default: m.MiniAppDataGrid })),
);

/// ```ui の図は描画部（@imanect/yui の SVG レンダラー・書き出し）が重いので、図が現れたときだけ
/// 読み込む。読み込み中は図と同じくらいの高さの枠を置いてレイアウトが跳ねないようにする。
const UiFigure = dynamic(() => import("./ui-figure").then((m) => m.UiFigure), {
  ssr: false,
  loading: () => (
    <div className="my-3 h-48 w-full animate-pulse rounded-xl border border-border/60 bg-secondary/40" />
  ),
});

/// 出典の画像が無い（会話以外の本文・出典を持たない閲覧者）。外部の画像は 1 枚も読み込まない。
const NO_IMAGE_SOURCES: ImageSources = new Map();

/// リンクの中を描いているか（リンクの中の画像を読み込まないとき、リンクを入れ子にしない）。
const InLinkContext = React.createContext(false);

/// 長い URL・英数字の連なりで折り返す（はみ出すと会話全体が横にずれ、吹き出しの左端が切れる）。
const WRAP_ANYWHERE = "[overflow-wrap:anywhere]";

/// LLM は GFM 表のセル内改行に `<br>` を使うことがある（表セルは実改行を持てないため）。
/// react-markdown は raw HTML を通さず `<br>` が文字列で残り「安っぽく」見えるので、
/// テキストノード中の `<br>` / `<br/>` だけを mdast の break ノードに変換する。
/// 他の HTML は一切通さないため XSS の増分リスクは無い（安全な最小対処）。
function remarkBrTags() {
  const BR = /<br\s*\/?>/i;
  const ONLY_BR = /^<br\s*\/?>$/i;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const walk = (node: any) => {
    if (!node || !Array.isArray(node.children)) return;
    for (let i = 0; i < node.children.length; i++) {
      const child = node.children[i];
      // remark は `<br>` を inline HTML（type: "html"）として持つので break ノードへ差し替える。
      if (child.type === "html" && ONLY_BR.test((child.value ?? "").trim())) {
        node.children.splice(i, 1, { type: "break" });
        continue;
      }
      // 念のためテキスト中に紛れた `<br>` も分割して break を挟む。
      if (child.type === "text" && BR.test(child.value)) {
        const parts = child.value.split(BR);
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        const replacement: any[] = [];
        parts.forEach((p: string, idx: number) => {
          if (p) replacement.push({ type: "text", value: p });
          if (idx < parts.length - 1) replacement.push({ type: "break" });
        });
        node.children.splice(i, 1, ...replacement);
        i += replacement.length - 1;
      } else {
        walk(child);
      }
    }
  };
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (tree: any) => walk(tree);
}

/// 引用番号の直後に続く閉じの約物（。、）」など）。
const CLOSING_PUNCT = /^[。、，．）」』】〕〉》！？!?,.:;：；…]+/;

/// 引用番号チップと直後の約物を、折り返さない 1 つの塊にする（#1015）。
///
/// チップは行内の箱（inline-flex）なので前後で改行でき、番号が行末に来ると直後の「。」だけが
/// 次の行頭に落ちる。単語結合子（U+2060）は Chrome では箱の後ろの改行を止めないため、
/// hast の段階で `<span class="whitespace-nowrap">` に包む。
function rehypeCiteNowrap() {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const walk = (node: any) => {
    if (!node || !Array.isArray(node.children)) return;
    for (let i = 0; i < node.children.length; i++) {
      const child = node.children[i];
      const next = node.children[i + 1];
      const href = child?.type === "element" && child.tagName === "a" ? String(child.properties?.href ?? "") : "";
      const punct = next?.type === "text" ? CLOSING_PUNCT.exec(next.value)?.[0] : undefined;
      if (punct && citeNumberFromHref(href) != null) {
        next.value = next.value.slice(punct.length);
        const glued = {
          type: "element",
          tagName: "span",
          properties: { className: ["whitespace-nowrap"] },
          children: [child, { type: "text", value: punct }],
        };
        node.children.splice(i, next.value ? 1 : 2, glued);
        continue;
      }
      walk(child);
    }
  };
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (tree: any) => walk(tree);
}

/// 段落の中の単独の改行を改行として描く（利用者の発話用・GFM の既定では空白に潰れる）。
/// textarea の時代に送った「一行目\n二行目」も、そのまま 2 行で見せるため。コードは触らない。
function remarkSoftBreaks() {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const walk = (node: any) => {
    if (!node || !Array.isArray(node.children)) return;
    for (let i = 0; i < node.children.length; i++) {
      const child = node.children[i];
      if (child.type === "text" && child.value.includes("\n")) {
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        const replacement: any[] = [];
        child.value.split("\n").forEach((part: string, idx: number) => {
          if (idx > 0) replacement.push({ type: "break" });
          if (part) replacement.push({ type: "text", value: part });
        });
        node.children.splice(i, 1, ...replacement);
        i += replacement.length - 1;
      } else {
        walk(child);
      }
    }
  };
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (tree: any) => walk(tree);
}

/// 1 つの React 要素（```pre > code```）からコード文字列と言語を取り出す。
/// `raw` は react-markdown が渡す中身そのもの（フェンスが閉じていなくても末尾に改行が足されている）。
function extractCode(child: React.ReactNode): {
  code: string;
  raw: string;
  lang?: string;
  ui: boolean;
  /// ```ui:bar のように情報文字列で指定された図の種類。
  uiKind?: string;
  /// ```data_grid（ミニアプリの画面の表・Task 15.5）。
  dataGrid?: boolean;
} {
  if (!React.isValidElement(child)) return { code: "", raw: "", ui: false };
  const props = child.props as { className?: string; children?: React.ReactNode };
  const className = props.className ?? "";
  const match = /language-(\w+)/.exec(className);
  const raw = String(props.children ?? "");
  return {
    code: raw.replace(/\n$/, ""),
    raw,
    lang: match?.[1],
    ui: isYuiLanguage(className),
    uiKind: /language-(?:ui|yui):(\S+)/.exec(className)?.[1],
    dataGrid: /(?:^|\s)language-data[_-]grid(?:\s|$)/.test(className),
  };
}

/// pre 直下の code 要素のクラス名。
function codeClassName(child: React.ReactNode): string | undefined {
  return React.isValidElement(child) ? (child.props as { className?: string }).className : undefined;
}

/// 描画中の本文と、生成中かどうか（```ui の図が書きかけの行を描かないようにする）。
/// `untrusted` は利用者が書いた本文（発話の吹き出し）。図・表・引用番号にせず、リンク先を見せる。
/// `imageSources` は画像として読み込んでよい URL（出典に現れたもの・lib/answer-images）。
/// `imageProxy` は出典の画像を shiki-server 経由で読む URL を作る（#1044）。無ければ外部の画像は読み込まない。
/// `page` は外部ページ由来の本文（Web ページの訳文）。図・表・引用番号にせず、リンク先を見せる。
/// 数式と、出所の判定を通った画像だけは描く（docs/agent/web-translate.md §4.6）。
/// `linkSources` は回答の出典のホスト（lib/answer-links）。あれば、出典に無い外部リンクは確認してから開く（#1143）。
/// null は確認しない（回答以外の本文: ノート・ヘルプなど）。
const SourceContext = React.createContext<{
  text: string;
  streaming: boolean;
  untrusted: boolean;
  page: boolean;
  imageSources: ImageSources;
  imageProxy: ImageProxy | null;
  linkSources: LinkSources | null;
}>({
  text: "",
  streaming: false,
  untrusted: false,
  page: false,
  imageSources: NO_IMAGE_SOURCES,
  imageProxy: null,
  linkSources: null,
});

/// フェンスが原文で終わっているか。react-markdown（mdast-util-to-hast）はフェンスの中身の末尾に
/// 必ず改行を足すため、中身からは閉じたかどうかも、最終行が書き終わったかどうかも分からない。
/// 閉じ行がある場合のほか、後ろに本文が続く場合（引用・リストの終わりで暗黙に閉じた）も終わっている。
function fenceEnded(text: string, node: ExtraProps["node"]): boolean {
  const start = node?.position?.start.offset;
  const end = node?.position?.end.offset;
  if (start === undefined || end === undefined) return false;
  if (end < text.trimEnd().length) return true;
  const lines = text.slice(start, end).split(/\r?\n/);
  // 引用・リストの中のフェンスは閉じ行に `>` が付く。
  return lines.length > 1 && /^\s*(?:>\s*)*(`{3,}|~{3,})\s*$/.test(lines[lines.length - 1] ?? "");
}

/// ```ui bar のように空白区切りで書かれた種類（react-markdown はクラス名に載せず meta に残す）。
function fenceMeta(node: ExtraProps["node"]): string | undefined {
  const code = node?.children?.[0];
  const meta = code && "data" in code ? (code.data as { meta?: unknown } | undefined)?.meta : undefined;
  return typeof meta === "string" ? meta.trim().split(/\s+/)[0] : undefined;
}

/// 利用者が書いたリンク。表示の文字と行き先が違うときは行き先のホストを添える（文字で行き先を偽れない）。
function UserLink({ href, children }: { href: string; children: React.ReactNode }) {
  let host: string | null = null;
  try {
    const u = new URL(href);
    if (u.protocol === "http:" || u.protocol === "https:") host = u.host;
  } catch {
    /* 相対・不正な URL はリンクにしない */
  }
  if (!host) return <>{children}</>;
  const label = typeof children === "string" ? children : null;
  const shown = label !== null && (label === href || label === href.replace(/^https?:\/\//, ""));
  return (
    <a
      href={href}
      target="_blank"
      rel="noopener noreferrer nofollow"
      title={href}
      className={cn(
        "font-medium text-primary underline decoration-primary/30 underline-offset-2 hover:decoration-primary",
        WRAP_ANYWHERE,
      )}
    >
      {children}
      {shown ? null : <span className="ml-1 text-[0.85em] font-normal text-muted-foreground">({host})</span>}
    </a>
  );
}

/// 回答の画像。出典に現れた URL と埋め込み画像だけを読み込み、それ以外はホストを添えたリンクにする
/// （#1005・PIT-77: 回答の画像は閲覧者のブラウザがクリック無しで取りに行くので、LLM が組み立てた URL で
/// 会話の中身を外へ送らせない）。読み込む画像も Referer を送らない（どの会話から開いたかを渡さない）。
function AnswerImage({ src, alt }: { src: string; alt: string }) {
  const { imageSources, imageProxy } = React.useContext(SourceContext);
  const inLink = React.useContext(InLinkContext);
  // サーバが取得を断った・取得先が失敗した画像は、読み込まなかった画像と同じリンクに落とす。
  const [failed, setFailed] = React.useState(false);
  // サーバ描画の <img> は hydration より先に読み込みに失敗し得る（その error は React に届かない）。
  // 付いた時点で「読み終えたのに中身が無い」なら失敗として扱う。
  const imgRef = React.useCallback((img: HTMLImageElement | null) => {
    if (img && img.complete && img.naturalWidth === 0) setFailed(true);
  }, []);
  const decision = decideAnswerImage(src, imageSources);
  if (decision.kind === "drop") return alt ? <>{alt}</> : null;
  if (decision.kind === "link") {
    return (
      <BlockedImage
        href={decision.href}
        host={decision.host}
        alt={alt}
        inLink={inLink}
        reason="出典に無い外部の画像のため読み込んでいません"
      />
    );
  }
  const host = new URL(decision.src).host;
  // 出典の画像でも、代理取得の経路が無い本文（会話以外）や取得に失敗したものは読み込まない。
  // ブラウザから外部ホストへ直接は読みに行かない（閲覧の事実と IP を配信元へ渡さない・#1044）。
  if (!imageProxy || failed) {
    return (
      <BlockedImage
        href={decision.src}
        host={host}
        alt={alt}
        inLink={inLink}
        reason={failed ? "外部の画像を読み込めませんでした" : "外部の画像のため読み込んでいません"}
      />
    );
  }
  // 段落（<p>）の中に置かれるので、ブロック要素（figure）ではなく span で組む。
  return (
    <span className="my-3 block">
      {/* eslint-disable-next-line @next/next/no-img-element -- shiki-server 経由の外部画像で、next/image の最適化対象外 */}
      <img
        src={imageProxy(decision.src)}
        alt={alt}
        loading="lazy"
        decoding="async"
        referrerPolicy="no-referrer"
        ref={imgRef}
        onError={() => setFailed(true)}
        data-testid="answer-image"
        className="block h-auto max-h-[28rem] max-w-full rounded-lg border border-border/60"
      />
      <span data-testid="answer-image-caption" className="mt-1.5 block text-[12px] text-muted-foreground">
        {alt ? `${alt} · ` : ""}
        {host}
      </span>
    </span>
  );
}

/// 読み込まない外部の画像。行き先のホストを添えたリンクにする（押したときだけ新しいタブで開く）。
function BlockedImage({
  href,
  host,
  alt,
  inLink,
  reason,
}: {
  href: string;
  host: string;
  alt: string;
  inLink: boolean;
  reason: string;
}) {
  const content = (
    <>
      <EyeOff aria-hidden className="mr-1 inline size-[0.95em] -translate-y-px align-middle text-muted-foreground" />
      外部の画像{alt ? `「${alt}」` : ""}
      <span className="ml-1 text-muted-foreground">{host}</span>
    </>
  );
  const label = `${reason}（${host}）`;
  // 回答の本文なら、出典に無い行き先は押したときに確認してから開く（#1143。画像の URL に会話の中身を
  // 載せて送らせる経路は、押す操作でも開いてしまう）。
  const { linkSources } = React.useContext(SourceContext);
  const guard = useUnverifiedOpen();
  const guarded = linkSources !== null && decideAnswerLink(href, linkSources).kind === "unverified";
  // リンクの中（`[![badge](img)](href)`）では外側のリンクに任せ、ラベルだけにする（<a> を入れ子にしない）。
  if (inLink) {
    return (
      <span
        title={`${label}: ${href}`}
        data-testid="answer-image-blocked"
        className={cn("rounded-[5px] bg-muted/60 px-1 text-[0.9em] text-foreground/80", WRAP_ANYWHERE)}
      >
        {content}
      </span>
    );
  }
  return (
    <>
    <a
      href={href}
      target={guarded ? undefined : "_blank"}
      rel="noopener noreferrer nofollow"
      referrerPolicy="no-referrer"
      title={`${label}。押すと${guarded ? "確認してから" : ""}新しいタブで開きます: ${href}`}
      aria-label={`${alt || "画像"}: ${label}。押すと${guarded ? "確認してから" : ""}新しいタブで開きます`}
      data-testid="answer-image-blocked"
      {...(guarded ? guard.handlers : {})}
      className={cn(
        // 本文の行の高さを変えない（上下の余白を持たせない）。枠は付けず、押せることは下線と hover で見せる。
        "inline rounded-[5px] bg-muted/60 px-1 text-[0.9em] text-foreground/80 underline decoration-border",
        "underline-offset-2 hover:bg-muted hover:text-foreground hover:decoration-foreground/40",
        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
        WRAP_ANYWHERE,
      )}
    >
      {content}
    </a>
    {guarded ? (
      <UnverifiedLinkDialog open={guard.open} onOpenChange={guard.setOpen} href={href} label={host} />
    ) : null}
    </>
  );
}

/// 図の「表で表示」。リンク・画像は描かない（図と同じく文字と数値だけにする）。
/// ノートの ```ui の図（NodeView）も同じ描き方にする。
export function renderFigureTable(markdown: string) {
  return (
    <ReactMarkdown
      remarkPlugins={[remarkGfm]}
      components={components}
      allowedElements={["p", "strong", "em", "br", "table", "thead", "tbody", "tr", "th", "td"]}
      unwrapDisallowed
    >
      {markdown}
    </ReactMarkdown>
  );
}

/// ブロックコード。```ui（Yui）の図は図にし、描けなければ通常のコードブロックに戻す（#530）。
function FencedCode({ node, children }: { node: ExtraProps["node"]; children: React.ReactNode }) {
  const { text, streaming, untrusted, page } = React.useContext(SourceContext);
  const { code, raw, lang, ui, uiKind, dataGrid } = extractCode(children);
  const closed = fenceEnded(text, node);
  const nested = React.useContext(YuiNestedContext);
  const gridHost = React.useContext(DataGridHostContext);
  // 利用者が書いたフェンスは図・表にせず、コードとして見せる（図と操作は AI の回答だけのもの）。
  if (untrusted) return <CodeBlock code={code} lang={lang} />;
  // 別行立ての数式（remark-math が pre > code.math-display にする・数式を読み込んだときだけ現れる）。
  if (mathKind(codeClassName(children)) === "display") return <MathTex tex={code} display />;
  // 外部ページの本文が書いたフェンスは、言語名が ui・data_grid でも図・表・ボタンにしない
  // （ページの内容が会話の操作を生まないように）。
  if (page) return <CodeBlock code={code} lang={lang} />;
  // ```data_grid はミニアプリの画面の表（トップレベルだけ・サーバーと同じ規則）。中身の 1 行目が別名。
  if (dataGrid && gridHost && !nested && closed) {
    const alias = code.split(/\r?\n/)[0]?.trim() ?? "";
    if (alias) {
      return (
        <React.Suspense
          fallback={<div className="my-4 h-24 animate-pulse rounded-xl border bg-muted/40" aria-label="表を読み込み中" />}
        >
          <MiniAppDataGrid alias={alias} />
        </React.Suspense>
      );
    }
  }
  // 生成中でフェンスがまだ閉じていないコードはハイライトしない（トークンごとに shiki を回さない）。
  if (!ui) return <CodeBlock code={code} lang={lang} pending={streaming && !closed} />;
  // 終わっていないブロックは、確定した行だけを渡す（本文が改行で終わっていれば最終行も確定）。
  // 生成中は書きかけの行が伸びるたびに図を解析し直さないため、生成後（停止・閉じ忘れ）は
  // 途中で切れた値を確定値として描かないため。
  const lastLineDone = closed || text.endsWith("\n");
  const source = lastLineDone ? raw : raw.slice(0, raw.lastIndexOf("\n", raw.length - 2) + 1);
  const kind = uiKind ?? fenceMeta(node);
  return (
    <UiFigure
      source={source}
      // 記述そのものを見せるのは書き終わった後だけ。生成中は渡さない（トークンごとに描き直さない）。
      raw={closed || !streaming ? raw : undefined}
      complete={closed || !streaming}
      closed={closed}
      truncated={!lastLineDone && !streaming && raw.trim() !== source.trim()}
      kind={kind ? resolveKind(kind) : undefined}
      lang={lang}
      renderTable={renderFigureTable}
    />
  );
}

/// 本文のリンク（引用番号・内部リンク・外部リンク・利用者が書いたリンク）。
function MarkdownLink({ href, children }: { href?: string; children: React.ReactNode }) {
  const { untrusted, page, linkSources } = React.useContext(SourceContext);
  const url = href ?? "";
  // 外部ページの本文のリンクも、引用番号・アプリ内の遷移にしない（http(s) だけ・行き先のホストを見せる）。
  if (untrusted || page) return <UserLink href={url}>{children}</UserLink>;
  // 引用マーカー（`#cite-n`・linkifyCitations が付ける）は本文中の上付き番号チップにする。
  // ホバーで原文カード、クリックで出典パネル（メッセージの引用文脈があるときのみ）。
  const cite = citeNumberFromHref(url);
  if (cite != null) return <CitationMarker n={cite}>{children}</CitationMarker>;
  // その他の内部リンクはクライアント遷移。
  if (url.startsWith("/")) {
    return (
      <Link
        href={url}
        className={cn(
          "font-medium text-primary underline decoration-primary/30 underline-offset-2 hover:decoration-primary",
          WRAP_ANYWHERE,
        )}
      >
        {children}
      </Link>
    );
  }
  const linkClass = cn(
    "font-medium text-primary underline decoration-primary/30 underline-offset-2 hover:decoration-primary",
    WRAP_ANYWHERE,
  );
  // 回答の本文なら、出典（Web の引用）に無い行き先は確認してから開く（#1143）。
  if (linkSources) {
    const decision = decideAnswerLink(url, linkSources);
    if (decision.kind === "plain") return <>{children}</>;
    if (decision.kind === "unverified") {
      return (
        <UnverifiedLink href={decision.href} label={decision.label} className={linkClass}>
          {children}
        </UnverifiedLink>
      );
    }
  }
  return (
    <a href={url} target="_blank" rel="noopener noreferrer" className={linkClass}>
      {children}
    </a>
  );
}

const components: Components = {
  // ブロックコードは pre をフックして CodeBlock に差し替える。
  pre({ node, children }) {
    // 数式（math-display）・図・表・コードのどれで描くかは FencedCode が決める。
    return <FencedCode node={node}>{children}</FencedCode>;
  },
  code({ className, children }) {
    // 行内の数式（remark-math が code.math-inline にする）。`\[ \]` が文中にあれば math-display。
    const math = mathKind(className);
    if (math) return <MathTex tex={String(children ?? "")} display={math === "display"} />;
    // インラインコード（pre 配下は上で処理済み）。
    return (
      <code
        className={cn(
          "rounded-[5px] bg-muted px-1.5 py-0.5 font-mono text-[0.85em] text-foreground",
          WRAP_ANYWHERE,
          className,
        )}
      >
        {children}
      </code>
    );
  },
  a({ href, children }) {
    return (
      <InLinkContext.Provider value={true}>
        <MarkdownLink href={href}>{children}</MarkdownLink>
      </InLinkContext.Provider>
    );
  },
  // 組版（#1015・案 B）: 日本語に合わせて行間を 1.8 にし、段落・リスト・見出しの前後に余白をとって
  // まとまりを分ける。本文の色はわずかに落とし（ルート）、見出し・太字を foreground で浮かせる。
  p({ children }) {
    return <p className="my-3.5 leading-[1.8] first:mt-0 last:mb-0">{children}</p>;
  },
  ul({ children }) {
    return <ul className="my-3.5 ml-5 list-disc space-y-1.5 marker:text-primary/60">{children}</ul>;
  },
  ol({ children }) {
    return <ol className="my-3.5 ml-5 list-decimal space-y-1.5 marker:text-primary/60">{children}</ol>;
  },
  li({ children }) {
    // 箇条書きの中の図はボタンを出さない（操作を持つ図はトップレベルだけ・サーバーと同じ規則）。
    return (
      <li className="leading-[1.8]">
        <YuiNestedContext.Provider value={true}>{children}</YuiNestedContext.Provider>
      </li>
    );
  },
  // 見出しは段の区切りなので上を大きく空ける。h1/h2 は下線、h3 は色の縦線で本文と階層を分ける。
  h1({ children }) {
    return (
      <h1 className="mb-3 mt-9 border-b border-border pb-2 text-xl font-bold text-foreground first:mt-0">
        {children}
      </h1>
    );
  },
  h2({ children }) {
    return (
      <h2 className="mb-3 mt-9 border-b border-border pb-2 text-[18px] font-bold text-foreground first:mt-0">
        {children}
      </h2>
    );
  },
  h3({ children }) {
    return (
      <h3 className="mb-2 mt-7 border-l-[3px] border-primary pl-2.5 text-[15px] font-semibold leading-snug text-foreground first:mt-0">
        {children}
      </h3>
    );
  },
  h4({ children }) {
    return <h4 className="mb-1.5 mt-5 text-[15px] font-semibold text-foreground first:mt-0">{children}</h4>;
  },
  blockquote({ children }) {
    return (
      <blockquote className="my-5 rounded-r-lg border-l-[3px] border-primary/30 bg-muted/50 py-2 pl-4 pr-3">
        <YuiNestedContext.Provider value={true}>{children}</YuiNestedContext.Provider>
      </blockquote>
    );
  },
  hr() {
    return <hr className="my-6 border-border" />;
  },
  // 表は横スクロールの箱に収める（本文の幅を押し広げない）。見出しは折り返さず、セルは
  // 最小幅を持たせて 1 文字ずつの縦長にならないようにする。箱の端まで来た横スクロールが
  // ページ（戻るジェスチャ）へ抜けないよう overscroll-x-contain。
  table({ children }) {
    return (
      <div className="my-5 max-w-full overflow-x-auto overscroll-x-contain rounded-lg border border-border">
        <table className="w-full border-collapse text-[13px]">{children}</table>
      </div>
    );
  },
  thead({ children }) {
    return <thead className="bg-muted/50">{children}</thead>;
  },
  th({ children }) {
    return (
      <th className="whitespace-nowrap border-b border-border px-3 py-2 text-left font-semibold">
        {children}
      </th>
    );
  },
  td({ children }) {
    return (
      <td className="min-w-[7rem] border-b border-border/60 px-3 py-2 [overflow-wrap:break-word]">
        {children}
      </td>
    );
  },
  strong({ children }) {
    return <strong className="font-semibold text-foreground">{children}</strong>;
  },
  img({ src, alt }) {
    // 利用者の発話は disallowedElements で画像を落とすので、ここに来るのは回答・画面の本文だけ。
    // react-markdown は http(s) 以外（data:・javascript: など）の URL を空にして渡す。そのときは代替テキストだけ出す。
    if (typeof src === "string" && src) return <AnswerImage src={src} alt={alt ?? ""} />;
    return alt ? <>{alt}</> : null;
  },
};

/// LLM の回答・利用者の発話・ノートなどの Markdown を描く。AI の回答では、数式を含むときだけ
/// 数式の解析と KaTeX を読み込み、引用番号と直後の約物を折り返さない塊にする（#1015）。
export function Markdown({
  children,
  className,
  streaming = false,
  breaks = false,
  untrusted = false,
  page = false,
  imageSources = NO_IMAGE_SOURCES,
  imageProxy = null,
  linkSources = null,
}: {
  children: string;
  className?: string;
  /// 生成中の本文か（```ui の図が書きかけの行を描かないようにする）。
  streaming?: boolean;
  /// 段落の中の単独の改行も改行として描く（利用者の発話）。
  breaks?: boolean;
  /// 利用者が書いた本文。画像は読み込まず（外部へのアクセスを生まない）、図・表・引用番号にせず、
  /// リンクには行き先のホストを添える。
  untrusted?: boolean;
  /// 外部ページ由来の本文（Web ページの訳文）。図・表・引用番号・アプリ内リンクにせず、リンクには
  /// 行き先のホストを添える。数式は描き、画像は `imageSources` の出所の判定に従う。
  page?: boolean;
  /// 画像として読み込んでよい URL（回答なら、その回答のツール結果・引用に現れた画像・lib/answer-images）。
  /// 省略すると外部の画像は読み込まず、行き先のホストを添えたリンクにする。
  imageSources?: ImageSources;
  /// 出典の画像を shiki-server 経由で読む URL を作る（#1044）。省略すると出典の画像も読み込まずリンクにする。
  imageProxy?: ImageProxy | null;
  /// 回答の出典のホスト（lib/answer-links の answerLinkSources）。渡すと、出典に無い外部リンクと外部画像の
  /// リンクは、押したときに確認してから開く（#1143）。省略（null）すると確認しない（回答以外の本文）。
  linkSources?: LinkSources | null;
}) {
  // 数式は AI の回答だけ（利用者の発話は打ったとおりに見せる）。
  const mathPlugins = useMathPlugins(!untrusted && mayHaveMath(children));
  const text = children;
  return (
    <div
      className={cn(
        // 本文はわずかに色を落とし、見出し・太字（foreground）を浮かせる（#1015）。
        "min-w-0 text-[15px] leading-[1.8] text-foreground/85",
        WRAP_ANYWHERE,
        className,
      )}
    >
      <SourceContext.Provider value={{ text, streaming, untrusted, page, imageSources, imageProxy, linkSources }}>
        <ReactMarkdown
          remarkPlugins={[
            remarkGfm,
            // 日本語の約物に隣接する `**` を強調として閉じる（`**「結論」**について` が太字にならない問題）。
            remarkCjkFriendly,
            remarkBrTags,
            ...(breaks ? [remarkSoftBreaks] : []),
            ...(mathPlugins ?? []),
          ]}
          rehypePlugins={untrusted || page ? undefined : [rehypeCiteNowrap]}
          components={components}
          disallowedElements={untrusted ? ["img"] : undefined}
        >
          {text}
        </ReactMarkdown>
      </SourceContext.Provider>
    </div>
  );
}
