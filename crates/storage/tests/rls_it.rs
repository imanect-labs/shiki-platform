//! テナント表の RLS（migration 0114・移行モード）とテナント文脈付き実行器の結合テスト（#1067）。
//!
//! superuser は RLS を迂回するので、ここでは実アプリと同じ形の**非 superuser のログインロール**
//! （`shiki_runtime` のメンバ）を作って接続し、[`storage::db_tenant`] の実行器越しに確かめる:
//!
//! - テナント表は漏れなく RLS が ENABLE / FORCE され、ポリシーが付いている（新しい表の付け忘れを落とす）
//! - テナント文脈を設定したトランザクションは、そのテナントの行しか読めず、他テナントの行を書けない
//! - 文脈が未設定なら従来どおり通る（移行モード）。一度設定した接続が空文字に戻っても未設定として扱う
//! - 文脈はトランザクション限りで、プールに返した接続に残らない
//! - 横断用の実行器だけが RLS を迂回でき、実行時ロールは自分では迂回できない
//!
//! `STORAGE_TEST_DATABASE_URL`（所有者の接続）未設定ならスキップ。

// テストコード: pedantic/安全系 lint は本番コードのみ厳格化する方針のため許容する。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic
)]

use std::str::FromStr;

use authz::TenantId;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{PgPool, Row};
use storage::db_tenant::{
    begin_cross_tenant_tx, begin_tenant_tx, set_tenant, CROSS_TENANT_APP_NAME_PREFIX,
    MAINTENANCE_ROLE,
};

/// テスト用のログインロール（`shiki_runtime` のメンバ）。実アプリの `shiki_app` と同じ形。
const APP_ROLE: &str = "shiki_rls_it";
const PERMISSION_DENIED: &str = "42501";

/// RLS の対象外にしているテナント表（migration 0114 の明示リストと一致させる）。`data_*` は別の段で扱う。
/// audit_log・storage_event_outbox は 0115 で対象に入れた（#1074）。
const EXCLUDED: &[&str] = &["outbox_delivery_seq"];

struct Env {
    /// 所有者（superuser）の接続。RLS を迂回するので、行の準備と後片付けに使う。
    owner: PgPool,
    /// 非 superuser のログインロールの接続（1 本だけ。接続の使い回しを確かめるため）。
    app: PgPool,
    a: TenantId,
    b: TenantId,
}

async fn setup() -> Option<Env> {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let owner = PgPoolOptions::new()
        .max_connections(2)
        .connect(&db_url)
        .await
        .expect("connect");
    sqlx::migrate!("../../migrations")
        .run(&owner)
        .await
        .expect("migrate");
    // ロールはクラスタ共通なので冪等に作る。テストは並行に走るので、作成と付与は advisory lock で
    // 直列にし、パスワードは接続先 URL から決定的に作る（全テストで同じ値。リポジトリには書かない）。
    let password = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        db_url.hash(&mut h);
        format!("rls{:016x}", h.finish())
    };
    let mut tx = owner.begin().await.expect("begin");
    sqlx::query("SELECT pg_advisory_xact_lock(1067)")
        .execute(&mut *tx)
        .await
        .expect("lock");
    sqlx::query(&format!(
        "DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '{APP_ROLE}') THEN \
             CREATE ROLE {APP_ROLE} LOGIN NOSUPERUSER NOBYPASSRLS; \
           END IF; \
         END $$"
    ))
    .execute(&mut *tx)
    .await
    .expect("create role");
    sqlx::query(&format!(
        "ALTER ROLE {APP_ROLE} WITH LOGIN PASSWORD '{password}'"
    ))
    .execute(&mut *tx)
    .await
    .expect("password");
    sqlx::query(&format!("GRANT shiki_runtime TO {APP_ROLE}"))
        .execute(&mut *tx)
        .await
        .expect("grant");
    tx.commit().await.expect("commit");
    let opts = PgConnectOptions::from_str(&db_url)
        .expect("url")
        .username(APP_ROLE)
        .password(&password);
    let app = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .expect("connect as app role");
    let tag = uuid::Uuid::new_v4().simple().to_string();
    let a = TenantId::parse(format!("rlsa{}", &tag[..12])).unwrap();
    let b = TenantId::parse(format!("rlsb{}", &tag[..12])).unwrap();
    for t in [&a, &b] {
        sqlx::query(
            "INSERT INTO tenant_placement (tenant_id, tenant_int) VALUES ($1, nextval('tenant_int_seq'))",
        )
        .bind(t.as_str())
        .execute(&owner)
        .await
        .expect("seed");
    }
    Some(Env { owner, app, a, b })
}

