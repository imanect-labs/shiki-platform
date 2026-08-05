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
    sqlx::query(
        "INSERT INTO outbox_delivery (consumer, event_id, tenant_id) \
         SELECT $1, o.id, o.tenant_id \
         FROM storage_event_outbox o \
         WHERE o.id = ANY($2) \
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
/// ⚠️ **必ずトランザクション上で呼ぶこと**（`pool.begin()`）。理由が 2 つある:
/// 1. 「登録の記録」と「バックログの fast-forward」が**原子的でないと壊れる**。autocommit で呼ぶと
///    登録だけ先にコミットされ、fast-forward が失敗した場合に**登録済みなのにバックログが
///    配送済みになっていない**状態が残る → relay がバックログ全件を拾い一斉発火する。
/// 2. GC との相互排除に使う advisory lock が **txn 単位**（autocommit では文の終わりで解放される）。
///
/// pool しか手元に無い呼び出し側は [`register_consumer_on_pool`] を使うこと（自前で txn を張る）。
pub async fn register_consumer(
    conn: &mut PgConnection,
    consumer: &str,
) -> Result<u64, StorageError> {
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
    super::gc::lock_for_fast_forward(&mut *conn).await?;
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
