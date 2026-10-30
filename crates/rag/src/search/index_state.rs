//! 文書の本文が索引にあるか、無いならなぜか（取り込みの状態・D-111）。
//!
//! ワークフローの「ファイルを読む」は、PDF・Office の本文を取り込み済みの索引から読む（原本は読み直さない）。
//! 索引に本文が無いときに、待てば読めるのか（取り込み待ち・取り込み中・一時的な失敗のやり直し待ち）と、待っても
//! 読めないのか（取り込めない種類・大きすぎる・取り込みに失敗した・本文が無かった・取り込まれない場所にある・
//! 取り込みが届いていない）を分けるためのもの。
//!
//! 「いまの内容」は、内容のバージョンの履歴（`node_version` の提案でない最新・[`storage::CONTENT_VERSION_SQL`]）で
//! 決める。名前の変更と移動も `node.version` を進めるが、本文は変わらず索引も作り直さないので、`node.version` と
//! 索引のバージョンは比べない（取り込みの側も同じ値で古いジョブを飛ばす）。ごみ箱から戻したときは `node.version`
//! で取り込み直すので、索引のバージョンが履歴の最新より大きくなることがある。
//!
//! 「待てば読める」は、取り込みへ向かう記録がどこかにあるときだけ言う。書き込みと同じ Tx で outbox に積まれた
//! イベント（未処理）・取り込みの待ち行列（`job_queue`）・取り込みの記録（`rag_ingest_job`）のどれも無いファイルは、
//! 待っても取り込まれない（システム領域は relay が積まない。社内文書の検索を有効にする前に置いたファイルは、
//! outbox の掃除で消えている）。
//!
//! **認可は呼び出し側が StorageService で済ませた証拠（[`storage::Node`]）を受け取る形にする**（[`super::read`] と
//! 同じ）。ここが返すのは取り込みの状態だけで、本文は返さない。本文は [`SearchService::read_document_text`] が
//! 可読集合の pre-filter を通してから返す。

use std::sync::LazyLock;

use authz::AuthContext;

use crate::error::RagError;
use crate::pipeline::indexer::parsable_type;
use crate::pipeline::RAG_INGEST_QUEUE;
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
    /// 取り込みに失敗した（やり直しを使い切った）。
    Failed,
    /// 取り込んだが、本文が無かった（文字の無い文書など）。
    Empty,
    /// 社内文書の検索に取り込まない場所（システム領域・`node.system`。AI エージェントの作業用のフォルダー）にある。
    Excluded,
    /// 取り込みへ向かう記録が無く、待っても取り込まれない（取り込みを有効にする前に置いた・取り込みを飛ばした）。
    /// 内容を保存し直す（新しいバージョンにする）と取り込まれる。
    NotIngested,
}

/// 状態を決める材料（1 本の SQL で引く）。
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct Facts {
    /// いまの内容のバージョン。
    content: i64,
    /// 索引の最新のバージョン（無ければ `None`）。
    indexed: Option<i64>,
    /// システム領域（取り込まない場所）か。
    system: bool,
    /// 取り込みへ向かう内容のイベントが outbox か待ち行列にあるか。
    queued: bool,
    /// いまの内容より後の取り込みの記録のうち、いちばん新しいもの（op と status）。
    latest_op: Option<String>,
    latest_status: Option<String>,
}

/// [`SearchService::index_state`] の SQL（`storage::CONTENT_VERSION_SQL` を差し込む）。
static FACTS_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "with cur as ( \
             select node.id, node.system, {cv} as version from node \
             where node.id = $3 and node.tenant_id = $1 and node.org = $2) \
         select cur.version as content, cur.system, \
                (select max(c.version) from rag_chunk c \
                 where c.tenant_id = $1 and c.org = $2 and c.node_id = cur.id) as indexed, \
                (exists (select 1 from storage_event_outbox o \
                         where o.tenant_id = $1 and o.node_id = cur.id and o.processed_at is null \
                           and o.op in ('create', 'update', 'restore')) \
                 or exists (select 1 from job_queue q \
                            where q.tenant_id = $1 and q.queue = $4 \
                              and q.payload->>'node_id' = cur.id::text \
                              and q.payload->>'op' in ('create', 'update', 'restore'))) as queued, \
                j.op as latest_op, j.status as latest_status \
         from cur \
         left join lateral (select j.op, j.status from rag_ingest_job j \
                            where j.tenant_id = $1 and j.org = $2 and j.node_id = cur.id \
                              and j.version >= cur.version \
                              and j.op in ('create', 'update', 'restore', 'delete') \
                            order by j.version desc, j.updated_at desc limit 1) j on true",
        cv = storage::CONTENT_VERSION_SQL,
    )
});

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
        let facts: Option<Facts> = sqlx::query_as(FACTS_SQL.as_str())
            .bind(&ctx.tenant_id)
            .bind(&ctx.org)
            .bind(node.id)
            .bind(RAG_INGEST_QUEUE)
            .fetch_optional(&self.pool)
            .await?;
        // 別の org の文脈ではノードが見えない（呼び出し側の閲覧の判定と食い違う）。読めるとは言わない。
        Ok(facts.map_or(IndexState::Pending, |f| classify(&f)))
    }
}

