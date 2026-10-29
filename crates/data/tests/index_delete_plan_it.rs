//! 索引行の削除の計画（検証 D-6・#915・#961・#986・実 PG）。
//!
//! バックフィルや更新は、(列, レコード) の組の索引行を消してから作り直す。新しい列や作ったばかりの表は
//! 統計に載っていないため、Postgres が値索引 (tenant_int, table_int, field_id, val, record_int) を選び、
//! 組ごとにその列の索引行を全部読み直すことがあった（30 万行で 1 回 3.5 秒）。削除文はこのテーブルの
//! パーティションへ直接向け、組ごとの探索ではテナントとテーブルの値を計画時に見せない（#986）。
//! ここでは実際にその文を流し、**このトランザクションの索引探索の回数**（`pg_stat_get_xact_numscans`）と
//! ロックで次を確かめる。
//!
//! - 組ごとの探索は主キーの 4 列等値で、組の数ちょうど（列の全行を読まない）。ほかの探索は、
//!   パーティションを引く 1 回だけ（値索引で組ごとに引かない・外側は TID で消す・#961）。
//! - ロックするパーティションは 1 つだけ（generic plan のように全パーティションをロックしない）。
//!
//! 統計（ANALYZE）と主キーの fillfactor を書き換えるので、他のテストと計画が干渉しないよう独立した
//! テストバイナリにする。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

mod common;

const TENANT: i32 = 2_000_000_001;
const OTHER_TENANT: i32 = 2_000_000_002;
const TABLE: i64 = 9_000_000_001;

/// このテーブルの行が入っている `data_index_text` のパーティション（同じ table_int は 1 パーティション）。
async fn partition(pool: &sqlx::PgPool) -> String {
    sqlx::query_scalar(
        "SELECT tableoid::regclass::text FROM data_index_text WHERE table_int = $1 LIMIT 1",
    )
    .bind(TABLE)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// パーティションの (主キー, それ以外の索引) の、この接続がまだ集計へ送っていない探索回数。
async fn index_scans(conn: &mut sqlx::PgConnection, part: &str) -> (i64, i64) {
    let rows: Vec<(bool, i64)> = sqlx::query_as(
        "SELECT c.relname LIKE '%\\_pkey', pg_stat_get_xact_numscans(c.oid) \
         FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid WHERE i.indrelid = $1::regclass",
    )
    .bind(part)
    .fetch_all(conn)
    .await
    .unwrap();
    let count = |pkey: bool| {
        rows.iter()
            .filter(|(p, _)| *p == pkey)
            .map(|(_, c)| *c)
            .sum::<i64>()
    };
    (count(true), count(false))
}

/// `data_index_text_p{n}` の n。
fn partition_number(part: &str) -> u16 {
    part.strip_prefix("data_index_text_p")
        .unwrap()
        .parse()
        .unwrap()
}

/// 削除文を 1 回流し（ロールバックする）、そのパーティションの (主キー, 値索引) の探索回数と、
/// ロックした `data_index_text` のパーティションの数を返す。
async fn delete_scans(pool: &sqlx::PgPool, fields: &[i16], records: &[i64]) -> (i64, i64, i64) {
    let part = partition(pool).await;
    let mut tx = pool.begin().await.unwrap();
    // 接続がまだ集計へ送っていない分（同じ接続の前の Tx の探索）も数に入るので、前後の差を取る。
    let before = index_scans(&mut tx, &part).await;
    sqlx::query(&data::text_index_delete_sql(Some(partition_number(&part))))
        .bind(TENANT)
        .bind(TABLE)
        .bind(fields)
        .bind(records)
        .execute(&mut *tx)
        .await
        .unwrap();
    let after = index_scans(&mut tx, &part).await;
    let locked: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT l.relation) FROM pg_locks l JOIN pg_inherits i ON i.inhrelid = l.relation \
         WHERE l.pid = pg_backend_pid() AND l.locktype = 'relation' \
           AND i.inhparent = 'data_index_text'::regclass",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    (after.0 - before.0, after.1 - before.1, locked)
}

/// 組ごとに主キーで 1 回ずつ引き、ほかの索引探索はしない。ロックは 1 パーティション。
fn assert_pkey_per_pair(scans: (i64, i64, i64), pairs: i64, state: &str) {
    let (pkey, val, locked) = scans;
    assert!(
        pkey == pairs && val == 0,
        "組ごとに主キーで 1 回ずつ引き、値索引も外側の索引探索も使わない（{state}）: \
         主キー {pkey} 回・値索引 {val} 回・組 {pairs}"
    );
    assert_eq!(locked, 1, "ロックするパーティションは 1 つだけ（{state}）");
}

