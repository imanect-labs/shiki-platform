//! 内部能力の書込の冪等の手順（[`journaled`]）のテスト（実 Postgres・`STORAGE_TEST_DATABASE_URL` が無ければ
//! スキップ）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

use super::*;

async fn pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .expect("db");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    Some(pool)
}

/// 副作用の無い誤りの後に予約を外せなくても、元の誤り（権限・名前の衝突など）を返す。「一時的な問題」に置き換えると、
/// 利用者に本当の原因が出ず、やり直しても予約が残っていて effect_in_progress になる（BUG-188 の続き）。
#[tokio::test]
async fn a_failed_release_keeps_the_original_error() {
    let Some(admin) = pool().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    // 予約は別の接続の口で取り、書込の中でその口を閉じる（予約を外す DELETE が失敗する）。
    let url = std::env::var("STORAGE_TEST_DATABASE_URL").unwrap();
    let journal_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("db");
    let journal = EffectJournal::new(journal_pool.clone());
    let out = journaled(&journal, &tenant, "k1", "d1", "保存", async {
        journal_pool.close().await;
        Err::<Value, _>(PortError::forbidden(
            "このフォルダーに保存する権限がありません。",
        ))
    })
    .await;

    // 片付け（外せなかった予約の行）。
    let left: i64 = sqlx::query_scalar(
        "WITH d AS (DELETE FROM effect_journal WHERE tenant_id = $1 RETURNING 1) SELECT count(*) FROM d",
    )
    .bind(&tenant)
    .fetch_one(&admin)
    .await
    .unwrap();

    let err = out.unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.message.as_str(), err.retryable),
        (
            "forbidden",
            "このフォルダーに保存する権限がありません。",
            false
        )
    );
    assert_eq!(left, 1, "予約は外せずに残っていた（外す経路を通った）");
}
