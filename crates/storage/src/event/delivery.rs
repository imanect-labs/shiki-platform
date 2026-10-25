//! per-consumer fan-out（P10-A0・配送台帳 `outbox_delivery`）。
//!
//! 既存の [`claim`]/[`mark_processed`]（RAG relay 専用・`processed_at` 経路）はそのまま温存し、
//! **追加コンシューマ**（workflow の event matcher・miniapp B2 関数トリガ等）はこの配送台帳ベースの
//! API を使う。これにより「片方の消費が他方を消す」取りこぼしを避けつつ、生成側（[`emit_on`]）は
//! 不変＝outbox が真の fan-out 点として機能する（roadmap/phase-10.md P10-A0）。
//!
//! 台帳へ登録されたコンシューマ集合（`outbox_consumer`）は **GC の判定正本**でもある。
//! 詳細は [`registered_consumers`] と `super::gc` を参照。
//!
//! [`claim`]: super::claim
//! [`mark_processed`]: super::mark_processed
//! [`emit_on`]: super::emit_on

use sqlx::PgConnection;

use super::{OutboxEvent, OutboxRow};
use crate::error::StorageError;

/// 指定コンシューマがまだ配送していない未処理イベントを FIFO で `limit` 件まで取り出す。
///
/// [`claim`](super::claim) と同じく `FOR UPDATE SKIP LOCKED` で同時実行の二重取得を防ぐが、判定を
/// `processed_at` 破壊的消費ではなく **`NOT EXISTS(outbox_delivery for consumer)`** で行う。
/// 存在性ベースの anti-join なので id 順・コミット順に依存せず、**後からコミットした小さい id の
/// 行も次スキャンで拾える**（単純 last_seq カーソルの「未コミット飛び越し」問題を回避）。
/// 掴んだ行は同一 txn 内で処理 → [`mark_delivered`] → commit する（at-least-once）。
///
/// ⚠️ **コスト特性**: 未配送行は定常状態ではテール（最大 id 側）にしか無いため、この anti-join は
/// **配送済みの全行を走査してからテールに到達する**（走査長 = outbox の総行数）。つまり
/// `super::gc` の GC が回っていることがこの API の性能前提である（#413。GC が止まると
/// 累積イベント数に線形劣化し、100 万行で 1 回 0.5 秒に達する）。
pub async fn claim_undelivered(
    conn: &mut PgConnection,
    consumer: &str,
    limit: i64,
) -> Result<Vec<OutboxEvent>, StorageError> {
    let rows: Vec<OutboxRow> = sqlx::query_as(
        "SELECT o.id, o.org, o.tenant_id, o.node_id, o.version, o.op, o.actor, o.trace_id, \
                o.payload, o.created_at, coalesce(n.system, false) AS system \
         FROM storage_event_outbox o \
         LEFT JOIN node n ON n.id = o.node_id \
         WHERE NOT EXISTS ( \
             SELECT 1 FROM outbox_delivery d \
             WHERE d.consumer = $1 AND d.event_id = o.id \
         ) \
         ORDER BY o.id \
         FOR UPDATE OF o SKIP LOCKED \
         LIMIT $2",
    )
    .bind(consumer)
    .bind(limit)
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(OutboxRow::into_event).collect())
}

/// [`claim_undelivered`] の、`after_id` より大きい id だけを見る版（走査の下限つき）。
///
/// 配送済みの全行を毎回走査しないための近道（[`ClaimHints`] が下限を決める）。`after_id` 以下で後から
/// コミットした行は、定期的な全走査（下限 0）で拾う（取りこぼさない・遅れて届くだけ）。
pub async fn claim_undelivered_after(
    conn: &mut PgConnection,
    consumer: &str,
    after_id: i64,
    limit: i64,
) -> Result<Vec<OutboxEvent>, StorageError> {
    if after_id <= 0 {
        return claim_undelivered(conn, consumer, limit).await;
    }
    let rows: Vec<OutboxRow> = sqlx::query_as(
        "SELECT o.id, o.org, o.tenant_id, o.node_id, o.version, o.op, o.actor, o.trace_id, \
                o.payload, o.created_at, coalesce(n.system, false) AS system \
         FROM storage_event_outbox o \
         LEFT JOIN node n ON n.id = o.node_id \
         WHERE o.id > $3 AND NOT EXISTS ( \
             SELECT 1 FROM outbox_delivery d \
             WHERE d.consumer = $1 AND d.event_id = o.id \
         ) \
         ORDER BY o.id \
         FOR UPDATE OF o SKIP LOCKED \
         LIMIT $2",
    )
    .bind(consumer)
    .bind(limit)
    .bind(after_id)
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(OutboxRow::into_event).collect())
}

