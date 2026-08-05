//! PDF/Office 文書 → ingestion-worker（Docling）経路（#405）。
//!
//! 国内の一次ソース（官公庁の統計・審議会資料・企業の IR）は PDF が主で、旧実装は
//! `is_textual` で弾いていた。deep research が「一次ソースを優先せよ」と言いながら
//! 一次ソースを開けない状態だったので、既にある Docling 経路へ載せる。
//!
//! **URL を worker へ渡さない**のが本モジュールの要点。worker の `/parse` は
//! `source_url` を受け取ると自分で HTTP GET する（SSRF ガード無し）。任意 URL を渡せると
//! `web_fetch` の宛先制限（解決後 IP 検証＋接続アドレス固定・PIT-48）を worker 経由で
//! 丸ごと迂回できてしまう。よって **web_fetch がガード済み経路で取得したバイト列**を
//! [`rag::ParseSource::Bytes`] で渡し、worker には取得させない。

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use authz::AuthContext;
use rag::types::{BlockType, ParsedDocument};
use rag::{DocumentParser, ParseRequest, ParseSource};
use url::Url;

use super::input::base_type;

/// Docling へ回すバイナリ文書の MIME（worker の `_DOCLING_TYPES` のうち非テキスト）。
const DOCUMENT_TYPES: &[&str] = &[
    "application/pdf",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
];

/// 拡張子 → MIME。**Content-Type が当てにならない配信**への保険。
const DOCUMENT_EXTENSIONS: &[(&str, &str)] = &[
    ("pdf", "application/pdf"),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    (
        "pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
];

/// 「中身を見ないと形式が分からない」汎用バイナリ MIME（ダウンロード配信でよく付く）。
const OPAQUE_TYPES: &[&str] = &[
    "application/octet-stream",
    "binary/octet-stream",
    "application/download",
    "application/x-download",
    "application/force-download",
];

/// パース待ちの上限。Docling は OCR 込みで数十秒かかる（大きなスキャン PDF は分単位）。
/// 対話ツールとしてはここで諦め、モデルに次の手を打たせる方が良い。
///
/// **諦めても worker 側の解析は止まらない**（同期処理でキャンセルできない）。ここを
/// 「先に諦めてよい」根拠にしているのは、worker 側が同時解析数を有界にして
/// 取り残しがスレッドプールを食い潰さないようにしているため（`ingestion-worker` の
/// `ParseSlots`。上限到達時は待たせず 503）。片方だけでは成立しない対で運用する。
pub(super) const PARSE_TIMEOUT: Duration = Duration::from_secs(90);

/// この応答を Docling 経路で読むか。読むなら **worker へ申告する MIME** を返す。
///
/// Content-Type だけを信じない。官公庁の配信や CDN のダウンロードエンドポイントは PDF を
/// `application/octet-stream` で返すことがあり、宣言だけ見ると「テキストではない」と
/// 門前払いになる（宣伝している PDF 対応が、相手のサーバ設定次第で消える）。
pub(super) fn classify(content_type: Option<&str>, url: &Url) -> Option<&'static str> {
    let base = base_type(content_type);
    if let Some(mime) = DOCUMENT_TYPES.iter().copied().find(|m| *m == base) {
        return Some(mime);
    }
    // 拡張子を根拠にするのは**宣言が無い／汎用バイナリのときだけ**。
    // `text/html` を拡張子で覆すと、PDF ビューアの HTML ページを Docling へ回してしまう。
    if content_type.is_none() || OPAQUE_TYPES.contains(&base.as_str()) {
        return extension_type(url);
    }
    None
}

/// URL の末尾セグメントの拡張子から MIME を引く。
fn extension_type(url: &Url) -> Option<&'static str> {
    let last = url.path().rsplit('/').next()?;
    let ext = last.rsplit_once('.')?.1.to_ascii_lowercase();
    DOCUMENT_EXTENSIONS
        .iter()
        .find(|(e, _)| *e == ext)
        .map(|(_, mime)| *mime)
}

/// 取得済みバイト列の先頭から形式を見分ける（宣言も拡張子も外れる配信の最後の砦）。
///
/// OOXML は ZIP なので先頭バイトだけでは docx/pptx/xlsx を割れない。ここは **PDF に限る**
/// （国内の一次ソースは PDF が主。曖昧な推測で Docling を誤爆させる方が高くつく）。
pub(super) fn sniff(bytes: &[u8]) -> Option<&'static str> {
    bytes.starts_with(b"%PDF-").then_some("application/pdf")
}

/// 取得済みバイト列を Docling でパースし、Markdown へ落とす。
///
/// `file_name` は Docling の形式判定に効くため、URL の末尾セグメントを渡す。
pub(super) async fn parse_to_markdown(
    parser: &Arc<dyn DocumentParser>,
    ctx: &AuthContext,
    bytes: &[u8],
    content_type: &str,
    file_name: &str,
) -> Result<Parsed, String> {
    let request = ParseRequest {
        source: ParseSource::Bytes(bytes),
        content_type,
        file_name,
    };
    let parsed = tokio::time::timeout(PARSE_TIMEOUT, parser.parse(ctx, request))
        .await
        .map_err(|_| {
            format!(
                "文書の解析が {} 秒を超えたため中断しました（大きな PDF・スキャン文書の可能性）",
                PARSE_TIMEOUT.as_secs()
            )
        })?
        .map_err(|e| format!("文書の解析に失敗しました: {e}"))?;
    Ok(to_markdown(&parsed))
}

