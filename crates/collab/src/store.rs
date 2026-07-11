//! Yjs update log ＋ snapshot の Postgres 永続化（PIT-37①）。
//!
//! 真実は「snapshot ＋ それ以降の update 列」。update が [`COMPACT_EVERY`] 件たまるたびに
//! 全状態を snapshot（yrs update v1 の merge 済み 1 update）へ圧縮し、取り込み済みの
//! update 行を削除する。これにより log は無限肥大せず、ロードは snapshot 1 行＋高々
//! `COMPACT_EVERY` 件の適用で済む。

use uuid::Uuid;

use crate::error::CollabError;

/// snapshot 圧縮を発火させる未圧縮 update 件数のしきい値。
///
/// 1 update は概ねキーストローク単位（数十〜数百バイト）。64 件ごとの圧縮で
/// ロード時の適用件数と snapshot 書込頻度のバランスを取る（PIT-37 の「決めること」）。
pub const COMPACT_EVERY: i64 = 64;

/// ロードしたドキュメントの永続状態。
#[derive(Debug)]
pub struct PersistedDoc {
    /// 全状態 snapshot（yrs update v1）。None は snapshot 未作成。
    pub snapshot: Option<Vec<u8>>,
    /// snapshot に取り込み済みの最終 seq。
    pub snapshot_seq: i64,
    /// 次に発番する update seq。
    pub next_seq: i64,
    /// snapshot 以降の update 列（seq 昇順）。
    pub updates: Vec<Vec<u8>>,
    /// md シリアライズ保存で反映済みの node.version（Task 11P.2）。
    pub saved_node_version: Option<i64>,
}

/// collab_doc / collab_update への永続化ゲートウェイ。
///
/// authz はここでは行わない（呼び出し側の [`crate::hub::CollabHub`] が接続時＋定期の
/// relation チェックを済ませてから到達する）。tenant_id/org は行に焼き込み隔離境界を保つ。
#[derive(Clone)]
pub struct DocStore {
    pool: sqlx::PgPool,
}

impl DocStore {
    pub fn new(pool: sqlx::PgPool) -> Self {
        DocStore { pool }
    }

    /// ドキュメント行を確保（無ければ作成）してロードする。
    pub async fn load_or_init(
        &self,
        node_id: Uuid,
        org: &str,
        tenant_id: &str,
    ) -> Result<PersistedDoc, CollabError> {
        sqlx::query(
            "INSERT INTO collab_doc (node_id, org, tenant_id) VALUES ($1, $2, $3)
             ON CONFLICT (node_id) DO NOTHING",
        )
        .bind(node_id)
        .bind(org)
        .bind(tenant_id)
        .execute(&self.pool)
        .await?;
        self.load(node_id, tenant_id).await
    }

    /// 永続状態をロードする（行が無ければ NotFound）。
    ///
    /// snapshot 行取得と「snapshot 以降の update 列」取得の 2 クエリは **REPEATABLE READ の
    /// 単一トランザクション**で撮る。並行 `compact` が 2 クエリの間に snapshot_seq を進めて
    /// update 行を削除しても、トランザクション開始時点の一貫スナップショットを見るため
    /// 「新 snapshot は読まず、畳まれた update も取りこぼす」欠落状態を作らない。
    pub async fn load(&self, node_id: Uuid, tenant_id: &str) -> Result<PersistedDoc, CollabError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await?;
        type DocRow = (Option<Vec<u8>>, i64, i64, Option<i64>);
        let row: Option<DocRow> = sqlx::query_as(
            "SELECT snapshot, snapshot_seq, next_seq, saved_node_version
             FROM collab_doc WHERE node_id = $1 AND tenant_id = $2",
        )
        .bind(node_id)
        .bind(tenant_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((snapshot, snapshot_seq, next_seq, saved_node_version)) = row else {
            return Err(CollabError::NotFound(format!("collab_doc {node_id}")));
        };
        let updates: Vec<(Vec<u8>,)> = sqlx::query_as(
            "SELECT payload FROM collab_update WHERE node_id = $1 AND seq > $2 ORDER BY seq",
        )
        .bind(node_id)
        .bind(snapshot_seq)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(PersistedDoc {
            snapshot,
            snapshot_seq,
            next_seq,
            updates: updates.into_iter().map(|(p,)| p).collect(),
            saved_node_version,
        })
    }

    /// update を 1 件追記し、発番した seq を返す（seq 発番と追記を単一 txn で直列化）。
    pub async fn append_update(
        &self,
        node_id: Uuid,
        payload: &[u8],
        author: &str,
    ) -> Result<i64, CollabError> {
        let mut tx = self.pool.begin().await?;
        let (seq,): (i64,) = sqlx::query_as(
            "UPDATE collab_doc SET next_seq = next_seq + 1, updated_at = now()
             WHERE node_id = $1 RETURNING next_seq - 1",
        )
        .bind(node_id)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO collab_update (node_id, seq, payload, author) VALUES ($1, $2, $3, $4)",
        )
        .bind(node_id)
        .bind(seq)
        .bind(payload)
        .bind(author)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(seq)
    }

    /// `upto_seq` までを snapshot に圧縮し、取り込み済み update 行を削除する。
    pub async fn compact(
        &self,
        node_id: Uuid,
        snapshot: &[u8],
        upto_seq: i64,
    ) -> Result<(), CollabError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE collab_doc SET snapshot = $2, snapshot_seq = $3, updated_at = now()
             WHERE node_id = $1 AND snapshot_seq < $3",
        )
        .bind(node_id)
        .bind(snapshot)
        .bind(upto_seq)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM collab_update WHERE node_id = $1 AND seq <= $2")
            .bind(node_id)
            .bind(upto_seq)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// snapshot を**無条件に**差し替え、`upto_seq` までの update を削除する（md インポート用）。
    ///
    /// [`Self::compact`] は `snapshot_seq < upto_seq` のときだけ snapshot を書くが、インポートは
    /// seq が進まなくても内容（本文）が変わるため無条件に書く必要がある。`upto_seq` は呼び出し元が
    /// 確定値で渡す（DB の `next_seq` を再計算しない）。ロード時（＝共有前・並行 append 無し）にのみ
    /// 呼ばれる前提。
    pub async fn overwrite_snapshot(
        &self,
        node_id: Uuid,
        snapshot: &[u8],
        upto_seq: i64,
    ) -> Result<(), CollabError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE collab_doc SET snapshot = $2, snapshot_seq = $3, updated_at = now()
             WHERE node_id = $1",
        )
        .bind(node_id)
        .bind(snapshot)
        .bind(upto_seq)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM collab_update WHERE node_id = $1 AND seq <= $2")
            .bind(node_id)
            .bind(upto_seq)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// md 保存済みの node.version を記録する（外部書込検出・Task 11P.2）。
    pub async fn set_saved_node_version(
        &self,
        node_id: Uuid,
        version: i64,
    ) -> Result<(), CollabError> {
        sqlx::query(
            "UPDATE collab_doc SET saved_node_version = $2, updated_at = now() WHERE node_id = $1",
        )
        .bind(node_id)
        .bind(version)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 未圧縮 update の件数（圧縮判断・テスト検証用）。
    pub async fn pending_update_count(&self, node_id: Uuid) -> Result<i64, CollabError> {
        let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM collab_update WHERE node_id = $1")
            .bind(node_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(n)
    }
}