/// 状態を決める。
fn classify(f: &Facts) -> IndexState {
    if f.indexed.is_some_and(|v| v >= f.content) {
        return IndexState::Ready;
    }
    // システム領域の書き込みは取り込みへ積まない（relay）。待っても読めない。
    if f.system {
        return IndexState::Excluded;
    }
    // 内容のイベントが取り込みへ向かっている（ごみ箱から戻した直後・新しい内容を保存した直後など）。
    if f.queued {
        return IndexState::Pending;
    }
    match (f.latest_op.as_deref(), f.latest_status.as_deref()) {
        // 取り込みへ向かう記録がどこにも無い（取り込みを有効にする前に置いた・outbox の掃除で消えた）。
        (None, _) => IndexState::NotIngested,
        // ごみ箱に入れて索引を消した後で戻した（フォルダーごと戻すと、ファイルごとの取り込みは展開を待つ）。
        (Some("delete"), _) => IndexState::Pending,
        // DLQ へ移した（やり直しを使い切った・恒久的な失敗）。
        (_, Some("dead")) => IndexState::Failed,
        // 取り込みを飛ばした（前の版で、取り込みより先に名前を変えたファイルなど）。新しい内容の取り込みは来ない。
        (_, Some("skipped")) => IndexState::NotIngested,
        // 取り込みは終わったのに、いまの内容の本文が索引に無い（本文の無い文書。古い版の索引も消してある）。
        (_, Some("succeeded")) if f.indexed.is_none() => IndexState::Empty,
        // 取り込み中・一時的な失敗のやり直し待ち・取り込みが終わった直後で索引の読み取りと重なった。
        _ => IndexState::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(content: i64, indexed: Option<i64>, latest: Option<(&str, &str)>) -> Facts {
        Facts {
            content,
            indexed,
            system: false,
            queued: false,
            latest_op: latest.map(|(op, _)| op.to_string()),
            latest_status: latest.map(|(_, st)| st.to_string()),
        }
    }

    #[test]
    fn classifies_by_content_version_index_and_job() {
        let c = |f: Facts| classify(&f);
        // 名前の変更で node.version が進んでも、内容のバージョンの索引があれば読める。
        assert_eq!(c(facts(3, Some(3), None)), IndexState::Ready);
        // ごみ箱から戻すと node.version で取り込み直す（履歴の最新より大きい）。
        assert_eq!(
            c(facts(3, Some(5), Some(("restore", "succeeded")))),
            IndexState::Ready
        );
        // 新しい内容の取り込みを待っている間は、古い内容を読まない。
        let queued = Facts {
            queued: true,
            ..facts(4, Some(3), None)
        };
        assert_eq!(c(queued), IndexState::Pending);
        assert_eq!(
            c(facts(4, Some(3), Some(("update", "running")))),
            IndexState::Pending
        );
        assert_eq!(
            c(facts(4, Some(3), Some(("update", "failed")))),
            IndexState::Pending
        );
        assert_eq!(
            c(facts(4, Some(3), Some(("update", "dead")))),
            IndexState::Failed
        );
        assert_eq!(
            c(facts(1, None, Some(("create", "succeeded")))),
            IndexState::Empty
        );
        // 取り込みが終わった直後で索引の読み取りと重なったときは、もう一度読めば読める。
        assert_eq!(
            c(facts(4, Some(3), Some(("update", "succeeded")))),
            IndexState::Pending
        );
        // 取り込みへ向かう記録が無い・取り込みを飛ばしたものは、待っても取り込まれない。
        assert_eq!(c(facts(1, None, None)), IndexState::NotIngested);
        assert_eq!(
            c(facts(1, None, Some(("create", "skipped")))),
            IndexState::NotIngested
        );
        // ごみ箱に入れて索引を消した後で戻した（戻した取り込みがまだ記録に無い）: 本文が無いとは言わない。
        assert_eq!(
            c(facts(4, None, Some(("delete", "succeeded")))),
            IndexState::Pending
        );
        // システム領域は取り込まない（記録が無くても、待てば読めるとは言わない）。索引があれば読める。
        let system = Facts {
            system: true,
            ..facts(1, None, None)
        };
        assert_eq!(c(system.clone()), IndexState::Excluded);
        assert_eq!(
            c(Facts {
                indexed: Some(1),
                ..system
            }),
            IndexState::Ready
        );
    }
}