/// Docling の出力（Markdown 本文と併記メタ）。
pub(super) struct Parsed {
    pub markdown: String,
    pub used_ocr: bool,
    /// 最初の見出し（タイトルとして扱う）。
    pub title: Option<String>,
}

/// 構造化ブロック列を Markdown へ。**ページ番号を見出しに残す**（引用位置の手掛かり）。
fn to_markdown(parsed: &ParsedDocument) -> Parsed {
    let mut out = String::new();
    let mut title = None;
    let mut page = None;
    for block in &parsed.blocks {
        if block.text.trim().is_empty() {
            continue;
        }
        if block.page.is_some() && block.page != page {
            page = block.page;
            if let Some(p) = page {
                let _ = writeln!(out, "\n<!-- p.{p} -->");
            }
        }
        match block.block_type {
            BlockType::Heading => {
                let level = block.level.unwrap_or(1).clamp(1, 6) as usize;
                if title.is_none() {
                    title = Some(block.text.trim().to_string());
                }
                let _ = writeln!(out, "\n{} {}\n", "#".repeat(level), block.text.trim());
            }
            // 表は worker が既に Markdown 化して寄越す（そのまま置く）。
            BlockType::Table => {
                let _ = writeln!(out, "\n{}\n", block.text.trim());
            }
            BlockType::Caption => {
                let _ = writeln!(out, "*{}*\n", block.text.trim());
            }
            BlockType::Paragraph => {
                let _ = writeln!(out, "{}\n", block.text.trim());
            }
        }
    }
    Parsed {
        markdown: out.trim().to_string(),
        used_ocr: parsed.used_ocr,
        title,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rag::types::ParsedBlock;

    fn block(
        block_type: BlockType,
        text: &str,
        level: Option<u32>,
        page: Option<i32>,
    ) -> ParsedBlock {
        ParsedBlock {
            block_type,
            level,
            text: text.into(),
            page,
        }
    }

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn recognizes_document_types() {
        let plain = url("https://example.com/a");
        assert_eq!(
            classify(Some("application/pdf"), &plain),
            Some("application/pdf")
        );
        assert_eq!(
            classify(Some("APPLICATION/PDF; charset=binary"), &plain),
            Some("application/pdf")
        );
        assert_eq!(classify(Some("text/html"), &plain), None);
        assert_eq!(classify(Some("image/png"), &plain), None);
        assert_eq!(classify(None, &plain), None);
    }

    /// 官公庁の配信は PDF を `application/octet-stream` で返すことがある。
    #[test]
    fn falls_back_to_the_url_extension_for_opaque_types() {
        let pdf = url("https://www.meti.go.jp/report/data/2026_report.pdf?dl=1");
        assert_eq!(
            classify(Some("application/octet-stream"), &pdf),
            Some("application/pdf")
        );
        assert_eq!(classify(None, &pdf), Some("application/pdf"));
        let docx = url("https://example.com/files/資料.docx");
        assert!(classify(Some("binary/octet-stream"), &docx)
            .is_some_and(|m| m.ends_with("wordprocessingml.document")));
        // 拡張子が無ければ従来どおり非文書。
        assert_eq!(
            classify(Some("application/octet-stream"), &pdf.join("x").unwrap()),
            None
        );
    }

    /// 宣言が正しいときは拡張子で覆さない（PDF ビューアの HTML を Docling へ回さない）。
    #[test]
    fn declared_html_is_never_overridden_by_the_extension() {
        let viewer = url("https://example.com/viewer/report.pdf");
        assert_eq!(classify(Some("text/html; charset=utf-8"), &viewer), None);
    }

    #[test]
    fn sniffs_pdf_magic_bytes() {
        assert_eq!(sniff(b"%PDF-1.7\n%..."), Some("application/pdf"));
        assert_eq!(sniff(b"<!doctype html>"), None);
        assert_eq!(sniff(b""), None);
    }

    #[test]
    fn renders_blocks_with_page_markers() {
        let parsed = ParsedDocument {
            blocks: vec![
                block(
                    BlockType::Heading,
                    "令和8年度 市場動向調査",
                    Some(1),
                    Some(1),
                ),
                block(
                    BlockType::Paragraph,
                    "国内市場は 1.2 兆円となった。",
                    None,
                    Some(1),
                ),
                block(
                    BlockType::Table,
                    "| 年 | 規模 |\n|---|---|\n| 2026 | 1.2兆 |",
                    None,
                    Some(2),
                ),
                block(BlockType::Caption, "図1 推移", None, Some(2)),
            ],
            used_ocr: true,
        };
        let out = to_markdown(&parsed);
        assert_eq!(out.title.as_deref(), Some("令和8年度 市場動向調査"));
        assert!(out.used_ocr);
        assert!(
            out.markdown.contains("# 令和8年度 市場動向調査"),
            "{}",
            out.markdown
        );
        assert!(out.markdown.contains("<!-- p.1 -->"), "{}", out.markdown);
        assert!(out.markdown.contains("<!-- p.2 -->"), "{}", out.markdown);
        assert!(
            out.markdown.contains("| 2026 | 1.2兆 |"),
            "{}",
            out.markdown
        );
        assert!(out.markdown.contains("*図1 推移*"), "{}", out.markdown);
    }

    #[test]
    fn skips_empty_blocks_and_clamps_heading_level() {
        let parsed = ParsedDocument {
            blocks: vec![
                block(BlockType::Paragraph, "   ", None, None),
                block(BlockType::Heading, "深い見出し", Some(99), None),
            ],
            used_ocr: false,
        };
        let out = to_markdown(&parsed);
        assert!(
            out.markdown.starts_with("###### 深い見出し"),
            "{}",
            out.markdown
        );
    }
}