async fn analyze(pool: &sqlx::PgPool, rel: &str) {
    sqlx::query(&format!("ANALYZE {rel}"))
        .execute(pool)
        .await
        .unwrap();
}

/// 主キーを fillfactor で肥大させる（値索引より大きくする）／元に戻す。
async fn bloat_pkey(pool: &sqlx::PgPool, on: bool) {
    let idx = format!("{}_pkey", partition(pool).await);
    let set = if on {
        format!("ALTER INDEX {idx} SET (fillfactor = 10)")
    } else {
        format!("ALTER INDEX {idx} RESET (fillfactor)")
    };
    sqlx::query(&set).execute(pool).await.unwrap();
    sqlx::query(&format!("REINDEX INDEX {idx}"))
        .execute(pool)
        .await
        .unwrap();
}

/// 削除は `data_record` のパーティション番号（書込の Tx で引く）を索引テーブルにも使う。全ての索引テーブルの
/// 同じ番号のパーティションが、`data_record` と同じハッシュの範囲を持つこと。
#[tokio::test]
async fn index_partitions_share_the_numbering_of_data_record() {
    let Some(pool) = common::setup().await else {
        return;
    };
    let mismatched: Vec<String> = sqlx::query_scalar(
        "SELECT c.relname::text FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid \
         WHERE i.inhparent = ANY(ARRAY['data_index_text', 'data_index_num', 'data_index_search', \
                                       'data_link', 'data_unique']::regclass[]) \
           AND pg_get_expr(c.relpartbound, c.oid) IS DISTINCT FROM ( \
               SELECT pg_get_expr(r.relpartbound, r.oid) FROM pg_class r \
                WHERE r.relname = 'data_record_p' || substring(c.relname from '_p([0-9]+)$'))",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(
        mismatched.is_empty(),
        "番号が data_record とずれている: {mismatched:?}"
    );
    let parts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_inherits WHERE inhparent = 'data_index_text'::regclass",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(parts > 1, "索引テーブルはハッシュ分割されている");
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // 統計の状態を順に作って確かめる 1 本の筋書き（状態は前の段の上に積む）。
async fn index_delete_uses_the_primary_key_even_with_stale_statistics() {
    let Some(pool) = common::setup().await else {
        return;
    };
    // 他のテストと重ならない内部キー（索引表には外部キーが無い）。同じ table_int の別テナントは
    // 同じパーティション（table_int のハッシュ）に入る。
    let cleanup = || {
        sqlx::query("DELETE FROM data_index_text WHERE tenant_int = ANY($1)")
            .bind(vec![TENANT, OTHER_TENANT])
            .execute(&pool)
    };
    cleanup().await.unwrap();
    let insert = "INSERT INTO data_index_text (tenant_int, table_int, field_id, record_int, ord, val) \
                  SELECT $1, $2, f::int2, r, 0, 'v' || (r % 1000) \
                  FROM generate_series($3::int8, $4::int8) r, generate_series($5::int4, $6::int4) f";
    let fill = |tenant: i32, lo: i64, hi: i64, f0: i32, f1: i32| {
        sqlx::query(insert)
            .bind(tenant)
            .bind(TABLE)
            .bind(lo)
            .bind(hi)
            .bind(f0)
            .bind(f1)
            .execute(&pool)
    };

    // ① 統計にこのテーブルが無く（他テナントの行だけ）、主キーが値索引より大きい（#986）。custom plan
    // では主キーと値索引の推定がどちらも 1 行の下限で並び、索引の大きさで値索引が選ばれていた。
    fill(OTHER_TENANT, 1, 2_000, 16, 16).await.unwrap();
    let part = partition(&pool).await;
    // 自動 ANALYZE が途中で統計を作り直さないようにする（終わりに戻す）。前回が途中で落ちていても
    // 主キーの肥大を戻してから始める。
    sqlx::query(&format!(
        "ALTER TABLE {part} SET (autovacuum_enabled = false)"
    ))
    .execute(&pool)
    .await
    .unwrap();
    bloat_pkey(&pool, false).await;
    analyze(&pool, &part).await;
    fill(TENANT, 1, 50_000, 16, 18).await.unwrap();
    fill(TENANT, 1, 60_000, 19, 19).await.unwrap();
    let fields: Vec<i16> = vec![16, 19];
    let records: Vec<i64> = (59_501..=60_500).collect();
    let pairs = (fields.len() * records.len()) as i64;
    bloat_pkey(&pool, true).await;
    let bloated = delete_scans(&pool, &fields, &records).await;

    // ② パーティションに列統計が全く無い（一度も ANALYZE されていない）。generic plan も値索引を
    // 選ぶので、大量に消す経路は先に統計を作る。統計の行を消せるのはスーパーユーザーだけ。
    let stats = || {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM pg_stats WHERE tablename = $1")
            .bind(&part)
            .fetch_one(&pool)
    };
    let wiped = sqlx::query("DELETE FROM pg_statistic WHERE starelid = $1::regclass")
        .bind(&part)
        .execute(&pool)
        .await;
    let reanalyzed = match wiped {
        Ok(_) => {
            let before = stats().await.unwrap();
            let pools: storage::PoolRouter = pool.clone().into();
            data::analyze_index_partitions_without_stats(&pools, "t-986", TENANT, TABLE).await;
            let after = stats().await.unwrap();
            Some((before, after, delete_scans(&pool, &fields, &records).await))
        }
        Err(e) => {
            eprintln!("pg_statistic を消せないので ② を飛ばします: {e}");
            None
        }
    };
    bloat_pkey(&pool, false).await;
    sqlx::query(&format!("ALTER TABLE {part} RESET (autovacuum_enabled)"))
        .execute(&pool)
        .await
        .unwrap();
    assert_pkey_per_pair(bloated, pairs, "統計にテーブルが無い・主キーが肥大");
    match reanalyzed {
        // 消してから確かめるまでに、他のテストの ANALYZE が統計を作り直していれば確かめられない。
        Some((before, _, _)) if before > 0 => {
            eprintln!("他の ANALYZE が先に統計を作ったので ② の確認を飛ばします");
        }
        // 他の ANALYZE がロックを持っていれば SKIP_LOCKED で飛ばす（次のチャンクで確かめ直す）。
        Some((_, 0, _)) => eprintln!("ANALYZE がロック待ちで飛ばされたので ② の確認を飛ばします"),
        Some((_, after, scans)) => {
            assert!(after > 0, "列統計の無いパーティションを ANALYZE する");
            assert_pkey_per_pair(scans, pairs, "列統計を作った後");
        }
        None => {}
    }

    // ③ 既存の列（16〜18）が少ないうちに統計を取り、その後で大きくする＋新しい列 19 を作る
    // （バックフィル中の列は統計に無い）。
    sqlx::query("DELETE FROM data_index_text WHERE tenant_int = $1")
        .bind(TENANT)
        .execute(&pool)
        .await
        .unwrap();
    fill(TENANT, 1, 300, 16, 18).await.unwrap();
    analyze(&pool, &part).await;
    fill(TENANT, 301, 100_000, 16, 18).await.unwrap();
    fill(TENANT, 1, 60_000, 19, 19).await.unwrap();
    assert_pkey_per_pair(
        delete_scans(&pool, &fields, &records).await,
        pairs,
        "列が統計に無い",
    );

    // 実際に消えるのは組の行だけ（列 16・19 × 1,000 件のうち、列 19 は 60,000 までしか無い）。
    let records: Vec<i64> = (59_501..=60_500).collect();
    let fields: Vec<i16> = vec![16, 19];
    sqlx::query(&data::text_index_delete_sql(Some(partition_number(&part))))
        .bind(TENANT)
        .bind(TABLE)
        .bind(&fields)
        .bind(&records)
        .execute(&pool)
        .await
        .unwrap();
    let left: Vec<(i16, i64)> = sqlx::query_as(
        "SELECT field_id, count(*) FROM data_index_text \
         WHERE tenant_int = $1 AND table_int = $2 AND record_int BETWEEN 59501 AND 60500 \
         GROUP BY field_id ORDER BY field_id",
    )
    .bind(TENANT)
    .bind(TABLE)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(left, vec![(17, 1_000), (18, 1_000)], "組に無い列は残る");
    let gone: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM data_index_text \
         WHERE tenant_int = $1 AND table_int = $2 AND field_id IN (16, 19)",
    )
    .bind(TENANT)
    .bind(TABLE)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(gone, 100_000 - 1_000 + 60_000 - 500, "組の行だけが消える");
    let other: i64 =
        sqlx::query_scalar("SELECT count(*) FROM data_index_text WHERE tenant_int = $1")
            .bind(OTHER_TENANT)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(other, 2_000, "同じパーティションの別テナントの行は消えない");

    cleanup().await.unwrap();
}
