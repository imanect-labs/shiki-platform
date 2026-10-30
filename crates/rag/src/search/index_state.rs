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
use sqlx::PgConnection;
use uuid::Uuid;

use crate::config::RagConfig;
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
    /// 取り込みに失敗した（やり直しを使い切った）。取り込み直せる（[`SearchService::request_reindex`]）。
    Failed,
    /// 取り込んだが、本文が無かった（文字の無い文書など）。
    Empty,
    /// 社内文書の検索に取り込まない場所（システム領域・`node.system`。AI エージェントの作業用のフォルダー）にある。
    Excluded,
    /// 取り込みへ向かう記録が無く、待っても取り込まれない（取り込みを有効にする前に置いた・取り込みを飛ばした）。
    /// 取り込み直す（[`SearchService::request_reindex`]・D-126）か、内容を保存し直す（新しいバージョンにする）と
    /// 取り込まれる。
    NotIngested,
}

/// 内容の取り込みへ向かう op（outbox・待ち行列・取り込みの記録で同じ。取り込み直しの `reindex` を含む・D-126）。
const CONTENT_OPS: &str = "('create', 'update', 'restore', 'reindex')";

/// 状態を決める材料（1 本の SQL で、何件でもまとめて引く）。
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub(crate) struct Facts {
    pub(crate) id: Uuid,
    /// いまの内容のバージョン。
    pub(crate) content: i64,
    /// 索引の最新のバージョン（無ければ `None`）。
    pub(crate) indexed: Option<i64>,
    /// システム領域（取り込まない場所）か。
    pub(crate) system: bool,
    /// 取り込みへ向かう内容のイベントが outbox か待ち行列にあるか。
    pub(crate) queued: bool,
    /// いまの内容より後の取り込みの記録のうち、いちばん新しいもの（op と status）。
    pub(crate) latest_op: Option<String>,
    pub(crate) latest_status: Option<String>,
}

/// [`facts_on`] の SQL（`storage::CONTENT_VERSION_SQL` を差し込む）。
///
/// 待ち行列は、テナントの取り込みのジョブを 1 回だけ読んで突き合わせる（ファイルごとに待ち行列を読み直さない・
/// 取り込み直しのまとめての走査は 1 回に数百件を引く）。
static FACTS_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "with cur as ( \
             select node.id, node.system, {cv} as version from node \
             where node.id = any($3) and node.tenant_id = $1 and node.org = $2), \
         outbox as materialized ( \
             select distinct o.node_id from storage_event_outbox o \
             where o.tenant_id = $1 and o.processed_at is null and o.node_id = any($3) \
               and o.op in {ops}), \
         jobs as materialized ( \
             select distinct q.payload->>'node_id' as node_id from job_queue q \
             where q.tenant_id = $1 and q.queue = $4 and q.payload->>'op' in {ops}) \
         select cur.id, cur.version as content, cur.system, \
                (select max(c.version) from rag_chunk c \
                 where c.tenant_id = $1 and c.org = $2 and c.node_id = cur.id) as indexed, \
                (cur.id in (select node_id from outbox) \
                 or cur.id::text in (select node_id from jobs)) as queued, \
                j.op as latest_op, j.status as latest_status \
         from cur \
         left join lateral (select j.op, j.status from rag_ingest_job j \
                            where j.tenant_id = $1 and j.org = $2 and j.node_id = cur.id \
                              and j.version >= cur.version \
                              and (j.op in {ops} or j.op = 'delete') \
                            order by j.version desc, j.updated_at desc limit 1) j on true \
         order by cur.id",
        cv = storage::CONTENT_VERSION_SQL,
        ops = CONTENT_OPS,
    )
});

/// `ids` のノード（`ctx` の tenant・org のもの）の状態の材料を引く（`conn` は呼び出し側の接続か Tx）。
///
/// 別の tenant・org のノードと無いノードは返らない。
pub(crate) async fn facts_on(
    conn: &mut PgConnection,
    tenant_id: &str,
    org: &str,
    ids: &[Uuid],
) -> Result<Vec<Facts>, RagError> {
    Ok(sqlx::query_as(FACTS_SQL.as_str())
        .bind(tenant_id)
        .bind(org)
        .bind(ids)
        .bind(RAG_INGEST_QUEUE)
        .fetch_all(conn)
        .await?)
}

