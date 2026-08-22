//! StorageService: 中断アップロードの回収（#468）。
//!
//! 二相アップロード（declare → presigned PUT → finalize）は、finalize が最後まで走れば
//! staging/incoming を自分で片付ける（`finalize.rs` の best-effort delete）。しかしその
//! 後始末は **ハンドラの future が drop されずに戻ってくる場合にしか実行されない**。
//! クライアント切断・画面遷移でリクエストが中断されると future はその場で落ち、
//! `pending_upload` 行も staging/incoming オブジェクトも残る。中断は異常系ではなく、
//! アップロード直後にタブを閉じる程度の通常操作で起きるため、放置すると
//! テナントごとにバイトが単調増加し、ユーザーからは見えず消す手段も無い。
//!
//! ここでは 2 つの sweep を提供する。**片方だけでは別の取り残し方をするため必ず対で回す。**
//!
//! - [`StorageService::sweep_expired_pending_uploads`] — TTL を過ぎた `pending_upload` 行を
//!   claim して消し、対応する staging/incoming を削除する。
//! - [`StorageService::sweep_orphan_upload_objects`] — `pending_upload` に対応行が無い
//!   `staging/` `incoming/` キーを削除する。行が先に消えた（finalize 成功後に delete が
//!   失敗した・プロセスが落ちた）ケースを回収する。
//!
//! ## 孤児判定の安全性
//!
//! 「`pending_upload` に行が無い ＝ 消してよい」と判断できるのは、finalize が **incoming →
//! final のコピーを終えた後にしか行を claim しない**ため（`finalize.rs` の新規作成パス・
//! `finalize_content_update` のどちらも、claim は txn の中で最後に行われ、その時点で
//! staging も incoming も不要になっている）。claim は commit までは他トランザクションから
//! 見えないので、finalize 実行中の行は sweep からは「まだ在る」ように見え、巻き添えにしない。
//!
//! 逆順（オブジェクトを消してから行を消す）にはしない。行を claim してから消せば、以降その
//! upload の finalize は必ず失敗する（claim が 0 行）ので、オブジェクトの所有権は sweep 側に
//! 移る。オブジェクト削除に失敗しても孤児 sweep が後から回収する。

#[allow(clippy::wildcard_imports)]
use super::*;

use crate::content_address::incoming_object_key;

/// TTL 超過で回収した pending_upload 行（オブジェクト削除に必要な分だけ取る）。
#[derive(sqlx::FromRow)]
struct ExpiredUpload {
    upload_id: Uuid,
    tenant_id: String,
    org: String,
    staging_key: String,
}

/// 孤児 sweep が走査する (tenant_id, org) の組。
#[derive(sqlx::FromRow)]
struct TenantOrg {
    tenant_id: String,
    org: String,
}

/// 1 回の孤児 sweep で削除するキーの上限。1 テナントの巨大な取り残しが 1 周を占有して
/// 他テナントの回収を止めないようにする（残りは次周で回収する）。
const ORPHAN_DELETE_LIMIT: usize = 10_000;

impl StorageService {
    /// TTL を過ぎた `pending_upload` を回収する。回収した件数を返す。
    ///
    /// 行を `DELETE ... RETURNING` で claim してからオブジェクトを消す（上のモジュール
    /// コメント参照）。オブジェクト削除は best-effort で、失敗しても行の回収は取り消さない
    /// （取り消すと永久に回収できない。取り残しは孤児 sweep が拾う）。
    pub async fn sweep_expired_pending_uploads(
        &self,
        now: DateTime<Utc>,
        ttl: std::time::Duration,
    ) -> Result<usize, StorageError> {
        let cutoff = now
            - chrono::Duration::from_std(ttl).map_err(|e| {
                StorageError::Invalid(format!("upload_gc の TTL が範囲外です: {e}"))
            })?;

        let expired: Vec<ExpiredUpload> = sqlx::query_as(
            "DELETE FROM pending_upload WHERE created_at < $1 \
             RETURNING upload_id, tenant_id, org, staging_key",
        )
        .bind(cutoff)
        .fetch_all(&self.db)
        .await?;

        if expired.is_empty() {
            return Ok(0);
        }

        // staging は行が持つキーをそのまま使う（declare 時の値が正）。incoming は finalize が
        // 組み立てるキーなので、同じ規則でここでも組み立てる（存在しなければ削除は no-op）。
        let mut keys = Vec::with_capacity(expired.len() * 2);
        for row in &expired {
            keys.push(row.staging_key.clone());
            keys.push(incoming_object_key(
                &row.tenant_id,
                &row.org,
                &row.upload_id.to_string(),
            ));
        }
        if let Err(e) = self.store.delete_batch(&keys).await {
            // 行は既に回収済み。オブジェクトは孤児 sweep が拾うので、ここでは失敗を握らず記録に留める。
            tracing::warn!(
                error = %e,
                count = keys.len(),
                "期限切れアップロードのオブジェクト削除に失敗しました（孤児 sweep で回収します）"
            );
        }

        Ok(expired.len())
    }