impl Env {
    /// 両テナントの行のうち、この接続から見えるもの。
    async fn visible(&self, conn: &mut sqlx::PgConnection) -> Vec<String> {
        let rows = sqlx::query(
            "SELECT tenant_id FROM tenant_placement WHERE tenant_id IN ($1, $2) ORDER BY tenant_id",
        )
        .bind(self.a.as_str())
        .bind(self.b.as_str())
        .fetch_all(conn)
        .await
        .expect("select");
        rows.iter().map(|r| r.get::<String, _>(0)).collect()
    }

    fn both(&self) -> Vec<String> {
        vec![self.a.to_string(), self.b.to_string()]
    }

    async fn cleanup(self) {
        sqlx::query("DELETE FROM tenant_placement WHERE tenant_id IN ($1, $2)")
            .bind(self.a.as_str())
            .bind(self.b.as_str())
            .execute(&self.owner)
            .await
            .expect("cleanup");
    }
}

/// migrations/ の SQL が `CREATE TABLE` する表の名前（小文字）。
fn migration_tables() -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let mut names = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(&dir).expect("migrations") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("sql") {
            continue;
        }
        names.extend(created_tables(
            &std::fs::read_to_string(&path).expect("read"),
        ));
    }
    names.into_iter().collect()
}

/// SQL が `CREATE TABLE [IF NOT EXISTS] [public.]<name>` する表の名前（小文字）。
fn created_tables(sql: &str) -> Vec<String> {
    // 空白（改行・タブを含む）を 1 つの空白に畳んでから探す（`CREATE\nTABLE` の書き方も拾う）。
    let sql = sql
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut out = Vec::new();
    let mut rest = sql.as_str();
    while let Some(at) = rest.find("create table") {
        rest = &rest[at + "create table".len()..];
        let mut words = rest.split_whitespace();
        let mut name = words.next().unwrap_or_default();
        if name == "if" {
            // IF NOT EXISTS
            words.next();
            words.next();
            name = words.next().unwrap_or_default();
        }
        let name: String = name
            .trim_start_matches("public.")
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            out.push(name);
        }
    }
    out
}

#[test]
fn created_tables_handles_line_breaks_tabs_and_qualifiers() {
    let sql =
        "CREATE\nTABLE a (tenant_id text);\ncreate\ttable IF NOT EXISTS\n  public.b(\n id int);\n\
               CREATE TABLE c AS SELECT 1;\n-- create unique index d on a (x);\n";
    assert_eq!(created_tables(sql), vec!["a", "b", "c"]);
}

fn sqlstate(err: &sqlx::Error) -> Option<String> {
    err.as_database_error()
        .and_then(|d| d.code())
        .map(|c| c.into_owned())
}

#[tokio::test]
async fn every_tenant_table_has_forced_rls_with_the_isolation_policy() {
    let Some(env) = setup().await else { return };
    // migration の導出式に頼らず、`tenant_id` 列を持つ表を（NULL 可・パーティション親も含めて）全部挙げ、
    // 明示の対象外リストに無いのに RLS・FORCE・ポリシーのどれかが欠けた表を落とす。
    let excluded: Vec<String> = EXCLUDED.iter().map(|s| (*s).to_string()).collect();
    // 共有の DB には、結合テストが実行時に作る表（durable_it の durable_test_* 等）も同居する。
    // 対象は migrations/ が作る表に限る。
    let migrated = migration_tables();
    assert!(
        migrated.len() > 100,
        "migrations から表を読めていない: {}",
        migrated.len()
    );
    let missing: Vec<String> = sqlx::query_scalar(
        "SELECT c.relname::text FROM pg_class c \
           JOIN pg_namespace n ON n.oid = c.relnamespace \
           JOIN pg_attribute a ON a.attrelid = c.oid \
          WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p') AND NOT c.relispartition \
            AND c.relname NOT LIKE 'data\\_%' AND NOT (c.relname = ANY($1)) \
            AND c.relname = ANY($2) \
            AND a.attname = 'tenant_id' AND NOT a.attisdropped \
            AND NOT (c.relrowsecurity AND c.relforcerowsecurity AND EXISTS ( \
                  SELECT FROM pg_policy p \
                   WHERE p.polrelid = c.oid AND p.polname = 'shiki_tenant_isolation')) \
          ORDER BY 1",
    )
    .bind(&excluded)
    .bind(&migrated)
    .fetch_all(&env.owner)
    .await
    .unwrap();
    assert!(
        missing.is_empty(),
        "RLS が付いていないテナント表がある（0114 と同じ ENABLE / FORCE / POLICY を付けるか、\
         理由を書いて対象外リストに足すこと）: {missing:?}"
    );
    let covered: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_policy WHERE polname = 'shiki_tenant_isolation'",
    )
    .fetch_one(&env.owner)
    .await
    .unwrap();
    assert!(covered >= 60, "対象の表が想定より少ない: {covered}");
    env.cleanup().await;
}

