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
//! 「`pending_upload` に行が無い ＝ 消してよい」と判断できるのは、finalize が
//! **incoming → final のコピーを終えてからでないと行を claim しない**ためである。
//! 新規作成パス・`finalize_content_update` のどちらも、コピーは txn に入る**前**に
//! 済ませ（`finalize.rs` の `store.copy(&incoming_key, &final_key)`）、claim はその後の
//! txn で行う。dedup で既存 blob に相乗りする場合はコピー自体が不要で、final のバイトは
//! 既にストアに在る。いずれの経路でも claim 時点では staging も incoming も不要になっている。
//! claim は commit までは他トランザクションから見えないので、finalize 実行中の行は sweep
//! からは「まだ在る」ように見え、巻き添えにしない。
//!
//! ⚠️ この安全性が依存しているのは **「final のバイトが確定してから claim する」順序**だけで、
//! claim が txn の何番目かではない（`finalize_content_update` は claim を txn の先頭で行う）。
//! `finalize.rs` でコピーを txn 内へ動かす・`blob_exists` 短絡の位置を変えるといった改変を
//! するときは、この順序を保つこと。壊すと、final へ渡る前の incoming を sweep が消して
//! アップロード内容を失う。
//!
//! 逆順（オブジェクトを消してから行を消す）にはしない。行を claim してから消せば、以降その
//! upload の finalize は必ず失敗する（claim が 0 行）ので、オブジェクトの所有権は sweep 側に
//! 移る。オブジェクト削除に失敗しても孤児 sweep が後から回収する。

#[allow(clippy::wildcard_imports)]
use super::*;

use std::collections::{HashMap, HashSet};

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

/// TTL sweep が 1 バッチで claim する行数。
const TTL_BATCH: i64 = 500;

/// TTL sweep が 1 周で回すバッチ数の上限（＝ 1 周 最大 10,000 行）。
///
/// 既存デプロイに初めて投入すると、それまで溜まった中断アップロードが 1 周目にまとめて
/// 対象になる。無制限に 1 文で消すと長時間トランザクションと WAL バーストで
/// `pending_upload` への並行 INSERT（＝全ユーザーの declare）が詰まるため、
/// `outbox_gc` と同じく刻んで複数周に散らす。
const MAX_TTL_BATCHES_PER_CYCLE: u32 = 20;

/// 1 回の孤児 sweep で削除するキーの上限。1 テナントの巨大な取り残しが 1 周を占有して
/// 他テナントの回収を止めないようにする（残りは次周で回収する）。
const ORPHAN_DELETE_LIMIT: usize = 10_000;

/// 1 回の孤児 sweep で発行する `list_prefix` の上限。
///
/// 削除件数の上限だけでは、**孤児が 0 件でプレフィックスだけが大量にある**定常状態
/// （テナント × 2 プレフィックス分の LIST を毎周全部叩く）を抑えられない。LIST 自体にも
/// 上限を置き、1 周のオブジェクトストア呼び出しを有界にする。
const MAX_LIST_CALLS_PER_CYCLE: usize = 500;

/// 孤児 sweep が 1 周で使える予算（オブジェクトストア呼び出しの有界化）。
struct SweepBudget {
    lists: usize,
    deletes: usize,
}

impl SweepBudget {
    fn new() -> Self {
        SweepBudget {
            lists: MAX_LIST_CALLS_PER_CYCLE,
            deletes: ORPHAN_DELETE_LIMIT,
        }
    }

    fn exhausted(&self) -> bool {
        self.lists == 0 || self.deletes == 0
    }

    /// LIST を 1 回消費する。予算切れなら false。
    fn take_list(&mut self) -> bool {
        if self.lists == 0 {
            return false;
        }
        self.lists -= 1;
        true
    }

    /// 削除予算を最大 `want` 件消費し、実際に使える件数を返す。
    fn take_deletes(&mut self, want: usize) -> usize {
        let n = want.min(self.deletes);
        self.deletes -= n;
        n
    }
}

/// 走査対象の (tenant_id, org) を引く。
///
/// オブジェクトキーは `{tenant_id}/{org}/...` なので、バケット全走査を避けるために DB 側から
/// 組を引く。`node` の素の `DISTINCT` は最も行数が伸びる表の全表スキャンになるため、
/// `node_tenant_org_idx`（migration 0065）を使った再帰 CTE の loose index scan で
/// **異なる組の個数**にしか比例しないようにする。
///
/// `tenant` 表も UNION する。プロビジョニング済みだが node がまだ 1 件も無い org
/// （最初のアップロードがそのまま中断された org）は node 側に現れないため、
/// これが無いと「最後の網」であるはずの孤児 sweep がその org を永久に見落とす。
/// 逆にオンプレ/dev のように `tenant` 表が空の構成では node 側が拾う。
const TENANT_ORG_PAIRS_SQL: &str = "\
WITH RECURSIVE pairs AS ( \
    (SELECT tenant_id, org FROM node ORDER BY tenant_id, org LIMIT 1) \
    UNION ALL \
    SELECT nxt.tenant_id, nxt.org FROM pairs p \
    CROSS JOIN LATERAL ( \
        SELECT n.tenant_id, n.org FROM node n \
         WHERE (n.tenant_id, n.org) > (p.tenant_id, p.org) \
         ORDER BY n.tenant_id, n.org LIMIT 1 \
    ) nxt \
) \
SELECT tenant_id, org FROM pairs \
UNION \
SELECT tenant_id, org FROM tenant WHERE status <> 'deleted'";

