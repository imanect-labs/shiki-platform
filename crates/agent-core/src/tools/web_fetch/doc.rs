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

/// Docling へ回すバイナリ文書の MIME（worker の `_DOCLING_TYPES` のうち非テキスト）。
const DOCUMENT_TYPES: &[&str] = &[
    "application/pdf",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
];

/// パース待ちの上限。Docling は OCR 込みで数十秒かかる（大きなスキャン PDF は分単位）。
/// 対話ツールとしてはここで諦め、モデルに次の手を打たせる方が良い。
pub(super) const PARSE_TIMEOUT: Duration = Duration::from_secs(90);

/// この Content-Type は Docling 経路で読めるか。
pub(super) fn is_document(content_type: Option<&str>) -> bool {
    let Some(ct) = content_type else {
        return false;
    };
    let base = ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    DOCUMENT_TYPES.contains(&base.as_str())
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

    #[test]
    fn recognizes_document_types() {
        assert!(is_document(Some("application/pdf")));
        assert!(is_document(Some("APPLICATION/PDF; charset=binary")));
        assert!(!is_document(Some("text/html")));
        assert!(!is_document(Some("image/png")));
        assert!(!is_document(None));
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