/// 溜まっている間の下限からの走査の間隔。後からコミットした小さい id の行はこの間隔以内に配送される。
pub const CLAIM_SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// 下限 0 の全走査を必ず挟む間隔（[`ClaimHints`] の推論が外れる稀な場合の安全網）。
pub const CLAIM_FULL_SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// claim の直前に取る outbox の採番と Tx の状況（[`ClaimHints`] の判断材料）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimProbe {
    /// outbox の id の採番の最後の値（未採番なら 0）。ロールバックした採番も含む。
    pub seq: i64,
    /// 実行中の Tx の最小 xid（`pg_snapshot_xmin`）。
    pub xmin: i64,
    /// 次に払い出す xid（`pg_snapshot_xmax`）。
    pub xmax: i64,
}

/// [`ClaimProbe`] を取る（O(1)・表は読まない）。
pub async fn claim_probe(conn: &mut PgConnection) -> Result<ClaimProbe, StorageError> {
    let (seq, xmin, xmax): (i64, i64, i64) = sqlx::query_as(
        "SELECT coalesce(pg_sequence_last_value('storage_event_outbox_id_seq'::regclass), 0), \
                pg_snapshot_xmin(pg_current_snapshot())::text::int8, \
                pg_snapshot_xmax(pg_current_snapshot())::text::int8",
    )
    .fetch_one(conn)
    .await?;
    Ok(ClaimProbe { seq, xmin, xmax })
}

/// [`ClaimHints::plan`] の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimPlan {
    /// 走査しない（前回の確認から outbox に新しい id が払い出されていない・全て配送済み）。
    Skip,
    /// `after` より大きい id を走査する（0 なら全走査）。
    Scan { after: i64 },
}

/// コンシューマごとの走査の下限（プロセス内・[`claim_undelivered_hinted`] 用）。
///
/// [`claim_undelivered`] の anti-join は配送済みの全行を走査してからテールに届く（上の ⚠️）。2 つの場面で
/// これを避ける:
///
/// 1. **溜まっている間**（一括取込で 10 万件のイベント・2 回目の検証 2-D-P3-2）: 直前のバッチが上限まで
///    埋まったら、直近に取った最大の id を下限にしてテールから読む（`tail`）。[`CLAIM_SWEEP_INTERVAL`] ごとに
///    確定済みの下限（`floor`）からの走査で、後からコミットした行・巻き戻った取得を拾う。
/// 2. **アイドルの間**（3 回目の検証 3-D-1: 各コンシューマが毎秒 outbox 全体を読み、PG が CPU を食っていた）:
///    「`floor` 以下の id は全て配送済み」が確かめられ、その後 outbox の採番（`seq`）が動いていなければ、
///    **走査そのものをしない**（[`ClaimPlan::Skip`]）。新しい書込は採番を動かすので、次の claim で走査する。
///
/// `floor` の確定（[`Self::observe`]）: 採番が今の値（`seq`）になってから最初に取った [`ClaimProbe`] の時点で
/// 実行中だった Tx が全て終わっている（今回の `xmin` ≥ その `xmax`）なら、`seq` 以下の id を払い出した
/// Tx は全てコミットかロールバックを終えている（採番はその材料より前なので、払い出した Tx はその時点で
/// 終わっているか実行中だった）。その上で `floor` 以下から走査して 1 件も無ければ、`seq`
/// 以下は全て配送済み（＝新しい `floor`）。行を取ったバッチでは確定しない（そのバッチが巻き戻っても
/// 取りこぼさない）。
///
/// 推論の外れる稀な場合（採番から行の挿入までの一瞬に xid を得た Tx、別プロセスが claim 中で
/// `SKIP LOCKED` で飛ばした行がその後巻き戻った場合）は、[`CLAIM_FULL_SWEEP_INTERVAL`] ごとの下限 0 の
/// 全走査で拾う（取りこぼさない・遅れて届くだけ）。キーはプールとコンシューマ（id の空間はプールごと）。
#[derive(Debug, Default)]
pub struct ClaimHints {
    inner: std::sync::Mutex<std::collections::HashMap<String, HintState>>,
}

