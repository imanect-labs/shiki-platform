//! 知識スコープの検証（Task 6.8 受け入れ条件）。
//!
//! 実 Postgres のみ必要（`STORAGE_TEST_DATABASE_URL`）。authz は台本フェイク:
//! - スコープを設定すると検索範囲がそのフォルダ/ファイルに絞られる
//! - スコープ内でも個人に閲覧権限のない文書は引用に現れない（post-filter 不変）
//! - スコープ未設定時は従来通り全可読範囲を検索する
//! - TenantOnly 縮退（可読集合 overflow）でもスコープ句は維持される

// テストコード: pedantic/安全系 lint は本番コードのみ厳格化する方針のため許容する。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic
)]

mod common;

use std::collections::HashSet;
use std::sync::Arc;

use authz::AuthContext;
use common::{fake_vector, test_ctx, FakeEmbedder, FakeReranker, FakeVectorStore, ScriptedAuthz};
use rag::vector_store::{ChunkPoint, VectorStore};
use rag::{RagConfig, SearchMode, SearchScope, SearchService, TantivyFulltext};
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::audit::AuditRecorder;
use uuid::Uuid;

struct Env {
    pool: PgPool,
    ctx: AuthContext,
    vector: Arc<FakeVectorStore>,
    _tmp: tempfile::TempDir,
}

async fn setup() -> Option<Env> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    Some(Env {
        pool,
        ctx: test_ctx(&tenant, "alice"),
        vector: Arc::new(FakeVectorStore::default()),
        _tmp: tempfile::tempdir().unwrap(),
    })
}

fn service(env: &Env, authz: ScriptedAuthz, config: RagConfig) -> SearchService {
    SearchService::new(
        env.pool.clone(),
        config,
        Arc::new(FakeEmbedder),
        Arc::new(FakeReranker),
        Arc::clone(&env.vector) as _,
        Arc::new(TantivyFulltext::new(env._tmp.path())),
        Arc::new(authz),
        AuditRecorder::new(env.pool.clone()),
    )
}

