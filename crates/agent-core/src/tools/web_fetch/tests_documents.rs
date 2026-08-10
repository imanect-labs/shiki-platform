//! `web_fetch` の**文書経路**（PDF/Office → Docling）のテスト。
//!
//! 「宣言・拡張子・magic bytes のどれで文書と判定するか」と「worker へ何を渡すか」を見る。
//! HTML 抽出の効率は [`super::tests_efficiency`]、取得経路の防御は [`super::tests`]
//! （行数規約でファイルを分けている）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::tests::{ctx, http_response, stub_server, tool_with};
use super::*;

/// スタブ parser と、その受け取り記録。
type Spy = (
    Arc<dyn rag::DocumentParser>,
    Arc<std::sync::Mutex<Vec<String>>>,
);

/// 受け取った要求（`ParseSource` の種別と申告 MIME）を記録するスタブ parser。
struct SpyParser {
    seen: Arc<std::sync::Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl rag::DocumentParser for SpyParser {
    async fn parse(
        &self,
        _ctx: &AuthContext,
        req: rag::ParseRequest<'_>,
    ) -> Result<rag::types::ParsedDocument, rag::RagError> {
        let kind = match req.source {
            rag::ParseSource::Url(u) => format!("url:{u}"),
            rag::ParseSource::Bytes(b) => format!("bytes:{} as {}", b.len(), req.content_type),
        };
        self.seen.lock().unwrap().push(kind);
        Ok(rag::types::ParsedDocument {
            blocks: vec![
                rag::types::ParsedBlock {
                    block_type: rag::types::BlockType::Heading,
                    level: Some(1),
                    text: "令和8年度 市場動向調査".into(),
                    page: Some(1),
                },
                rag::types::ParsedBlock {
                    block_type: rag::types::BlockType::Paragraph,
                    level: None,
                    text: "国内市場は 1.2 兆円となった。".into(),
                    page: Some(1),
                },
            ],
            used_ocr: true,
        })
    }
}

fn spy() -> Spy {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    (Arc::new(SpyParser { seen: seen.clone() }), seen)
}

/// PDF は Docling へ回す。**worker には URL ではなくバイト列を渡す**（PIT-48 の迂回防止）。
#[tokio::test]
async fn pdf_goes_to_docling_with_bytes_never_url() {
    let (parser, seen) = spy();
    let (addr, _) = stub_server(http_response(
        "HTTP/1.1 200 OK\r\nContent-Type: application/pdf",
        "%PDF-1.7 fake body",
    ))
    .await;
    let (mut tool, _) = tool_with(vec![addr]);
    tool.skip_addr_guard = true;
    let tool = tool.with_parser(parser);

    let out = tool
        .call(
            &ctx(),
            serde_json::json!({"url": format!("http://gov.example.invalid:{}/report.pdf", addr.port())}),
            None,
        )
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert!(
        out.content.contains("令和8年度 市場動向調査"),
        "{}",
        out.content
    );
    assert!(out.content.contains("1.2 兆円"), "{}", out.content);
    assert!(out.content.contains("docling+ocr"), "{}", out.content);

    // 決定的な不変条件: worker へ渡ったのはバイト列だけで、URL は一度も渡っていない。
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].starts_with("bytes:"), "{:?}", seen);
}

/// 汎用バイナリ MIME でも、URL の拡張子を根拠に文書経路へ回す。
///
/// 官公庁の配信や CDN のダウンロードエンドポイントは PDF を `application/octet-stream` で
/// 返す。宣言だけを見ると「テキストではない」で門前払いになり、宣伝している PDF 対応が
/// 相手のサーバ設定次第で消えていた。
#[tokio::test]
async fn opaque_content_type_with_document_extension_still_parses() {
    let (parser, seen) = spy();
    let (addr, _) = stub_server(http_response(
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream",
        "%PDF-1.7 fake body",
    ))
    .await;
    let (mut tool, _) = tool_with(vec![addr]);
    tool.skip_addr_guard = true;
    let tool = tool.with_parser(parser);
    let out = tool
        .call(
            &ctx(),
            serde_json::json!({"url": format!("http://dl.example.invalid:{}/files/2026.pdf?dl=1", addr.port())}),
            None,
        )
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("1.2 兆円"), "{}", out.content);
    // worker へは Docling が解釈できる MIME を申告する（octet-stream のままでは弾かれる）。
    let seen = seen.lock().unwrap();
    assert!(seen[0].ends_with("as application/pdf"), "{seen:?}");
}

/// 宣言も拡張子も外れる配信は、取得後の magic bytes で拾う。
#[tokio::test]
async fn pdf_without_content_type_is_detected_by_magic_bytes() {
    let (parser, seen) = spy();
    let (addr, _) = stub_server(http_response("HTTP/1.1 200 OK", "%PDF-1.7 fake body")).await;
    let (mut tool, _) = tool_with(vec![addr]);
    tool.skip_addr_guard = true;
    let tool = tool.with_parser(parser);
    let out = tool
        .call(
            &ctx(),
            serde_json::json!({"url": format!("http://dl.example.invalid:{}/download", addr.port())}),
            None,
        )
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("docling"), "{}", out.content);
    assert_eq!(seen.lock().unwrap().len(), 1);
}

/// **上限で切れた文書はパーサへ渡さない**。
///
/// PDF の相互参照表も OOXML のセントラルディレクトリも末尾にあるため、切れたバイト列は
/// 必ず解析に失敗する。無駄に Docling を回すのではなく「大きすぎる」と返す方が、モデルは
/// 次の手（分割版・HTML 版）を打てる。
#[tokio::test]
async fn truncated_document_is_never_sent_to_the_parser() {
    let (parser, seen) = spy();
    // Content-Type 無し＝テキストとして 256KiB 上限で読む。中身は PDF なので magic で拾われる。
    let body = format!("%PDF-1.7 {}", "x".repeat(FETCH_BODY_CAP + 10_000));
    let (addr, _) = stub_server(http_response("HTTP/1.1 200 OK", &body)).await;
    let (mut tool, _) = tool_with(vec![addr]);
    tool.skip_addr_guard = true;
    let tool = tool.with_parser(parser);
    let out = tool
        .call(
            &ctx(),
            serde_json::json!({"url": format!("http://dl.example.invalid:{}/big", addr.port())}),
            None,
        )
        .await
        .unwrap();
    assert!(out.is_error, "{}", out.content);
    assert!(
        out.content.contains("切れているため解析しません"),
        "{}",
        out.content
    );
    assert!(out.content.contains("256 KiB"), "{}", out.content);
    assert!(seen.lock().unwrap().is_empty(), "パーサを呼んでいる");
}