impl StorageService {
    /// TTL を過ぎた `pending_upload` を回収する。回収した件数を返す。
    ///
    /// 行を `DELETE ... RETURNING` で claim してからオブジェクトを消す（上のモジュール
    /// コメント参照）。オブジェクト削除は best-effort で、失敗しても行の回収は取り消さない
    /// （取り消すと永久に回収できない。取り残しは孤児 sweep が拾う）。
    ///
    /// 締切は **DB 側の `now()`** で作る。アプリの時計で作ると、API ホストの時計が DB より
    /// 進んだとき（NTP 障害・VM サスペンド復帰）に TTL が実質的に縮み、進行中の declare を
    /// 行ごと staging ごと消してしまう。ユーザーから見ると「PUT は成功したのに finalize が
    /// NotFound」になり、アップロード済みのバイトは復旧できない。
    pub async fn sweep_expired_pending_uploads(
        &self,
        ttl: std::time::Duration,
    ) -> Result<usize, StorageError> {
        let ttl_secs = ttl.as_secs_f64();
        let mut total = 0usize;
        for _ in 0..MAX_TTL_BATCHES_PER_CYCLE {
            // 複数レプリカが同時に回っても同じ行を奪い合わない（SKIP LOCKED）。
            let expired: Vec<ExpiredUpload> = sqlx::query_as(
                "DELETE FROM pending_upload WHERE upload_id IN ( \
                   SELECT upload_id FROM pending_upload \
                    WHERE created_at < now() - make_interval(secs => $1) \
                    ORDER BY created_at LIMIT $2 FOR UPDATE SKIP LOCKED \
                 ) RETURNING upload_id, tenant_id, org, staging_key",
            )
            .bind(ttl_secs)
            .bind(TTL_BATCH)
            .fetch_all(&self.db)
            .await?;

            if expired.is_empty() {
                break;
            }
            let claimed = expired.len();
            total += claimed;
            self.discard_expired_objects(expired).await;
            // 端数バッチ＝残りが無い。次周まで待たずに抜ける。
            if claimed < TTL_BATCH as usize {
                break;
            }
        }
        Ok(total)
    }

    /// claim 済みの期限切れ行に対応するオブジェクトを消し、監査に残す（best-effort）。
    async fn discard_expired_objects(&self, rows: Vec<ExpiredUpload>) {
        // staging は行が持つキーをそのまま使う（declare 時の値が正）。incoming は finalize が
        // 組み立てるキーなので、同じ規則でここでも組み立てる（存在しなければ削除は no-op）。
        let mut keys = Vec::with_capacity(rows.len() * 2);
        let mut by_org: HashMap<(String, String), Vec<Uuid>> = HashMap::new();
        for row in rows {
            keys.push(incoming_object_key(
                &row.tenant_id,
                &row.org,
                &row.upload_id.to_string(),
            ));
            keys.push(row.staging_key);
            by_org
                .entry((row.tenant_id, row.org))
                .or_default()
                .push(row.upload_id);
        }

        if let Err(e) = self.store.delete_batch(&keys).await {
            // 行は既に回収済み。オブジェクトは孤児 sweep が拾うので、ここでは失敗を握らず記録に留める。
            tracing::warn!(
                error = %e,
                count = keys.len(),
                "期限切れアップロードのオブジェクト削除に失敗しました（孤児 sweep で回収します）"
            );
        }

        // 全テナント横断の破壊的操作なので削除証跡を残す（`tenant.purge` と同じ扱い・design §4.9）。
        // 設定ミスや時計 skew で進行中アップロードを巻き添えにした場合、どのテナントのどの
        // upload_id を消したかをログ保持期間に依らず追跡できるようにする。
        for ((tenant_id, org), upload_ids) in by_org {
            let sctx = system_ctx(&tenant_id, &org, "system");
            if let Err(e) = self
                .audit
                .record(
                    &sctx,
                    AuditEntry {
                        action: "storage.upload_gc.expire",
                        object_type: "organization",
                        object_id: &org,
                        decision: Decision::Allow,
                        trace_id: None,
                        metadata: json!({
                            "count": upload_ids.len(),
                            "upload_ids": upload_ids,
                        }),
                    },
                )
                .await
            {
                tracing::warn!(
                    error = %e,
                    tenant_id = %tenant_id,
                    org = %org,
                    "中断アップロード回収の監査記録に失敗しました"
                );
            }
        }
    }