/// node＋rag_chunk 行を直接作り、FakeVectorStore にベクタを積む（search_scripted_it と同型）。
async fn seed_chunk(env: &Env, name: &str, query: &str, weight: f32, folder_id: Uuid) -> Uuid {
    let ctx = &env.ctx;
    let sha = format!("{:0>64}", Uuid::new_v4().simple().to_string());
    sqlx::query(
        "insert into blob (tenant_id, org, sha256, size_bytes, content_type, object_key, refcount) \
         values ($1, $2, $3, 10, 'text/plain', $4, 1)",
    )
    .bind(&ctx.tenant_id)
    .bind(&ctx.org)
    .bind(&sha)
    .bind(format!("{}/{}/{}", ctx.tenant_id, ctx.org, sha))
    .execute(&env.pool)
    .await
    .unwrap();
    let node_id: Uuid = sqlx::query_scalar(
        "insert into node (org, tenant_id, kind, name, blob_sha256, size_bytes, content_type, \
                           created_by) \
         values ($1, $2, 'file', $3, $4, 10, 'text/plain', $5) returning id",
    )
    .bind(&ctx.org)
    .bind(&ctx.tenant_id)
    .bind(name)
    .bind(&sha)
    .bind(&ctx.principal.id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    let chunk_id = Uuid::new_v5(&node_id, b"leaf-0");
    // 構造タグ: file 自身＋所属フォルダ（本物のインジェストと同じ形）。
    let tags = vec![
        ctx.ns().file(&node_id.to_string()).as_str().to_string(),
        ctx.ns().folder(&folder_id.to_string()).as_str().to_string(),
    ];
    sqlx::query(
        "insert into rag_chunk \
             (id, tenant_id, org, node_id, version, parent_id, kind, ordinal, page, heading_path, \
              content, char_count, authz_tags, embedding_model_version) \
         values ($1, $2, $3, $4, 1, null, 'leaf', 0, 1, '{}', $5, 10, $6, 'fake-model')",
    )
    .bind(chunk_id)
    .bind(&ctx.tenant_id)
    .bind(&ctx.org)
    .bind(node_id)
    .bind(format!("{name} の本文"))
    .bind(&tags)
    .execute(&env.pool)
    .await
    .unwrap();
    let query_vec = fake_vector(query);
    env.vector
        .upsert(
            ctx,
            &[ChunkPoint {
                chunk_id,
                node_id,
                version: 1,
                vector: query_vec.iter().map(|x| x * weight).collect(),
                authz_tags: tags,
            }],
        )
        .await
        .unwrap();
    node_id
}

/// 全チャンク可読の台本 authz（フォルダ 2 つの構造タグを可読集合に持つ）。
fn allow_all_authz(env: &Env, folders: &[Uuid]) -> ScriptedAuthz {
    ScriptedAuthz {
        readable_folders: folders
            .iter()
            .map(|f| env.ctx.ns().folder(&f.to_string()).as_str().to_string())
            .collect(),
        readable_files: vec![],
        denied_files: HashSet::new(),
    }
}

#[tokio::test]
async fn scope_narrows_search_and_none_keeps_full_range() {
    let Some(env) = setup().await else { return };
    let query = "四半期売上";
    let folder_a = Uuid::new_v4();
    let folder_b = Uuid::new_v4();
    let in_scope = seed_chunk(&env, "in-scope", query, 0.9, folder_a).await;
    let out_scope = seed_chunk(&env, "out-scope", query, 0.8, folder_b).await;

    // スコープ未設定 → 全可読範囲（両方ヒット・受け入れ条件③）。
    let svc = service(
        &env,
        allow_all_authz(&env, &[folder_a, folder_b]),
        RagConfig::default(),
    );
    let output = svc
        .search(&env.ctx, query, Some(5), SearchMode::Dense, None, None)
        .await
        .unwrap();
    let files: HashSet<Uuid> = output.results.iter().map(|r| r.file_id).collect();
    assert!(files.contains(&in_scope) && files.contains(&out_scope));

    // フォルダスコープ → 配下のみ（受け入れ条件①）。
    let scope = SearchScope {
        folders: vec![folder_a],
        files: vec![],
    };
    let output = svc
        .search(
            &env.ctx,
            query,
            Some(5),
            SearchMode::Dense,
            Some(&scope),
            None,
        )
        .await
        .unwrap();
    let files: HashSet<Uuid> = output.results.iter().map(|r| r.file_id).collect();
    assert!(
        files.contains(&in_scope),
        "スコープ内のファイルはヒットする"
    );
    assert!(
        !files.contains(&out_scope),
        "スコープ外のファイルはヒットしない"
    );

    // 個別ファイルスコープでも絞れる。
    let scope = SearchScope {
        folders: vec![],
        files: vec![out_scope],
    };
    let output = svc
        .search(
            &env.ctx,
            query,
            Some(5),
            SearchMode::Dense,
            Some(&scope),
            None,
        )
        .await
        .unwrap();
    let files: HashSet<Uuid> = output.results.iter().map(|r| r.file_id).collect();
    assert_eq!(files, HashSet::from([out_scope]));
}

#[tokio::test]
async fn scope_never_widens_post_filter_still_applies() {
    let Some(env) = setup().await else { return };
    let query = "極秘資料";
    let folder = Uuid::new_v4();
    let readable = seed_chunk(&env, "readable", query, 0.9, folder).await;
    let unreadable = seed_chunk(&env, "unreadable", query, 0.8, folder).await;

    // スコープ内でも本人が読めないファイルは post-filter で落ちる（受け入れ条件②）。
    let authz = ScriptedAuthz {
        readable_folders: vec![env
            .ctx
            .ns()
            .folder(&folder.to_string())
            .as_str()
            .to_string()],
        readable_files: vec![],
        denied_files: HashSet::from([env
            .ctx
            .ns()
            .file(&unreadable.to_string())
            .as_str()
            .to_string()]),
    };
    let scope = SearchScope {
        folders: vec![folder],
        files: vec![],
    };
    let output = service(&env, authz, RagConfig::default())
        .search(
            &env.ctx,
            query,
            Some(5),
            SearchMode::Dense,
            Some(&scope),
            None,
        )
        .await
        .unwrap();
    let files: HashSet<Uuid> = output.results.iter().map(|r| r.file_id).collect();
    assert!(files.contains(&readable));
    assert!(
        !files.contains(&unreadable),
        "スコープは権限を広げない（個人 ReBAC 再チェック）"
    );

    // 監査にスコープが記録される（Task 6.12）。
    let scoped_audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log \
         WHERE tenant_id = $1 AND action = 'rag.search' AND metadata ? 'scope'",
    )
    .bind(&env.ctx.tenant_id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert!(scoped_audits >= 1, "rag.search 監査に scope が残ること");
}

#[tokio::test]
async fn tenant_only_degradation_keeps_scope_clause() {
    let Some(env) = setup().await else { return };
    let query = "月次報告";
    let folder_a = Uuid::new_v4();
    let folder_b = Uuid::new_v4();
    let in_scope = seed_chunk(&env, "in-scope-d", query, 0.9, folder_a).await;
    let out_scope = seed_chunk(&env, "out-scope-d", query, 0.8, folder_b).await;

    // readable_tags_max=0 → 可読集合 overflow → TenantOnly 縮退。スコープ句は維持される。
    let config = RagConfig {
        enabled: true,
        readable_tags_max: 0,
        ..RagConfig::default()
    };
    let scope = SearchScope {
        folders: vec![folder_a],
        files: vec![],
    };
    let output = service(&env, allow_all_authz(&env, &[folder_a, folder_b]), config)
        .search(
            &env.ctx,
            query,
            Some(5),
            SearchMode::Dense,
            Some(&scope),
            None,
        )
        .await
        .unwrap();
    assert_eq!(output.debug.prefilter_mode, "tenant_only");
    let files: HashSet<Uuid> = output.results.iter().map(|r| r.file_id).collect();
    assert!(files.contains(&in_scope));
    assert!(
        !files.contains(&out_scope),
        "縮退時もスコープは狭める方向に効き続ける"
    );
}