/// 中身を見る前に決まる状態（取り込めない種類・大きすぎる）。どちらでもなければ `None`。
pub(crate) fn precheck(
    config: &RagConfig,
    content_type: Option<&str>,
    name: &str,
    size_bytes: Option<i64>,
) -> Option<IndexState> {
    if parsable_type(content_type, name).is_none() {
        return Some(IndexState::Unsupported);
    }
    (size_bytes.unwrap_or(0) > config.max_parse_bytes).then_some(IndexState::TooLarge {
        limit_bytes: config.max_parse_bytes,
    })
}

/// `node`（呼び出し側が閲覧を確かめたもの）の状態と、いまの内容のバージョン（`conn` の上で引く）。
///
/// 別の org の文脈ではノードが見えない（呼び出し側の閲覧の判定と食い違う）。読めるとは言わず `Pending`。
pub(crate) async fn state_on(
    conn: &mut PgConnection,
    config: &RagConfig,
    ctx: &AuthContext,
    node: &storage::Node,
) -> Result<(IndexState, Option<i64>), RagError> {
    if let Some(state) = precheck(
        config,
        node.content_type.as_deref(),
        &node.name,
        node.size_bytes,
    ) {
        return Ok((state, None));
    }
    let facts = facts_on(conn, &ctx.tenant_id, &ctx.org, &[node.id]).await?;
    Ok(facts.first().map_or((IndexState::Pending, None), |f| {
        (classify(f), Some(f.content))
    }))
}

impl SearchService {
    /// `node`（呼び出し側が閲覧を確かめたもの）の本文が、いまの内容のまま索引にあるか。
    pub async fn index_state(
        &self,
        ctx: &AuthContext,
        node: &storage::Node,
    ) -> Result<IndexState, RagError> {
        let mut conn = self.pool.acquire().await?;
        Ok(state_on(&mut conn, &self.config, ctx, node).await?.0)
    }
}

/// 状態を決める。
pub(crate) fn classify(f: &Facts) -> IndexState {
    if f.indexed.is_some_and(|v| v >= f.content) {
        return IndexState::Ready;
    }
    // システム領域の書き込みは取り込みへ積まない（relay）。待っても読めない。
    if f.system {
        return IndexState::Excluded;
    }
    // 内容のイベントが取り込みへ向かっている（ごみ箱から戻した直後・新しい内容を保存した直後・取り込み直しを
    // 頼んだ直後など）。
    if f.queued {
        return IndexState::Pending;
    }
    match (f.latest_op.as_deref(), f.latest_status.as_deref()) {
        // 取り込みへ向かう記録がどこにも無い（取り込みを有効にする前に置いた・outbox の掃除で消えた）。
        (None, _) => IndexState::NotIngested,
        // ごみ箱に入れて索引を消した後で戻した（フォルダーごと戻すと、ファイルごとの取り込みは展開を待つ）。
        (Some("delete"), _) => IndexState::Pending,
        // DLQ へ移した（やり直しを使い切った・恒久的な失敗）。取り込み直し（D-126）を飛ばした（中身が見つからない
        // など）も同じ: 待っても取り込まれない。「記録が無い」と言うと、取り込み直すたびに「取り込みに回した」と答えて
        // 何も起きないので、失敗として言う（取り込み直すと、もう一度試す・`begin_job`）。
        (_, Some("dead")) | (Some("reindex"), Some("skipped")) => IndexState::Failed,
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
            id: Uuid::nil(),
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
        // 取り込み直しを飛ばした（中身が見つからないなど）は失敗（取り込み直すと、もう一度試す）。
        assert_eq!(
            c(facts(1, None, Some(("reindex", "skipped")))),
            IndexState::Failed
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