    /// `pending_upload` に対応行が無い `staging/` `incoming/` オブジェクトを削除する。
    /// 削除した件数を返す。
    ///
    /// TTL sweep が行ごと回収できなかった取り残し（finalize 成功後に delete が失敗した・
    /// プロセスが落ちた）を回収する最後の網。
    pub async fn sweep_orphan_upload_objects(&self) -> Result<usize, StorageError> {
        let pairs: Vec<TenantOrg> = sqlx::query_as(TENANT_ORG_PAIRS_SQL)
            .fetch_all(&self.db)
            .await?;

        let mut budget = SweepBudget::new();
        let mut deleted = 0usize;
        let mut failures = 0u32;
        for pair in pairs {
            for kind in ["staging", "incoming"] {
                if budget.exhausted() {
                    tracing::info!(
                        deleted,
                        "孤児 sweep が 1 周の上限に達しました（残りは次周で回収します）"
                    );
                    return Ok(deleted);
                }
                let prefix = format!("{}/{}/{}/", pair.tenant_id, pair.org, kind);
                let mut n = 0usize;
                let result = self
                    .sweep_orphans_under(&prefix, &pair, &mut n, &mut budget)
                    .await;
                deleted += n;
                if n > 0 {
                    self.record_orphan_audit(&pair, &prefix, n).await;
                }
                if let Err(e) = result {
                    // 1 プレフィックスの失敗で周回全体を止めない（head-of-line blocking の解消・
                    // 共有リンク失効 sweep の B-1 と同じ扱い）。ここで `?` すると、恒常的に
                    // エラーを返すテナントより後ろの組が毎周走査されず永久に回収されない。
                    tracing::warn!(
                        error = %e,
                        prefix = %prefix,
                        "孤児 sweep が 1 プレフィックスで失敗しました（次のプレフィックスへ）"
                    );
                    failures += 1;
                }
            }
        }
        if failures > 0 {
            tracing::warn!(
                failures,
                deleted,
                "孤児 sweep で一部のプレフィックスが失敗しました（#468）"
            );
        }
        Ok(deleted)
    }

    /// 1 つの prefix 配下を 1 ページずつ走査し、孤児キーを削除する。
    ///
    /// 削除件数は `deleted` に積む（途中でエラーになってもそこまでの削除は数える）。
    /// LIST・削除のいずれも `budget` から取るので、1 プレフィックスが 1 周を占有しない。
    async fn sweep_orphans_under(
        &self,
        prefix: &str,
        pair: &TenantOrg,
        deleted: &mut usize,
        budget: &mut SweepBudget,
    ) -> Result<(), StorageError> {
        let mut continuation: Option<String> = None;
        loop {
            if !budget.take_list() {
                return Ok(());
            }
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
                    // 走査中のキーと同じ (tenant_id, org) にスコープする。キー側はテナント
                    // スコープなのにここだけ全テナント横断で引くと、削除判断が他テナントの行に
                    // 左右される経路になる（tenant_id を落とさない・CLAUDE.md）。
                    let live: HashSet<Uuid> = sqlx::query_scalar(
                        "SELECT upload_id FROM pending_upload \
                          WHERE upload_id = ANY($1) AND tenant_id = $2 AND org = $3",
                    )
                    .bind(&ids)
                    .bind(&pair.tenant_id)
                    .bind(&pair.org)
                    .fetch_all(&self.db)
                    .await?
                    .into_iter()
                    .collect();
                    let mut orphans: Vec<String> = candidates
                        .into_iter()
                        .filter(|(id, _)| !live.contains(id))
                        .map(|(_, key)| key)
                        .collect();
                    let allowed = budget.take_deletes(orphans.len());
                    orphans.truncate(allowed);
                    if !orphans.is_empty() {
                        self.store.delete_batch(&orphans).await?;
                        *deleted += orphans.len();
                    }
                    if budget.deletes == 0 {
                        return Ok(());
                    }
                }
            }
            match next {
                Some(c) => continuation = Some(c),
                None => break,
            }
        }
        Ok(())
    }

    /// 孤児削除の監査（system ctx・非チェーン。TTL sweep と同じく削除証跡として残す）。
    async fn record_orphan_audit(&self, pair: &TenantOrg, prefix: &str, count: usize) {
        let sctx = system_ctx(&pair.tenant_id, &pair.org, "system");
        if let Err(e) = self
            .audit
            .record(
                &sctx,
                AuditEntry {
                    action: "storage.upload_gc.orphan_delete",
                    object_type: "organization",
                    object_id: &pair.org,
                    decision: Decision::Allow,
                    trace_id: None,
                    metadata: json!({ "count": count, "prefix": prefix }),
                },
            )
            .await
        {
            tracing::warn!(
                error = %e,
                prefix = %prefix,
                "孤児オブジェクト削除の監査記録に失敗しました"
            );
        }
    }
}
