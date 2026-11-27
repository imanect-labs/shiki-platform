//! 「ファイルを読む」（storage.read ノード）の読み口（D-111・`NodePorts::storage_read_document`）。
//!
//! 文章のファイルは中身を文字コードを決めて読み（[`read_text_file`]・BUG-101・GAP-45）、文章でない種類（PDF・Office
//! など）は、社内文書の検索が取り込んだ本文を `rag::SearchService::read_document_text` で読む（原本は読み直さない）。
//!
//! 認可はチャットの doc_read と同じ契約にする:
//! - 閲覧の判定は StorageService（`get_metadata`・viewer・監査つき）。ここを通った `Node` だけを索引の読み出しに渡す。
//! - 索引の読み出しは、検索と同じ可読集合で本文の authz_tags を照合する（pre-filter）。片方が誤って許可しても本文は
//!   返さない（二段 authz）。
//! - 見えない・無いは同じ誤り（存在の秘匿）。監査では区別が残る。
//!
//! 索引に本文が無いときは、待てば読めるのか（`index_pending`・やり直せる）と、待っても読めないのか（取り込めない
//! 種類・大きすぎる・本文が無い・AI エージェントの作業用のフォルダーにあるは `not_indexable`、取り込みに失敗したもの・
//! 取り込みが届いていないものは `index_failed`・やり直さない）を分ける。`index_failed` は、ファイルを編集できる人が
//! ドライブのメニューで取り込み直せる（D-126）ので、そう案内する。社内文書の検索が使えない環境は `search_disabled`
//! （この環境で使えない機能の分類・BUG-158）。

use serde_json::{json, Value};
use storage::{Node, NodeKind, StorageError, StorageService};
use uuid::Uuid;
use workflow_engine::nodes::ports::{
    hidden_not_found, INDEX_FAILED, INDEX_PENDING, NOT_INDEXABLE, NOT_TEXT_FILE, SEARCH_DISABLED,
};
use workflow_engine::{ExecCtx, PortError, INDEX_WAIT_MAX_MINUTES};

use super::auth::auth_ctx;
use super::ports::{map_storage, ProdNodePorts};
use super::text_file::{is_binary, not_readable_text, read_text};

/// 社内文書の検索が使えない環境で、PDF・Office の本文を読もうとしたときの文言。
const NO_SEARCH: &str =
    "この環境では社内文書の検索が使えないため、PDF や Word の本文を読めません。";

/// 取り込めない種類のときの文言。
const UNSUPPORTED: &str = "このファイルの種類は社内文書の検索に取り込めないため、本文を読めません。読めるのは PDF、Word（.docx）、Excel（.xlsx）、PowerPoint（.pptx）です。";

/// 取り込みに失敗したときの文言（取り込み直せる・D-126）。
const FAILED: &str = "このファイルは社内文書の検索に取り込めなかったため、本文を読めません。ドライブのメニューで「検索に取り込み直す」を選んでください。もう一度取り込めないときは、ファイルが壊れていないか確かめて、保存し直してください。";

/// 社内文書の検索に取り込まない場所（AI エージェントの作業用のフォルダー・システム領域）にあるときの文言。
const EXCLUDED: &str = "このファイルは AI エージェントの作業用のフォルダーにあり、社内文書の検索に取り込まれないため、本文を読めません。";

/// 取り込みへ向かう記録が無く、待っても取り込まれないときの文言（社内文書の検索を有効にする前に置いたファイルなど・
/// 取り込み直せる・D-126）。
const NOT_INGESTED: &str = "このファイルは社内文書の検索に取り込まれていないため、本文を読めません。ドライブのメニューで「検索に取り込み直す」を選ぶと、取り込まれます。";

/// 読む間にファイルが保存し直され、文章でない種類に変わったときの文言（再開すると、新しい種類の読み方で読み直す）。
const KIND_CHANGED: &str = "読む間にファイルが保存し直されて種類が変わったため、読めませんでした。失敗したところから再開してください。";

/// 取り込んだが本文が無かったときの文言。
const EMPTY: &str = "このファイルからは本文を取り出せませんでした。文字の無い文書や、文字が画像になっていて読み取れない文書は読めません。";

/// 社内文書の検索に一時的につながらなかったときの文言（原文はログへ）。
const SEARCH_UNAVAILABLE: &str =
    "社内文書の検索に一時的につながりませんでした。少し待ってから、もう一度実行してください。";