#[tokio::test]
async fn rls_tables_have_no_index_that_rls_would_disable() {
    let Some(env) = setup().await else { return };
    // RLS の付いた表では、leakproof でない演算子・関数を含む条件はインデックス条件に使えない
    // （ポリシーより先に評価できないため）。式インデックスと、leakproof でない演算子でしか引けない
    // インデックス（jsonb の GIN 等）は、文脈が未設定でも効かなくなる。そういうインデックスを RLS の
    // 付いた表に足したら落とす（生成列＋btree にするか、表を対象外にする判断を促す）。
    let bad: Vec<String> = sqlx::query_scalar(
        "WITH scoped AS (SELECT polrelid AS rel FROM pg_policy WHERE polname = 'shiki_tenant_isolation'), \
         idx AS ( \
           SELECT i.indexrelid, c.relname AS index_name, i.indexprs IS NOT NULL AS has_expr, i.indclass \
             FROM pg_index i JOIN scoped s ON s.rel = i.indrelid \
             JOIN pg_class c ON c.oid = i.indexrelid) \
         SELECT x.index_name::text FROM idx x \
          WHERE x.has_expr OR EXISTS ( \
            SELECT FROM unnest(x.indclass::oid[]) AS oc(opc) \
              JOIN pg_opclass opc ON opc.oid = oc.opc \
              JOIN pg_amop ao ON ao.amopfamily = opc.opcfamily AND ao.amoppurpose = 's' \
                   AND ao.amoplefttype = opc.opcintype AND ao.amoprighttype = opc.opcintype \
              JOIN pg_operator o ON o.oid = ao.amopopr \
              JOIN pg_proc p ON p.oid = o.oprcode \
             WHERE NOT p.proleakproof) \
          ORDER BY 1",
    )
    .fetch_all(&env.owner)
    .await
    .unwrap();
    assert!(
        bad.is_empty(),
        "RLS の付いた表に、RLS 下で使えなくなるインデックスがある: {bad:?}"
    );
    env.cleanup().await;
}

#[tokio::test]
async fn tenant_tx_reads_and_writes_only_its_own_tenant() {
    let Some(env) = setup().await else { return };
    let mut tx = begin_tenant_tx(&env.app, &env.a).await.unwrap();
    assert_eq!(tx.tenant(), &env.a);
    assert_eq!(env.visible(&mut tx).await, vec![env.a.to_string()]);

    // 他テナントの行は UPDATE / DELETE の対象に入らない（WHERE で指しても 0 行）。
    let updated = sqlx::query("UPDATE tenant_placement SET pool_id = 'moved' WHERE tenant_id = $1")
        .bind(env.b.as_str())
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(updated.rows_affected(), 0);
    let deleted = sqlx::query("DELETE FROM tenant_placement WHERE tenant_id = $1")
        .bind(env.b.as_str())
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(deleted.rows_affected(), 0);
    tx.commit().await.unwrap();

    // 他テナントの tenant_id を持つ行は書けない（WITH CHECK 違反）。
    let mut tx = begin_tenant_tx(&env.app, &env.a).await.unwrap();
    let err = sqlx::query(
        "INSERT INTO tenant_placement (tenant_id, tenant_int) VALUES ($1, nextval('tenant_int_seq'))",
    )
    .bind(format!("{}x", env.b))
    .execute(&mut *tx)
    .await
    .expect_err("他テナントの行は INSERT できない");
    assert_eq!(sqlstate(&err).as_deref(), Some(PERMISSION_DENIED), "{err}");
    tx.rollback().await.unwrap();

    // 自テナントの行の tenant_id を他テナントへ書き換えることもできない。
    let mut tx = begin_tenant_tx(&env.app, &env.a).await.unwrap();
    let err = sqlx::query("UPDATE tenant_placement SET tenant_id = $2 WHERE tenant_id = $1")
        .bind(env.a.as_str())
        .bind(format!("{}y", env.b))
        .execute(&mut *tx)
        .await
        .expect_err("行を他テナントへ移せない");
    assert_eq!(sqlstate(&err).as_deref(), Some(PERMISSION_DENIED), "{err}");
    tx.rollback().await.unwrap();

    let pool_b: String =
        sqlx::query_scalar("SELECT pool_id FROM tenant_placement WHERE tenant_id = $1")
            .bind(env.b.as_str())
            .fetch_one(&env.owner)
            .await
            .unwrap();
    assert_eq!(pool_b, "default", "他テナントの行は変わっていない");
    env.cleanup().await;
}