#[derive(Debug)]
struct HintState {
    /// 溜まっている間のテールの下限（0 = 溜まっていない）。
    tail: i64,
    /// 次に `floor` から走査する時刻（溜まっている間）。
    next_sweep: std::time::Instant,
    /// 次に下限 0 で全走査する時刻。
    next_full: std::time::Instant,
    /// この id 以下は全て配送済み（確かめられていなければ `None`）。
    floor: Option<i64>,
    /// 今の採番の値（`seq`）で最初に取った [`ClaimProbe`]。
    first_at_seq: Option<ClaimProbe>,
}

impl HintState {
    fn new(now: std::time::Instant) -> Self {
        Self {
            tail: 0,
            next_sweep: now + CLAIM_SWEEP_INTERVAL,
            next_full: now + CLAIM_FULL_SWEEP_INTERVAL,
            floor: None,
            first_at_seq: None,
        }
    }
}

impl ClaimHints {
    /// 次の claim の走査の仕方。
    pub fn plan(&self, key: &str, probe: ClaimProbe) -> ClaimPlan {
        self.plan_at(key, probe, std::time::Instant::now())
    }

    fn plan_at(&self, key: &str, probe: ClaimProbe, now: std::time::Instant) -> ClaimPlan {
        let Ok(mut m) = self.inner.lock() else {
            return ClaimPlan::Scan { after: 0 };
        };
        let e = m
            .entry(key.to_string())
            .or_insert_with(|| HintState::new(now));
        if now >= e.next_full {
            e.next_full = now + CLAIM_FULL_SWEEP_INTERVAL;
            e.next_sweep = now + CLAIM_SWEEP_INTERVAL;
            return ClaimPlan::Scan { after: 0 };
        }
        if e.floor == Some(probe.seq) {
            return ClaimPlan::Skip;
        }
        let floor = e.floor.unwrap_or(0);
        if now >= e.next_sweep {
            e.next_sweep = now + CLAIM_SWEEP_INTERVAL;
            return ClaimPlan::Scan { after: floor };
        }
        ClaimPlan::Scan {
            after: e.tail.max(floor),
        }
    }

    /// 走査の結果を反映する。バッチが上限（`limit`）まで埋まった＝溜まっているときだけ最大の id を
    /// テールの下限にし（下げない）、埋まらなければテールを外す。`floor` の確定は型の doc を参照。
    pub fn observe(
        &self,
        key: &str,
        probe: ClaimProbe,
        after: i64,
        events: &[OutboxEvent],
        limit: i64,
    ) {
        let full = i64::try_from(events.len()).unwrap_or(i64::MAX) >= limit;
        let Ok(mut m) = self.inner.lock() else {
            return;
        };
        let now = std::time::Instant::now();
        let e = m
            .entry(key.to_string())
            .or_insert_with(|| HintState::new(now));
        match events.iter().map(|e| e.id).max() {
            Some(max) if full => e.tail = e.tail.max(max),
            _ => e.tail = 0,
        }
        // 同じ採番の値で最初に取った材料と比べる（後の材料ほど xmax が大きく、確定が遅れる）。
        if e.first_at_seq.is_none_or(|c| c.seq != probe.seq) {
            e.first_at_seq = Some(probe);
        }
        let settled = e
            .first_at_seq
            .is_some_and(|c| c.seq == probe.seq && probe.xmin >= c.xmax && c != probe);
        if events.is_empty() && after <= e.floor.unwrap_or(0) && settled {
            e.floor = Some(probe.seq);
        }
    }
}

/// [`ClaimHints`] を使って claim する（アイドル時は走査しない・溜まっている間はテールから読む）。
///
/// [`claim_undelivered`] と同じく呼び出し側の Tx 上で行をロックする。返るイベントの集合の意味も同じ
/// （未配送のものを id 順に最大 `limit` 件）だが、後からコミットした小さい id の行やアイドル判定の
/// 外れる稀な行は、[`CLAIM_SWEEP_INTERVAL`]〜[`CLAIM_FULL_SWEEP_INTERVAL`] 遅れて届くことがある。
pub async fn claim_undelivered_hinted(
    conn: &mut PgConnection,
    hints: &ClaimHints,
    key: &str,
    consumer: &str,
    limit: i64,
) -> Result<Vec<OutboxEvent>, StorageError> {
    let probe = claim_probe(&mut *conn).await?;
    let after = match hints.plan(key, probe) {
        ClaimPlan::Skip => return Ok(Vec::new()),
        ClaimPlan::Scan { after } => after,
    };
    let events = claim_undelivered_after(conn, consumer, after, limit).await?;
    hints.observe(key, probe, after, &events, limit);
    Ok(events)
}

