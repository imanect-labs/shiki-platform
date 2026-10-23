//! 購読の登録と配信（data-platform §9.3〜§9.5・Task 15.6）。
//!
//! # 配信の流れ（250 ms ごとのバッチ）
//!
//! 1. リレー（[`super::relay`]）で outbox の未採番分をテーブルのストリームへ採番する（永続化）。
//! 2. 購読のあるテーブルごとに、購読者の位置（low-water mark）の最小から先を昇順に読む。
//! 3. 購読者の材料を確かめる（TTL 切れ・世代の変化・スキーマ改訂なら引き直し、変わっていれば
//!    `stream.reset` で閉じる・[`super::auth`]）。
//! 4. 変更行の before（outbox の旧値）と after（現在の本体）について、購読者を**ロール共通部の
//!    グループ**に分けてグループごとに 1 回だけ SQL で評価し、ユーザー固有部は O(1) で足す
//!    （[`super::visibility`]）。可視と判定した組は独立した評価器で事後検査する。
//! 5. 購読者ごとに `delivery_seq` 昇順で送る: 不可視→可視・可視→可視は `record.upserted`（マスク・
//!    派生値は本人の権限）、**可視→不可視・削除は `record.removed`（id のみ）**、不可視→不可視は何も
//!    送らない。送った・送らなかったに関わらず位置を進める（位置は連続して処理した最後の番号）。
//!
//! # 位置の契約（§9.2）
//!
//! - ストリームの番号は穴が無い（[`storage::event::mark_delivered_seq`]）。読んだ範囲に穴があれば
//!   それは保持期間による GC で、その先へは進めず `stream.reset` を送る（黙って飛ばさない）。
//! - 送信は永続化（採番）の後。送信待ちが溢れた・切断した購読は閉じ、クライアントは `Last-Event-ID`
//!   （最後に受けた位置）から再開する。重複はクライアントが `(record_id, rev)` で捨てる。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use authz::AuthContext;
use chrono::{DateTime, Utc};
use serde_json::json;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::auth::{Resolved, SubAuth};
use super::events::{
    AwarenessEvent, RecordRemovedEvent, RecordUpsertedEvent, SchemaChangedEvent, StreamEvent,
    StreamMessage, StreamReadyEvent, StreamResetEvent, StreamResetReason,
};
use super::outbox::ChangeKind;
use super::visibility::{eval_group, group_plan, verify, visible, Decomposed, Viewer};
use crate::model::DataTable;
use crate::record::RecordRow;
use crate::store::DataStore;
use crate::{map_db, DataError};

/// 配信の設定。
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// バッチの窓（既定 250 ms・§9.4）。
    pub batch_window: Duration,
    /// SSE 購読中の認可材料の TTL（既定 5 秒・**剥奪の配信への反映はこれだけ遅れ得る**・PIT-70）。
    pub authz_ttl: Duration,
    /// ストリームの保持期間（既定 10 分）。これより古い位置からの再開は `stream.reset`、スナップショット
    /// 読取は 410。
    pub retention: Duration,
    /// 1 バッチで読むストリームの件数。
    pub max_batch: i64,
    /// 購読者ごとの送信待ちの上限（溢れたら閉じる＝クライアントは位置から再開する）。
    pub channel_capacity: usize,
    /// 購読が無い状態がこれだけ続いたら配信タスクを止める（次の購読で再開する）。
    pub idle_shutdown: Duration,
    /// awareness の有効期間（これより古いカーソル位置は配信しない）。
    pub awareness_ttl: Duration,
    /// 次に要る位置が読めない（飛び番）まま待つ上限。超えたら保持期間外として `stream.reset`。
    pub gap_timeout: Duration,
    /// 検証用: 配信タスクを起動せず、[`DataStore::stream_tick`] を呼んだときだけ配信する。
    pub manual: bool,
    /// outbox の台帳 consumer の名前（既定 [`super::LEDGER_CONSUMER`]）。テーブルのストリームはこの
    /// 名前空間の下に作る。本番は全レプリカで同じ名前にする（同じ consumer のリレーは採番行のロックで
    /// 直列化される）。名前を分けると、outbox を互いに奪わない独立の配信系になる（検証の隔離用）。
    pub consumer: String,
}