#[tokio::test]
async fn unset_context_is_permissive_and_does_not_leak_across_checkouts() {
    let Some(env) = setup().await else { return };
    // 移行モード: 文脈が未設定なら両テナントが見える（既存の SQL は従来どおり動く）。
    let mut conn = env.app.acquire().await.unwrap();
    assert_eq!(env.visible(&mut conn).await, env.both());
    drop(conn);

    // 同じ接続（プールは 1 本）で文脈を設定してコミットしたあと、次の利用者には残らない。
    // 一度設定した接続では値が空文字に戻るが、これも未設定として扱う。
    let mut tx = begin_tenant_tx(&env.app, &env.a).await.unwrap();
    assert_eq!(env.visible(&mut tx).await, vec![env.a.to_string()]);
    tx.commit().await.unwrap();
    let mut conn = env.app.acquire().await.unwrap();
    let raw: Option<String> = sqlx::query_scalar("SELECT current_setting('app.tenant_id', true)")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(
        raw.as_deref(),
        Some(""),
        "コミット後は空文字に戻る（NULL ではない）"
    );
    assert_eq!(env.visible(&mut conn).await, env.both());
    drop(conn);

    // ロールバックでも同じ。
    let mut tx = begin_tenant_tx(&env.app, &env.b).await.unwrap();
    assert_eq!(env.visible(&mut tx).await, vec![env.b.to_string()]);
    tx.rollback().await.unwrap();
    let mut conn = env.app.acquire().await.unwrap();
    assert_eq!(env.visible(&mut conn).await, env.both());
    drop(conn);
    env.cleanup().await;
}

#[tokio::test]
async fn set_tenant_scopes_an_already_open_transaction() {
    let Some(env) = setup().await else { return };
    let mut tx = env.app.begin().await.unwrap();
    assert_eq!(env.visible(&mut tx).await, env.both());
    set_tenant(&mut tx, &env.b).await.unwrap();
    assert_eq!(env.visible(&mut tx).await, vec![env.b.to_string()]);
    tx.rollback().await.unwrap();
    env.cleanup().await;
}