/// 指定コンシューマへの配送を台帳に記録する（[`claim_undelivered`] と同一 txn 内で呼ぶ）。
///
/// `(consumer, event_id)` 主キーで冪等（再配信で同じ行を掴んでも二重記録にならない）。
pub async fn mark_delivered(
    conn: &mut PgConnection,
    consumer: &str,
    ids: &[i64],
) -> Result<(), StorageError> {
    if ids.is_empty() {
        return Ok(());
    }
    // tenant_id は outbox 行から写す（台帳の絞り込み・監査用）。
    // `FOR KEY SHARE` で読んだ行を FK 検査まで押さえる。READ COMMITTED の INSERT...SELECT は
    // 文のスナップショットで見えた行を、FK 検査の時点で並行 GC が消し終えていると FK 違反で落ちる
    // （バッチ全体が巻き戻る）。ロックを取れば、削除中の行はその完了を待って「消えた行」として
    // 外れる（消えたイベントは配送しようがないので台帳にも載せない）。claim 済みの行は既に
    // `FOR UPDATE` を持っているので追加の待ちは無い。
    sqlx::query(
        "INSERT INTO outbox_delivery (consumer, event_id, tenant_id) \
         SELECT $1, o.id, o.tenant_id \
         FROM storage_event_outbox o \
         WHERE o.id = ANY($2) \
         FOR KEY SHARE OF o \
         ON CONFLICT (consumer, event_id) DO NOTHING",
    )
    .bind(consumer)
    .bind(ids)
    .execute(conn)
    .await?;
    Ok(())
}

/// 新規追加コンシューマを **現時点のバックログを飛ばして** 登録する（初回配送の暴発防止）。
///
/// 台帳ベースの [`claim_undelivered`] は「自分の delivery が無い行」を全て返すため、コンシューマを
/// 初めて有効化すると **過去の全 storage.write を再配送**してしまう（新規 workflow matcher が
/// 履歴イベントで一斉発火する）。これを避けるため、登録時に**現スナップショットで可視な**（＝コミット
/// 済みの）outbox 行を全て「配送済み」として台帳に刻む。**未コミットの in-flight イベントはこの
/// スナップショットに映らない**ため delivery が付かず、コミット後に正しく配送される（＝有効化以降の
/// イベントのみ処理・未コミット飛び越しも起こさない）。冪等（`ON CONFLICT DO NOTHING`）。
///
/// 起動時 wiring から consumer 有効化のたびに呼べる（**初回登録時のみ** fast-forward）。
///
/// `outbox_consumer` 台帳に consumer 名を一度だけ記録し、初回だけ現バックログを配送済みに刻む。
/// 2 回目以降（再起動）は no-op ＝ **停止中に到着した未配送イベントを取りこぼさない**。返り値は刻んだ件数。
///
/// 登録の記録・バックログの fast-forward・GC との相互排除のロックは**この関数が張る Tx**（呼び出し側が
/// Tx 中ならセーブポイント）の中で行う。理由が 2 つある:
/// 1. 「登録の記録」と「バックログの fast-forward」が**原子的でないと壊れる**。別々にコミットされると
///    fast-forward が失敗した場合に**登録済みなのにバックログが配送済みになっていない**状態が残る
///    → relay がバックログ全件を拾い一斉発火する。
/// 2. GC との相互排除に使う advisory lock が **txn 単位**（autocommit では文の終わりで解放される）。
///
/// pool しか手元に無い呼び出し側は [`register_consumer_on_pool`] を使える（自前で txn を張る）。
pub async fn register_consumer(
    conn: &mut PgConnection,
    consumer: &str,
) -> Result<u64, StorageError> {
    // 登録と fast-forward（と GC との相互排除のロック）を 1 つの Tx に閉じる。呼び出し側が Tx 中なら
    // セーブポイントになり、autocommit の接続を渡されても原子性と相互排除が崩れない。
    let mut tx = sqlx::Connection::begin(&mut *conn).await?;
    let marked = register_in_tx(&mut tx, consumer).await?;
    tx.commit().await?;
    Ok(marked)
}