/// 見えない・無いを言い分けない誤り（付録 D の N3.5.1・存在の秘匿）。自動実行では読める範囲を 2 文目で補う。
fn hidden_file(ec: &ExecCtx) -> PortError {
    let mut message = hidden_not_found("ファイル");
    if ec.principal_kind == "workflow" {
        message.push_str(
            "自動実行では、オンにするときに使えるようにしたファイルとフォルダーの中だけを読めます。",
        );
    }
    PortError::new("not_found", message, false)
}

/// 「ファイルを読む」の読み口（[`workflow_engine::NodePorts::storage_read_document`]）。
pub(super) async fn storage_read_document(
    ports: &ProdNodePorts,
    ec: &ExecCtx,
    file_id: Uuid,
    max_chars: u32,
) -> Result<Value, PortError> {
    let ctx = auth_ctx(ec)?;
    let trace = ec.trace_id.as_deref();
    let hide = |e: StorageError| match e {
        StorageError::Forbidden | StorageError::NotFound => hidden_file(ec),
        other => map_storage(other),
    };
    // 閲覧の判定（post-filter 相当・監査つき）。ここを通った Node だけを読む。
    let meta = ports
        .storage
        .get_metadata(&ctx, file_id, trace)
        .await
        .map_err(hide)?;
    // フォルダーは中身の読み取りに任せる（いまの誤りのまま）。文章のファイルは中身を読む。
    if meta.kind != NodeKind::File || !is_binary(&meta.name, meta.content_type.as_deref()) {
        return read_text_file(&ports.storage, &ctx, file_id, trace, hide, kind_changed).await;
    }
    let Some(search) = ports.search.as_ref() else {
        return Err(PortError::new(SEARCH_DISABLED, NO_SEARCH, false));
    };
    let unavailable = |e: rag::RagError| {
        tracing::warn!(target: "workflow.storage_read", error = %e, "社内文書の検索の索引を読めなかった");
        PortError::unavailable(SEARCH_UNAVAILABLE)
    };
    match search
        .index_state(&ctx, &meta)
        .await
        .map_err(unavailable)?
    {
        rag::IndexState::Ready => {}
        rag::IndexState::Pending => return Err(pending()),
        rag::IndexState::Unsupported => {
            return Err(PortError::new(NOT_INDEXABLE, UNSUPPORTED, false))
        }
        rag::IndexState::TooLarge { limit_bytes } => {
            return Err(PortError::new(
                NOT_INDEXABLE,
                format!(
                    "ファイルが大きすぎて社内文書の検索に取り込まれないため、本文を読めません（上限 {} MB）。",
                    limit_bytes / 1024 / 1024
                ),
                false,
            ))
        }
        rag::IndexState::Failed => return Err(PortError::new(INDEX_FAILED, FAILED, false)),
        rag::IndexState::Empty => return Err(PortError::new(NOT_INDEXABLE, EMPTY, false)),
        rag::IndexState::Excluded => return Err(PortError::new(NOT_INDEXABLE, EXCLUDED, false)),
        // 待っても取り込まれない（やり直さない）。取り込み直せる（D-126）ので、取り込みに失敗したものと同じ code。
        rag::IndexState::NotIngested => {
            return Err(PortError::new(INDEX_FAILED, NOT_INGESTED, false))
        }
    }
    let read = search
        .read_document_text(
            &ctx,
            &meta,
            usize::try_from(max_chars).unwrap_or(usize::MAX),
            trace,
        )
        .await
        .map_err(unavailable)?;
    let page = match read {
        rag::DocRead::Page(page) => page,
        // 二段 authz の pre-filter で読めない（viewer 判定と食い違った）。見えないと同じ誤りにする。
        rag::DocRead::Denied => return Err(hidden_file(ec)),
        // 状態を確かめた後に取り込み直しと重なった（索引を作り直している）。待てば読める。
        rag::DocRead::NotIndexed | rag::DocRead::PositionNotFound { .. } => return Err(pending()),
    };
    Ok(document_output(&meta, &page))
}

