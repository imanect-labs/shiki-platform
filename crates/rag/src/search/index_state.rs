//! 文書の本文が索引にあるか、無いならなぜか（取り込みの状態・D-111）。
//!
//! ワークフローの「ファイルを読む」は、PDF・Office の本文を取り込み済みの索引から読む（原本は読み直さない）。
//! 索引に本文が無いときに、待てば読めるのか（取り込み待ち・取り込み中・一時的な失敗のやり直し待ち）と、待っても
//! 読めないのか（取り込めない種類・大きすぎる・取り込みに失敗した・本文が無かった）を分けるためのもの。
//!
//! 「いまの内容」は、内容のバージョンの履歴（`node_version` の提案でない最新）で決める。名前の変更と移動も
//! `node.version` を進めるが、本文は変わらず索引も作り直さないので、`node.version` と索引のバージョンは比べない。
//! ごみ箱から戻したときは `node.version` で取り込み直すので、索引のバージョンが履歴の最新より大きくなることがある。
//!
//! **認可は呼び出し側が StorageService で済ませた証拠（[`storage::Node`]）を受け取る形にする**（[`super::read`] と
//! 同じ）。ここが返すのは取り込みの状態だけで、本文は返さない。本文は [`SearchService::read_document_text`] が
//! 可読集合の pre-filter を通してから返す。

use authz::AuthContext;

use crate::error::RagError;
use crate::pipeline::indexer::parsable_type;
use crate::search::SearchService;

/// 文書の本文の索引の状態（[`SearchService::index_state`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexState {
    /// いまの内容の本文が索引にある。
    Ready,
    /// 取り込みを待っている・取り込み中・一時的な失敗のやり直しを待っている（待てば読める）。
    Pending,
    /// 取り込めない種類（取り込みの対象の形式でない・種類が分からない）。
    Unsupported,
    /// 取り込みの上限（[`crate::RagConfig::max_parse_bytes`]・`limit_bytes`）より大きい。
    TooLarge { limit_bytes: i64 },
    /// 取り込みに失敗した（やり直しを使い切った・取り込みを飛ばした）。
    Failed,
    /// 取り込んだが、本文が無かった（文字の無い文書など）。
    Empty,
}

impl SearchService {
    /// `node`（呼び出し側が閲覧を確かめたもの）の本文が、いまの内容のまま索引にあるか。
    pub async fn index_state(
        &self,
        ctx: &AuthContext,
        node: &storage::Node,
    ) -> Result<IndexState, RagError> {
        if parsable_type(node.content_type.as_deref()).is_none() {
            return Ok(IndexState::Unsupported);
        }
        if node.size_bytes.unwrap_or(0) > self.config.max_parse_bytes {
            return Ok(IndexState::TooLarge {
                limit_bytes: self.config.max_parse_bytes,
            });
        }
        // いまの内容のバージョン（履歴が無ければ node.version）・索引の最新のバージョン・いまの内容より後の
        // 取り込みの結果（新しいバージョンの順）を 1 本の SQL で引く。
        let (content, indexed, status): (i64, Option<i64>, Option<String>) = sqlx::query_as(
            "with cur as ( \
                 select coalesce((select max(v.version) from node_version v \
                                  where v.node_id = $3 and v.tenant_id = $1 and v.org = $2 \
                                    and not v.is_proposal and v.version <= $4), $4) as version) \
             select cur.version, \
                    (select max(c.version) from rag_chunk c \
                     where c.tenant_id = $1 and c.org = $2 and c.node_id = $3), \
                    (select j.status from rag_ingest_job j \
                     where j.tenant_id = $1 and j.org = $2 and j.node_id = $3 \
                       and j.version >= cur.version and j.op in ('create', 'update', 'restore') \
                     order by j.version desc, j.updated_at desc limit 1) \
             from cur",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(node.id)
        .bind(node.version)
        .fetch_one(&self.pool)
        .await?;
        Ok(classify(content, indexed, status.as_deref()))
    }
}

/// 状態を決める（いまの内容のバージョン・索引の最新のバージョン・いまの内容の取り込みの結果から）。
fn classify(content: i64, indexed: Option<i64>, status: Option<&str>) -> IndexState {
    if indexed.is_some_and(|v| v >= content) {
        return IndexState::Ready;
    }
    match status {
        // DLQ へ移した（やり直しを使い切った・恒久的な失敗）。取り込みを飛ばしたものも、待っても読めない。
        Some("dead" | "skipped") => IndexState::Failed,
        // 取り込みは終わったのに、いまの内容の本文が索引に無い（本文の無い文書。古い版の索引も消してある）。
        Some("succeeded") if indexed.is_none() => IndexState::Empty,
        // 取り込みの結果がまだ無い（届いていない）・取り込み中・一時的な失敗のやり直し待ち。
        _ => IndexState::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_content_version_index_and_job() {
        // 名前の変更で node.version が進んでも、内容のバージョンの索引があれば読める。
        assert_eq!(classify(3, Some(3), None), IndexState::Ready);
        // ごみ箱から戻すと node.version で取り込み直す（履歴の最新より大きい）。
        assert_eq!(classify(3, Some(5), Some("succeeded")), IndexState::Ready);
        // 新しい内容の取り込みを待っている間は、古い内容を読まない。
        assert_eq!(classify(4, Some(3), None), IndexState::Pending);
        assert_eq!(classify(4, Some(3), Some("running")), IndexState::Pending);
        assert_eq!(classify(4, Some(3), Some("failed")), IndexState::Pending);
        assert_eq!(classify(1, None, None), IndexState::Pending);
        assert_eq!(classify(4, Some(3), Some("dead")), IndexState::Failed);
        assert_eq!(classify(1, None, Some("skipped")), IndexState::Failed);
        assert_eq!(classify(1, None, Some("succeeded")), IndexState::Empty);
        // 取り込みが終わった直後で索引の読み取りと重なったときは、もう一度読めば読める。
        assert_eq!(classify(4, Some(3), Some("succeeded")), IndexState::Pending);
    }
}