async fn register_in_tx(conn: &mut PgConnection, consumer: &str) -> Result<u64, StorageError> {
    // 初回登録か判定（RETURNING で挿入できたら初回）。
    let first: Option<String> = sqlx::query_scalar(
        "INSERT INTO outbox_consumer (name) VALUES ($1) \
         ON CONFLICT (name) DO NOTHING RETURNING name",
    )
    .bind(consumer)
    .fetch_optional(&mut *conn)
    .await?;
    if first.is_none() {
        // 既登録: fast-forward しない（未配送を温存）。
        return Ok(0);
    }
    // 下の fast-forward は **WHERE 無しで全 outbox 行**を台帳へ写すため、並行 GC の DELETE と
    // レースすると FK 違反で落ちる（→ この txn がロールバックし `outbox_consumer` の登録も消え、
    // 未登録のまま relay がバックログ全件を拾って一斉発火する）。GC と相互排除する。
    // 詳細は `super::gc::GC_LOCK_KEY` の doc を参照。
    super::gc::lock_outbox_gc(&mut *conn).await?;
    let done = sqlx::query(
        "INSERT INTO outbox_delivery (consumer, event_id, tenant_id) \
         SELECT $1, o.id, o.tenant_id FROM storage_event_outbox o \
         ON CONFLICT (consumer, event_id) DO NOTHING",
    )
    .bind(consumer)
    .execute(conn)
    .await?;
    Ok(done.rows_affected())
}

/// [`register_consumer`] を**自前のトランザクション**で実行する（pool しか無い呼び出し側向け）。
///
/// 起動時 wiring はコネクションを `acquire()` して渡しがちだが、それは autocommit であり
/// [`register_consumer`] の前提（登録と fast-forward の原子性・txn 単位 advisory lock）を破る。
/// 正しい使い方を既定にするためのラッパ。
pub async fn register_consumer_on_pool(
    pool: &sqlx::PgPool,
    consumer: &str,
) -> Result<u64, StorageError> {
    let mut tx = pool.begin().await?;
    let marked = register_consumer(&mut tx, consumer).await?;
    tx.commit().await?;
    Ok(marked)
}

/// コンシューマを**恒久的に廃止**する（登録を消し、その台帳行も消す）。廃止できたら `true`。
///
/// GC（`super::gc::gc_delivered_registered`）は `outbox_consumer` に載っている全コンシューマの配送が
/// 揃うまで outbox 行を消さない。したがってコンシューマを**コードから削除しただけでは GC が永久に
/// 停止する**（誰も配送しない行が溜まり続ける）。機能を畳むときは必ずこれを呼ぶこと。
///
/// ⚠️ **一時的な無効化（`workflow.enabled=false` 等）でこれを呼んではいけない。** 再有効化したときに
/// [`register_consumer`] の fast-forward が走り、**停止中に到着したイベントを配送済みとして飛ばす**。
/// 一時停止中はイベントを溜めておくのが正しい（それが台帳方式の目的）。
pub async fn unregister_consumer(
    conn: &mut PgConnection,
    consumer: &str,
) -> Result<bool, StorageError> {
    let removed: Option<String> =
        sqlx::query_scalar("DELETE FROM outbox_consumer WHERE name = $1 RETURNING name")
            .bind(consumer)
            .fetch_optional(&mut *conn)
            .await?;
    if removed.is_none() {
        return Ok(false);
    }
    // 台帳行も消す（残しても GC 判定は consumer 集合基準なので害はないが、無用に肥大させない）。
    sqlx::query("DELETE FROM outbox_delivery WHERE consumer = $1")
        .bind(consumer)
        .execute(conn)
        .await?;
    Ok(true)
}