impl Default for StreamConfig {
    fn default() -> Self {
        StreamConfig {
            batch_window: Duration::from_millis(250),
            authz_ttl: Duration::from_secs(5),
            retention: Duration::from_mins(10),
            max_batch: 500,
            channel_capacity: 1_024,
            idle_shutdown: Duration::from_mins(1),
            awareness_ttl: Duration::from_secs(30),
            gap_timeout: Duration::from_secs(10),
            manual: false,
            consumer: super::LEDGER_CONSUMER.to_string(),
        }
    }
}

type FeedKey = (String, Uuid);

/// 1 バッチの窓で回すリレーの最大回数（1 回 [`super::relay::RELAY_BATCH_LEN`] 件）。
const RELAY_ROUNDS_PER_TICK: usize = 8;

pub(super) struct Sub {
    id: u64,
    pub(super) ctx: AuthContext,
    trace_id: Option<String>,
    /// 連続して処理し終えた位置（low-water mark）。
    pub(super) cursor: i64,
    tx: mpsc::Sender<StreamMessage>,
    auth: Box<SubAuth>,
    /// 次に要る位置が読めないまま待っている（飛び番では進めない）。
    gap_since: Option<std::time::Instant>,
    /// 材料を解決した時点の認可の世代（これが進んだら TTL を待たずに引き直す）。
    epochs: HashMap<Uuid, i64>,
}

#[derive(Default)]
struct Feed {
    subs: Vec<Sub>,
    /// awareness をここより後から配る。
    awareness_after: Option<DateTime<Utc>>,
}

/// 購読の登録簿（プロセス内・`DataStore` が持つ）。
#[derive(Default)]
pub(crate) struct StreamRegistry {
    feeds: Mutex<HashMap<FeedKey, Feed>>,
    running: AtomicBool,
    next_id: AtomicU64,
    /// ロール共通部の評価回数（グループごとに 1 回・計測用）。
    evaluations: AtomicU64,
    /// 事後検査で落とした組（0 であるべき）。
    verify_drops: AtomicU64,
}

impl StreamRegistry {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<FeedKey, Feed>> {
        self.feeds.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// 購読（SSE のイベント列）。サーバが閉じると `None`。
pub struct RecordStream {
    rx: mpsc::Receiver<StreamMessage>,
}

impl RecordStream {
    /// 次のイベント（閉じたら `None`）。
    pub async fn next(&mut self) -> Option<StreamMessage> {
        self.rx.recv().await
    }