/// 取り込みが終わっていない誤り（やり直せる・エンジンは試す回数を使わずに待って読み直す・D-125）。
fn pending() -> PortError {
    PortError::new(
        INDEX_PENDING,
        format!(
            "このファイルは、社内文書の検索への取り込みが {INDEX_WAIT_MAX_MINUTES} 分待っても終わらなかったため、本文を読めませんでした。取り込みが終わってから、失敗したところから再開してください。"
        ),
        true,
    )
}

/// 索引から読んだ本文の結果（文章のファイルの結果と同じ項目＋切ったか。encoding は無い）。
fn document_output(meta: &Node, page: &rag::DocReadPage) -> Value {
    json!({
        "id": meta.id.to_string(),
        "name": meta.name,
        // 読んだ本文のバージョン（名前の変更でファイルのバージョンが進んでも、本文は取り込んだ内容のもの）。
        "version": page.version,
        "content_type": meta.content_type,
        "size": meta.size_bytes,
        "text": page.text.trim_end(),
        "truncated": page.next.is_some(),
    })
}

/// 読む間にファイルの種類が文章でないものに変わったときの誤り（「ファイルを読む」・やり直さない・再開で読み直す）。
fn kind_changed() -> PortError {
    PortError::new(NOT_TEXT_FILE, KIND_CHANGED, false)
}

/// 閲覧を確かめたファイルの中身を、文章として読む（storage.read の文章の経路・BUG-101・GAP-45）。
///
/// 文章でない種類（読んだ後の種類で決め直す）・文字コードを決められない中身は [`NOT_TEXT_FILE`] で断る（文字化けした
/// 中身を成功として渡さない）。`map_err` は storage の誤りの写し方（ノードは存在を秘匿する・script はいまのまま）、
/// `not_text` は読んだ後の種類が文章でなかったときの誤り（読み手ごとの文言・BUG-158）。
pub(super) async fn read_text_file(
    storage: &StorageService,
    ctx: &authz::AuthContext,
    file_id: Uuid,
    trace: Option<&str>,
    map_err: impl Fn(StorageError) -> PortError,
    not_text: fn() -> PortError,
) -> Result<Value, PortError> {
    let (node, bytes) = storage
        .read_file_internal(ctx, file_id, trace)
        .await
        .map_err(map_err)?;
    if is_binary(&node.name, node.content_type.as_deref()) {
        return Err(not_text());
    }
    // 文字コードを決めて読めない中身を、文字化けのまま成功させない（BUG-101・GAP-45）。
    let decoded = read_text(&bytes).ok_or_else(not_readable_text)?;
    // version は読んだ中身のバージョン（後ろの「CSV を編集」の前提にできる・D-96）。file_id はエンジンが足す。
    // encoding は読んだ文字コード（UTF-8・Shift_JIS など。読み違いを実行履歴で確かめられる）。
    Ok(json!({
        "id": node.id.to_string(),
        "name": node.name,
        "version": node.version,
        "content_type": node.content_type,
        "size": node.size_bytes,
        "encoding": decoded.encoding.name(),
        "text": decoded.text,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ec(kind: &str) -> ExecCtx {
        ExecCtx {
            tenant_id: "t".into(),
            org: "o".into(),
            principal: "p".into(),
            principal_kind: kind.into(),
            trace_id: None,
            run_id: None,
            step_path: None,
        }
    }

    /// 見えない・無いは同じ決まった文。自動実行では、読める範囲を 2 文目で補う（付録 D の N3.5.1）。
    #[test]
    fn hidden_file_says_the_fixed_sentence() {
        let user = hidden_file(&ec("user"));
        assert_eq!((user.code.as_str(), user.retryable), ("not_found", false));
        assert_eq!(
            user.message,
            "ファイルが見つかりません。削除されたか、見る権限がありません。"
        );
        let auto = hidden_file(&ec("workflow"));
        assert!(auto.message.starts_with(&user.message));
        assert!(auto.message.ends_with("フォルダーの中だけを読めます。"));
        assert!(pending().retryable);
        assert_eq!(pending().code, INDEX_PENDING);
        // 待つ上限を過ぎたときに残る文（試す回数を増やすよう言わない・D-125）。
        let waited = format!("{INDEX_WAIT_MAX_MINUTES} 分待っても");
        assert!(pending().message.contains(&waited), "{}", pending().message);
        assert!(
            !pending().message.contains("試す回数"),
            "{}",
            pending().message
        );
    }
}