#[tokio::test]
async fn only_the_cross_tenant_tx_bypasses_rls() {
    let Some(env) = setup().await else { return };
    // 実行時ロールは BYPASSRLS を持たず、row_security を切って迂回することもできない。
    let mut tx = begin_tenant_tx(&env.app, &env.a).await.unwrap();
    let err = sqlx::query("SET LOCAL row_security = off")
        .execute(&mut *tx)
        .await
        .map(|_| ());
    match err {
        // 設定できても、ポリシーが効く表を読むとエラーになる（迂回はできない）。
        Ok(()) => {
            let read = sqlx::query("SELECT 1 FROM tenant_placement LIMIT 1")
                .execute(&mut *tx)
                .await
                .expect_err("row_security = off では読めない");
            assert_eq!(
                sqlstate(&read).as_deref(),
                Some(PERMISSION_DENIED),
                "{read}"
            );
        }
        Err(e) => panic!("row_security の設定自体は失敗しないはず: {e}"),
    }
    tx.rollback().await.unwrap();

    // 横断用の実行器はテナント文脈を設定しても全テナントが見える（BYPASSRLS）。
    let mut tx = begin_cross_tenant_tx(&env.app, "rls_it").await.unwrap();
    let who: String = sqlx::query_scalar("SELECT current_user::text")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(who, MAINTENANCE_ROLE);
    let app_name: String = sqlx::query_scalar("SELECT current_setting('application_name')")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(app_name, format!("{CROSS_TENANT_APP_NAME_PREFIX}rls_it"));
    assert_eq!(tx.purpose(), "rls_it");
    sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
        .bind(env.a.as_str())
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(env.visible(&mut tx).await, env.both());
    tx.commit().await.unwrap();

    // コミット後は元のロールに戻る（プールの次の利用者は迂回できない）。
    let mut conn = env.app.acquire().await.unwrap();
    let who: String = sqlx::query_scalar("SELECT current_user::text")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(who, APP_ROLE);
    let bypass: bool =
        sqlx::query_scalar("SELECT rolbypassrls FROM pg_roles WHERE rolname = current_user")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert!(!bypass);
    drop(conn);
    env.cleanup().await;
}

#[tokio::test]
async fn audit_and_outbox_side_columns_follow_their_rows_under_rls() {
    let Some(env) = setup().await else { return };
    // 実行時ロールのテナント文脈の下で書く（0115 のトリガは呼び出し側の権限で動く）。
    let mut tx = begin_tenant_tx(&env.app, &env.a).await.unwrap();
    let audit_id: i64 = sqlx::query_scalar(
        "INSERT INTO audit_log (tenant_id, org, actor, action, object_type, object_id, decision, metadata) \
         VALUES ($1, 'o1', 'u', 'rag.search', 'query', 'q', 'allow', \
                 '{\"thread_id\":\"th-9\",\"message_id\":\"m-9\",\"cited_chunk_ids\":[\"c-1\",\"c-2\",\"c-1\",null]}') \
         RETURNING id",
    )
    .bind(env.a.as_str())
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let (thread, message): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT meta_thread_id, meta_message_id FROM audit_log WHERE id = $1")
            .bind(audit_id)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(thread.as_deref(), Some("th-9"));
    assert_eq!(message.as_deref(), Some("m-9"));
    let cited: Vec<String> = sqlx::query_scalar(
        "SELECT chunk_id FROM audit_cited_chunk WHERE audit_id = $1 ORDER BY chunk_id",
    )
    .bind(audit_id)
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    assert_eq!(
        cited,
        vec!["c-1", "c-2"],
        "重複は 1 行にまとまり、null の要素は写さない"
    );

    let event_id: i64 = sqlx::query_scalar(
        "INSERT INTO storage_event_outbox (org, tenant_id, node_id, version, op, actor, payload) \
         VALUES ('o1', $1, gen_random_uuid(), 1, 'update', 'u', \
                 '{\"event_type\":\"data.record.changed\",\"table_id\":\"tb-1\"}') RETURNING id",
    )
    .bind(env.a.as_str())
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let (event_type, table_id): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT payload_event_type, payload_table_id FROM storage_event_outbox WHERE id = $1",
    )
    .bind(event_id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(event_type.as_deref(), Some("data.record.changed"));
    assert_eq!(table_id.as_deref(), Some("tb-1"));
    tx.commit().await.unwrap();

    // 他テナントの文脈からは、監査ログも引用チャンクも見えない。
    let mut tx = begin_tenant_tx(&env.app, &env.b).await.unwrap();
    let seen: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM audit_log WHERE id = $1) \
              + (SELECT count(*) FROM audit_cited_chunk WHERE audit_id = $1)",
    )
    .bind(audit_id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(seen, 0);
    tx.rollback().await.unwrap();

    // 保持期間の削除と同じく親を消すと、引用チャンクも消える（ON DELETE CASCADE）。
    let mut tx = begin_tenant_tx(&env.app, &env.a).await.unwrap();
    sqlx::query("DELETE FROM audit_log WHERE id = $1")
        .bind(audit_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("DELETE FROM storage_event_outbox WHERE id = $1")
        .bind(event_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let left: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_cited_chunk WHERE audit_id = $1")
            .bind(audit_id)
            .fetch_one(&env.owner)
            .await
            .unwrap();
    assert_eq!(left, 0);
    env.cleanup().await;
}