    /// 受信待ちを閉じる（切断の模擬・サーバ側の送信は失敗し購読は片付く）。
    pub fn close(&mut self) {
        self.rx.close();
    }
}

use super::batch::Batch;

/// 判定対象の行ごとの共通葉の真偽（グループで共有する）。
type CommonBits = Vec<Vec<bool>>;

/// 送る予定の 1 件。
enum Out {
    Msg(StreamMessage),
    Upsert { seq: i64, id: Uuid, derived: bool },
}

impl DataStore {
    /// テーブルのレコード変更を購読する（viewer・SSE `GET /data/tables/{id}/stream` の本体）。
    ///
    /// `since` は `Last-Event-ID`（連続して受けた最後の位置）。保持期間外・未来の位置は
    /// `stream.reset` を 1 件送って閉じる（クライアントは取り直して `since` なしで繋ぎ直す）。
    /// `since` が無ければ現在の位置から（REST の応答の `snapshot_seq` を渡すと取りこぼしが無い）。
    pub async fn subscribe_records(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        since: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<RecordStream, DataError> {
        // 世代は材料の解決より**前に**読む（解決の後の変更は次のバッチで必ず検知される）。
        let epochs = self.authz_epochs(&ctx.tenant_id).await?;
        let az = self.authz_memo();
        let table = self
            .open_for_read(ctx, &az, table_id, "data.stream.subscribe", trace_id)
            .await?;
        let mut auth = match self.resolve_stream_auth(ctx, &az, &table).await? {
            Resolved::Ok(a) => a,
            Resolved::Revoked => return Err(DataError::Forbidden),
        };
        auth.carry = Some(az.into_carry());
        let bounds = {
            let mut conn = self
                .pools
                .for_tenant(&ctx.tenant_id)
                .acquire()
                .await
                .map_err(map_db)?;
            storage::event::stream_bounds(&mut conn, &self.stream_key(table_id))
                .await
                .map_err(|e| DataError::Internal(format!("stream: {e}")))?
        };
        let (tx, rx) = mpsc::channel(self.stream_config.channel_capacity.max(2));
        self.record_audit_best_effort(
            ctx,
            "data.stream.subscribe",
            &table_id.to_string(),
            trace_id,
            json!({ "since": since, "ttl_ms": self.stream_config.authz_ttl.as_millis() as u64 }),
        )
        .await;
        if let Some(s) = since.filter(|s| !bounds.resumable(*s)) {
            tracing::info!(%table_id, since = s, head = bounds.head, "保持期間外の再開（stream.reset）");
            let _ = tx.try_send(StreamMessage {
                id: Some(bounds.head),
                event: StreamEvent::Reset(StreamResetEvent {
                    reason: StreamResetReason::Retention,
                }),
            });
            return Ok(RecordStream { rx });
        }
        let cursor = since.unwrap_or(bounds.head);
        let _ = tx.try_send(StreamMessage {
            id: Some(cursor),
            event: StreamEvent::Ready(StreamReadyEvent { position: cursor }),
        });
        let id = self.stream.next_id.fetch_add(1, Ordering::Relaxed);
        self.stream
            .lock()
            .entry((ctx.tenant_id.clone(), table_id))
            .or_default()
            .subs
            .push(Sub {
                id,
                ctx: ctx.clone(),
                trace_id: trace_id.map(str::to_string),
                cursor,
                tx,
                auth,
                gap_since: None,
                epochs,
            });
        self.ensure_stream_task();
        Ok(RecordStream { rx })
    }

    /// 検証用: ロール共通部の評価回数（グループごとに 1 回）の累計。
    #[doc(hidden)]
    pub fn stream_predicate_evaluations(&self) -> u64 {
        self.stream.evaluations.load(Ordering::Relaxed)
    }

    /// 検証用: 事後検査が落とした組の累計（0 であるべき）。
    #[doc(hidden)]
    pub fn stream_verify_drops(&self) -> u64 {
        self.stream.verify_drops.load(Ordering::Relaxed)
    }

    /// 検証用: 登録中の購読数。
    #[doc(hidden)]
    pub fn stream_subscriber_count(&self) -> usize {
        self.stream.lock().values().map(|f| f.subs.len()).sum()
    }

    fn ensure_stream_task(&self) {
        if self.stream_config.manual || self.stream.running.swap(true, Ordering::AcqRel) {
            return;
        }
        let store = self.clone();
        tokio::spawn(async move { store.stream_loop().await });
    }

    async fn stream_loop(self) {
        let mut idle = Duration::ZERO;
        loop {
            tokio::time::sleep(self.stream_config.batch_window).await;
            if self.stream_subscriber_count() == 0 {
                idle += self.stream_config.batch_window;
                if idle >= self.stream_config.idle_shutdown {
                    self.stream.running.store(false, Ordering::Release);
                    // 止めた直後に登録された購読があれば自分で続ける（取りこぼさない）。
                    if self.stream_subscriber_count() == 0
                        || self.stream.running.swap(true, Ordering::AcqRel)
                    {
                        return;
                    }
                }
                continue;
            }
            idle = Duration::ZERO;
            if let Err(e) = self.stream_tick().await {
                tracing::warn!(error = %e, "SSE 配信のバッチに失敗（次の窓で再試行）");
            }
        }
    }

    /// 1 バッチ分の配信（リレー → 購読のあるテーブルごとに判定・送信）。
    #[doc(hidden)]
    pub async fn stream_tick(&self) -> Result<(), DataError> {
        let keys: Vec<FeedKey> = self
            .stream
            .lock()
            .iter()
            .filter(|(_, f)| !f.subs.is_empty())
            .map(|(k, _)| k.clone())
            .collect();
        if keys.is_empty() {
            return Ok(());
        }
        // 溜まっていれば数バッチ続けて採番する（他テーブルの大量の変更で手前が詰まらないように）。
        for _ in 0..RELAY_ROUNDS_PER_TICK {
            match self.relay_record_changes_once().await {
                Ok(n) if n >= super::relay::RELAY_BATCH_LEN => {}
                Ok(_) => break,
                Err(e) => {
                    tracing::warn!(error = %e, "outbox の採番に失敗（次の窓で再試行）");
                    break;
                }
            }
        }
        let mut epochs: HashMap<String, HashMap<Uuid, i64>> = HashMap::new();
        for (tenant, _) in &keys {
            if !epochs.contains_key(tenant) {
                epochs.insert(tenant.clone(), self.authz_epochs(tenant).await?);
            }
        }
        for key in keys {
            let feed = std::mem::take(self.stream.lock().entry(key.clone()).or_default());
            let now_epochs = epochs.get(&key.0).cloned().unwrap_or_default();
            let feed = self.process_feed(&key, feed, now_epochs).await;
            let mut map = self.stream.lock();
            let slot = map.entry(key).or_default();
            // 処理中に登録された購読は残したまま戻す。
            let added = std::mem::take(&mut slot.subs);
            *slot = feed;
            slot.subs.extend(added);
        }
        self.stream.lock().retain(|_, f| !f.subs.is_empty());
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // 1 テーブル分の判定（材料の更新 → 読込 → 評価 → 送信）。
    async fn process_feed(
        &self,
        key: &FeedKey,
        mut feed: Feed,
        epochs: HashMap<Uuid, i64>,
    ) -> Feed {
        let (tenant_id, table_id) = (key.0.as_str(), key.1);
        let Some(first) = feed.subs.first() else {
            return feed;
        };
        let ctx0 = first.ctx.clone();
        let table = match self.fetch_live(&ctx0, table_id).await {
            Ok(t) => t,
            Err(DataError::NotFound) => {
                for s in std::mem::take(&mut feed.subs) {
                    self.end_sub(
                        s,
                        table_id,
                        Some(StreamResetReason::TableDeleted),
                        "table_deleted",
                    )
                    .await;
                }
                return feed;
            }
            Err(e) => {
                tracing::warn!(error = %e, %table_id, "SSE: テーブルの読込に失敗");
                return feed;
            }
        };
        let subs = self
            .refresh_auth(std::mem::take(&mut feed.subs), &table, &epochs)
            .await;
        let r = self
            .deliver(tenant_id, &table, subs, feed.awareness_after)
            .await;
        match r {
            Ok((subs, aw)) => {
                feed.subs = subs;
                feed.awareness_after = aw;
            }
            Err((subs, e)) => {
                tracing::warn!(error = %e, %table_id, "SSE: 配信に失敗（次の窓で再試行）");
                feed.subs = subs;
            }
        }
        feed
    }

    /// 材料の TTL・世代・スキーマの版を確かめ、要れば引き直す（変化した購読は閉じる）。
    async fn refresh_auth(
        &self,
        subs: Vec<Sub>,
        table: &DataTable,
        epochs: &HashMap<Uuid, i64>,
    ) -> Vec<Sub> {
        let ttl = self.stream_config.authz_ttl;
        let mut live = Vec::with_capacity(subs.len());
        for mut s in subs {
            if s.tx.is_closed() {
                self.end_sub(s, table.id, None, "disconnected").await;
                continue;
            }
            let stale = super::invalidate::changed_for(&s.epochs, epochs, table.id)
                || s.auth.resolved_at.elapsed() >= ttl
                || s.auth.schema_version != table.schema_version;
            if !stale {
                live.push(s);
                continue;
            }
            let az = self.authz_memo();
            match self.resolve_stream_auth(&s.ctx, &az, table).await {
                Ok(Resolved::Ok(mut a)) => {
                    if a.fingerprint != s.auth.fingerprint {
                        self.end_sub(
                            s,
                            table.id,
                            Some(StreamResetReason::AuthzChanged),
                            "authz_changed",
                        )
                        .await;
                        continue;
                    }
                    a.carry = Some(az.into_carry());
                    s.epochs = epochs.clone();
                    tracing::debug!(table_id = %table.id, sub = s.id, "SSE: 認可材料を引き直した（変化なし）");
                    s.auth = a;
                    live.push(s);
                }
                Ok(Resolved::Revoked) => {
                    self.end_sub(
                        s,
                        table.id,
                        Some(StreamResetReason::AuthzChanged),
                        "revoked",
                    )
                    .await;
                }
                Err(e) => {
                    // 引き直せない間は配信しない（fail-closed）。購読は閉じ、クライアントが再接続する。
                    tracing::warn!(error = %e, table_id = %table.id, "SSE: 認可材料の引き直しに失敗");
                    self.end_sub(s, table.id, None, "authz_error").await;
                }
            }
        }
        live
    }

    /// 購読を閉じる（`reset` があれば送ってから・監査に残す）。
    async fn end_sub(
        &self,
        s: Sub,
        table_id: Uuid,
        reset: Option<StreamResetReason>,
        reason: &str,
    ) {
        if let Some(r) = reset {
            let _ = s.tx.try_send(StreamMessage {
                id: Some(s.cursor),
                event: StreamEvent::Reset(StreamResetEvent { reason: r }),
            });
        }
        self.record_audit_best_effort(
            &s.ctx,
            "data.stream.end",
            &table_id.to_string(),
            s.trace_id.as_deref(),
            json!({ "reason": reason, "position": s.cursor }),
        )
        .await;
    }

    /// 読込 → 評価 → 送信。失敗したら購読を位置を変えずに返す（次の窓で再試行）。
    #[allow(clippy::too_many_lines)]
    async fn deliver(
        &self,
        tenant_id: &str,
        table: &DataTable,
        subs: Vec<Sub>,
        awareness_after: Option<DateTime<Utc>>,
    ) -> Result<(Vec<Sub>, Option<DateTime<Utc>>), (Vec<Sub>, DataError)> {
        if subs.is_empty() {
            return Ok((subs, awareness_after));
        }
        let loaded = match self
            .load_batch(tenant_id, table, &subs, awareness_after)
            .await
        {
            Ok(l) => l,
            Err(e) => return Err((subs, e)),
        };
        let Batch {
            parsed,
            states,
            rows,
            awareness,
            awareness_at,
            mut conn,
            first_seq,
        } = loaded;
        let d = match Decomposed::of(&table.schema) {
            Ok(d) => d,
            Err(e) => return Err((subs, e)),
        };
        // ロール共通部のグループごとに 1 回だけ評価する（principal・共有集合は鍵に入らない）。
        let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
        let mut plans = HashMap::new();
        for (i, s) in subs.iter().enumerate() {
            let v = Viewer {
                principal: &s.ctx.principal.id,
                material: &s.auth.material,
                shared: &s.auth.shared,
            };
            match group_plan(&d, &table.schema, &v) {
                Ok(p) => {
                    groups.entry(p.key.clone()).or_default().push(i);
                    plans.entry(p.key.clone()).or_insert(p);
                }
                Err(e) => return Err((subs, e)),
            }
        }
        let mut common_of: Vec<Option<Arc<CommonBits>>> = vec![None; subs.len()];
        for (k, members) in &groups {
            let Some(plan) = plans.get(k) else { continue };
            let res = if d.has_policy() {
                match eval_group(&mut conn, plan, &states).await {
                    Ok(r) => r,
                    Err(e) => return Err((subs, e)),
                }
            } else {
                vec![Vec::new(); states.len()]
            };
            self.stream.evaluations.fetch_add(1, Ordering::Relaxed);
            let res = Arc::new(res);
            for m in members {
                common_of[*m] = Some(Arc::clone(&res));
            }
        }
        drop(conn);
        let mut survivors = Vec::with_capacity(subs.len());
        for (i, mut s) in subs.into_iter().enumerate() {
            let common = common_of[i].take().unwrap_or_default();
            if first_seq > 0 && s.cursor + 1 < first_seq {
                // 次に要る位置がまだ読めない（low-water mark は飛び番で進めない）。コミット待ちの
                // 採番なら直に埋まる。埋まらないまま待ちが続くのは保持期間の GC で、その先は欠落なく
                // 続けられないので stream.reset。
                let since = *s.gap_since.get_or_insert_with(std::time::Instant::now);
                if since.elapsed() >= self.stream_config.gap_timeout {
                    self.end_sub(s, table.id, Some(StreamResetReason::Retention), "retention")
                        .await;
                } else {
                    survivors.push(s);
                }
                continue;
            }
            s.gap_since = None;
            let v = Viewer {
                principal: &s.ctx.principal.id,
                material: &s.auth.material,
                shared: &s.auth.shared,
            };
            let vis = |idx: Option<usize>| -> bool {
                let Some(i) = idx else { return false };
                let empty = Vec::new();
                let c = common.get(i).unwrap_or(&empty);
                if !visible(&d, &v, &states[i], c) {
                    return false;
                }
                if verify(&table.schema, &v, &states[i]) {
                    return true;
                }
                self.stream.verify_drops.fetch_add(1, Ordering::Relaxed);
                tracing::error!(
                    target: "security",
                    alert = "data.stream.post_filter_mismatch",
                    table_id = %table.id,
                    "SSE の可視性判定が事後検査と一致しません（配信しない）"
                );
                false
            };
            let mut out: Vec<Out> = Vec::new();
            let mut terminal = false;
            let mut cursor = s.cursor;
            for p in parsed.iter().filter(|p| p.seq > s.cursor) {
                match p.kind {
                    ChangeKind::Schema => {
                        out.push(Out::Msg(StreamMessage {
                            id: Some(p.seq),
                            event: StreamEvent::SchemaChanged(SchemaChangedEvent {
                                table_id: table.id,
                                schema_version: p.rev,
                            }),
                        }));
                        terminal = true;
                    }
                    ChangeKind::Refresh => {
                        out.push(Out::Msg(StreamMessage {
                            id: Some(p.seq),
                            event: StreamEvent::Reset(StreamResetEvent {
                                reason: StreamResetReason::Refresh,
                            }),
                        }));
                        terminal = true;
                    }
                    ChangeKind::Derived => {
                        if vis(p.after) {
                            out.push(Out::Upsert {
                                seq: p.seq,
                                id: p.record_id,
                                derived: true,
                            });
                        }
                    }
                    ChangeKind::Create | ChangeKind::Update | ChangeKind::Delete => {
                        match (vis(p.before), vis(p.after)) {
                            (_, true) => out.push(Out::Upsert {
                                seq: p.seq,
                                id: p.record_id,
                                derived: false,
                            }),
                            // 可視 → 不可視・削除: 値を含まない removed（PIT-73）。
                            (true, false) => out.push(Out::Msg(StreamMessage {
                                id: Some(p.seq),
                                event: StreamEvent::Removed(RecordRemovedEvent {
                                    record_id: p.record_id,
                                }),
                            })),
                            (false, false) => {}
                        }
                    }
                }
                cursor = p.seq;
                if terminal {
                    break;
                }
            }
            // awareness（データを含まない・見えない行・マスク列のものは送らない）。
            if !terminal {
                for a in &awareness {
                    if a.principal == s.ctx.principal.id {
                        continue;
                    }
                    if a.field.as_ref().is_some_and(|f| s.auth.masked.contains(f)) {
                        continue;
                    }
                    if a.record_id.is_some() && !vis(a.state) {
                        continue;
                    }
                    out.push(Out::Msg(StreamMessage {
                        id: None,
                        event: StreamEvent::Awareness(AwarenessEvent {
                            principal: a.principal.clone(),
                            record_id: a.record_id,
                            field: a.field.clone(),
                            at: a.at,
                        }),
                    }));
                }
            }
            match self.send_all(&mut s, table, &rows, out).await {
                Ok(()) if terminal => {
                    s.cursor = cursor;
                    self.end_sub(s, table.id, None, "terminal").await;
                }
                Ok(()) => {
                    s.cursor = cursor;
                    survivors.push(s);
                }
                Err(reason) => {
                    // 送れなかった（切断・溢れ）。クライアントは最後に受けた位置から再開する。
                    self.end_sub(s, table.id, None, reason).await;
                }
            }
        }
        Ok((survivors, awareness_at.or(awareness_after)))
    }

    /// 送信（upsert は本人の権限で派生値・マスクを適用してから）。失敗は理由を返す。
    async fn send_all(
        &self,
        s: &mut Sub,
        table: &DataTable,
        rows: &HashMap<Uuid, RecordRow>,
        out: Vec<Out>,
    ) -> Result<(), &'static str> {
        let ups: Vec<Uuid> = out
            .iter()
            .filter_map(|o| match o {
                Out::Upsert { id, .. } => Some(*id),
                Out::Msg(_) => None,
            })
            .collect();
        let mut records: HashMap<Uuid, crate::model::DataRecord> = HashMap::new();
        if !ups.is_empty() {
            let mut recs: Vec<crate::model::DataRecord> = ups
                .iter()
                .collect::<HashSet<_>>()
                .into_iter()
                .filter_map(|id| rows.get(id))
                .map(|r| r.clone().into_record(&table.schema))
                .collect();
            let carry = s.auth.carry.take();
            let az = match carry {
                Some(c) => crate::policy::memo::AuthzMemo::resume(self.authz.as_ref(), c),
                None => self.authz_memo(),
            };
            let r = self
                .resolve_derived_fields(&s.ctx, &az, table, &s.auth.masked, &mut recs)
                .await;
            s.auth.carry = Some(az.into_carry());
            if let Err(e) = r {
                tracing::warn!(error = %e, table_id = %table.id, "SSE: 派生値の計算に失敗");
                return Err("derived_error");
            }
            Self::apply_mask_records(&s.auth.masked, &mut recs);
            records = recs.into_iter().map(|r| (r.id, r)).collect();
        }
        for o in out {
            let msg = match o {
                Out::Msg(m) => m,
                Out::Upsert { seq, id, derived } => {
                    let Some(record) = records.get(&id).cloned() else {
                        continue;
                    };
                    StreamMessage {
                        id: Some(seq),
                        event: StreamEvent::Upserted(RecordUpsertedEvent { record, derived }),
                    }
                }
            };
            match s.tx.try_send(msg) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => return Err("overflow"),
                Err(mpsc::error::TrySendError::Closed(_)) => return Err("disconnected"),
            }
        }
        Ok(())
    }
}