    /// `pending_upload` に対応行が無い `staging/` `incoming/` オブジェクトを削除する。
    /// 削除した件数を返す。
    ///
    /// TTL sweep が行ごと回収できなかった取り残し（finalize 成功後に delete が失敗した・
    /// プロセスが落ちた）を回収する最後の網。
    pub async fn sweep_orphan_upload_objects(&self) -> Result<usize, StorageError> {
        // 走査対象の (tenant_id, org)。オブジェクトキーは `{tenant_id}/{org}/...` なので、
        // バケット全走査を避けるために DB 側から組を引く。アップロードが起きた org は
        // node/blob/pending_upload のいずれかに必ず現れる。
        let pairs: Vec<TenantOrg> = sqlx::query_as(
            "SELECT tenant_id, org FROM node \
             UNION SELECT tenant_id, org FROM blob \
             UNION SELECT tenant_id, org FROM pending_upload",
        )
        .fetch_all(&self.db)
        .await?;

        let mut deleted = 0usize;
        for pair in pairs {
            for kind in ["staging", "incoming"] {
                let prefix = format!("{}/{}/{}/", pair.tenant_id, pair.org, kind);
                let n = self.sweep_orphans_under(&prefix).await?;
                deleted += n;
                if deleted >= ORPHAN_DELETE_LIMIT {
                    tracing::info!(
                        deleted,
                        "孤児 sweep が 1 周の上限に達しました（残りは次周で回収します）"
                    );
                    return Ok(deleted);
                }
            }
        }
        Ok(deleted)
    }

    /// 1 つの prefix 配下を 1 ページずつ走査し、孤児キーを削除する。
    async fn sweep_orphans_under(&self, prefix: &str) -> Result<usize, StorageError> {
        let mut deleted = 0usize;
        let mut continuation: Option<String> = None;
        loop {
            let (keys, next) = self
                .store
                .list_prefix(prefix, continuation.as_deref())
                .await?;
            if !keys.is_empty() {
                // キー末尾の upload_id を取り出し、pending_upload に無いものだけ消す。
                // upload_id として解釈できないキーは触らない（想定外の混入を消さない）。
                let mut candidates: Vec<(Uuid, String)> = Vec::with_capacity(keys.len());
                for key in keys {
                    let Some(tail) = key.rsplit('/').next() else {
                        continue;
                    };
                    let Ok(id) = Uuid::parse_str(tail) else {
                        tracing::warn!(key = %key, "upload_id として解釈できないキーを孤児 sweep から除外しました");
                        continue;
                    };
                    candidates.push((id, key));
                }
                if !candidates.is_empty() {
                    let ids: Vec<Uuid> = candidates.iter().map(|(id, _)| *id).collect();
                    // 1 ページ分（最大 1000 件）を 1 クエリで突き合わせる。キーごとに問い合わせない。
                    let live: std::collections::HashSet<Uuid> = sqlx::query_scalar(
                        "SELECT upload_id FROM pending_upload WHERE upload_id = ANY($1)",
                    )
                    .bind(&ids)
                    .fetch_all(&self.db)
                    .await?
                    .into_iter()
                    .collect();
                    let orphans: Vec<String> = candidates
                        .into_iter()
                        .filter(|(id, _)| !live.contains(id))
                        .map(|(_, key)| key)
                        .collect();
                    if !orphans.is_empty() {
                        self.store.delete_batch(&orphans).await?;
                        deleted += orphans.len();
                    }
                }
            }
            match next {
                Some(c) => continuation = Some(c),
                None => break,
            }
        }
        Ok(deleted)
    }
}