/// 現在登録されている台帳コンシューマ名を返す（`outbox_consumer` が正本）。
///
/// **これが GC 判定の正本である理由**: 台帳コンシューマはそれぞれ独立したフィーチャフラグの背後で
/// 起動する（`workflow` は `workflow.enabled`、`miniapp-functions` は `gateway.enabled`）。
/// 「このプロセスが spawn したコンシューマ集合」を GC に渡すと、片方のフラグが off のレプリカが
/// **もう片方が未配送のイベントを削除してしまう＝イベント喪失**になる。永続化された登録台帳だけが
/// 「この配備で配送を待つべきコンシューマ」を正しく表す（#413）。
pub async fn registered_consumers(conn: &mut PgConnection) -> Result<Vec<String>, StorageError> {
    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM outbox_consumer ORDER BY name")
        .fetch_all(conn)
        .await?;
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(id: i64) -> OutboxEvent {
        OutboxEvent {
            id,
            org: String::new(),
            tenant_id: String::new(),
            node_id: uuid::Uuid::nil(),
            version: 0,
            op: "create".into(),
            actor: String::new(),
            trace_id: None,
            payload: serde_json::Value::Null,
            created_at: chrono::Utc::now(),
            system: false,
        }
    }

    fn probe(seq: i64, xmin: i64, xmax: i64) -> ClaimProbe {
        ClaimProbe { seq, xmin, xmax }
    }

    fn after(plan: ClaimPlan) -> i64 {
        match plan {
            ClaimPlan::Scan { after } => after,
            ClaimPlan::Skip => panic!("走査するはず"),
        }
    }

    #[test]
    fn hints_raise_the_lower_bound_and_never_lower_it() {
        let h = ClaimHints::default();
        // 採番が動き続けている（アイドルではない）。
        let p = |s| probe(s, 1, 2);
        assert_eq!(after(h.plan("k", p(10))), 0, "最初は全走査");
        h.observe("k", p(10), 0, &[ev(5), ev(9), ev(7)], 3);
        assert_eq!(after(h.plan("k", p(20))), 9, "溜まっている間はテールから");
        h.observe("k", p(20), 9, &[ev(3), ev(4), ev(2)], 3);
        assert_eq!(after(h.plan("k", p(30))), 9, "下限は下げない");
        assert_eq!(
            after(h.plan("other", p(30))),
            0,
            "キーごと（プール・コンシューマ）"
        );
        h.observe("k", p(30), 9, &[ev(12)], 3);
        assert_eq!(after(h.plan("k", p(40))), 0, "溜まっていなければ全走査");
    }

    #[test]
    fn idle_consumer_stops_scanning_until_a_new_id_is_allocated() {
        let h = ClaimHints::default();
        // 1 回目: 何も無い。前回の材料が無いので確定しない。
        assert_eq!(after(h.plan("k", probe(50, 100, 100))), 0);
        h.observe("k", probe(50, 100, 100), 0, &[], 10);
        // 2 回目: 採番は動かず、前回実行中だった Tx（xid < 100）は終わった → 50 以下は配送済み。
        assert_eq!(after(h.plan("k", probe(50, 100, 101))), 0);
        h.observe("k", probe(50, 100, 101), 0, &[], 10);
        assert_eq!(
            h.plan("k", probe(50, 101, 101)),
            ClaimPlan::Skip,
            "走査しない"
        );
        assert_eq!(h.plan("k", probe(50, 105, 107)), ClaimPlan::Skip);
        // 新しい書込（採番が動いた）→ 確定済みの下限から走査する。
        assert_eq!(after(h.plan("k", probe(52, 107, 108))), 50);
        h.observe("k", probe(52, 107, 108), 50, &[ev(51), ev(52)], 10);
        // 行を取ったバッチでは確定しない（巻き戻っても取りこぼさない）。
        assert_eq!(after(h.plan("k", probe(52, 108, 108))), 50);
        h.observe("k", probe(52, 108, 108), 50, &[], 10);
        assert_eq!(h.plan("k", probe(52, 108, 109)), ClaimPlan::Skip);
    }

    #[test]
    fn idle_is_not_settled_while_an_older_transaction_may_still_commit() {
        let h = ClaimHints::default();
        h.observe("k", probe(50, 90, 100), 0, &[], 10);
        // 前回実行中だった xid 95 の Tx がまだ終わっていない（xmin=95 < 前回の xmax=100）。その Tx が
        // 50 以下の id を持っていて後からコミットするかもしれないので、確定しない。
        h.observe("k", probe(50, 95, 101), 0, &[], 10);
        assert_eq!(after(h.plan("k", probe(50, 95, 101))), 0, "走査を続ける");
        // 下限からではなくテールから走査した空振りでも確定しない。
        let h = ClaimHints::default();
        h.observe("k", probe(10, 1, 2), 0, &[ev(1), ev(2)], 2);
        h.observe("k", probe(10, 2, 2), 2, &[], 2);
        assert_ne!(h.plan("k", probe(10, 2, 2)), ClaimPlan::Skip);
    }

    #[test]
    fn full_sweep_runs_periodically_even_when_idle() {
        let h = ClaimHints::default();
        let t0 = std::time::Instant::now();
        h.observe("k", probe(5, 1, 1), 0, &[], 10);
        h.observe("k", probe(5, 1, 2), 0, &[], 10);
        assert_eq!(h.plan_at("k", probe(5, 1, 1), t0), ClaimPlan::Skip);
        let later = t0 + CLAIM_FULL_SWEEP_INTERVAL + std::time::Duration::from_secs(1);
        assert_eq!(
            h.plan_at("k", probe(5, 1, 1), later),
            ClaimPlan::Scan { after: 0 },
            "安全網の全走査"
        );
    }
}
