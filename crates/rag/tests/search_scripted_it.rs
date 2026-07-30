//! SearchService の制御フロー検証（PIT-1 縮退・PIT-2 バックフィル・削除防壁）。
//!
//! 実 Postgres のみ必要（`STORAGE_TEST_DATABASE_URL`）。authz は台本フェイク
//! （ScriptedAuthz）で deny パターンを厳密に制御し、以下を検証する:
//! - post-filter で大量 deny されても**バックフィルが top_k を回復**する（PIT-2 受入条件）
//! - 可読集合の上限超過で **tenant-only へ縮退**しつつ検索が成立する（PIT-1 フォールバック）
//! - 削除済み node はインデックスに残っていてもハイドレーションで消える（第三の防壁）

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
use rag::{RagConfig, SearchMode, SearchService, TantivyFulltext};
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

/// node＋rag_chunk 行を直接作り、FakeVectorStore に指定スコアのベクタを積む。
/// クエリベクトル（`fake_vector(query)`）との内積が `weight` になるよう仕込む。
async fn seed_chunk(env: &Env, name: &str, query: &str, weight: f32, folder_tag: &str) -> Uuid {
    seed_chunk_in_org(env, name, query, weight, folder_tag, &env.ctx.org).await
}

/// `seed_chunk` の org 指定版。`env.ctx.org` 以外を渡すと、**同一テナントの別 org** の文書になる
/// （pre-filter/post-filter は通るが hydrate の org 述語で落ちる状況を作れる・#377/PIT-45）。
async fn seed_chunk_in_org(
    env: &Env,
    name: &str,
    query: &str,
    weight: f32,
    folder_tag: &str,
    org: &str,
) -> Uuid {
    let ctx = &env.ctx;
    let sha = format!("{:0>64}", Uuid::new_v4().simple().to_string());
    sqlx::query(
        "insert into blob (tenant_id, org, sha256, size_bytes, content_type, object_key, refcount) \
         values ($1, $2, $3, 10, 'text/plain', $4, 1)",
    )
    .bind(&ctx.tenant_id)
    .bind(org)
    .bind(&sha)
    .bind(format!("{}/{}/{}", ctx.tenant_id, org, sha))
    .execute(&env.pool)
    .await
    .unwrap();
    let node_id: Uuid = sqlx::query_scalar(
        // updated_by は NOT NULL（migration 0045）。作成＝最初の更新なので created_by と同値。
        "insert into node (org, tenant_id, kind, name, blob_sha256, size_bytes, content_type, \
                           created_by, updated_by) \
         values ($1, $2, 'file', $3, $4, 10, 'text/plain', $5, $5) returning id",
    )
    .bind(org)
    .bind(&ctx.tenant_id)
    .bind(name)
    .bind(&sha)
    .bind(&ctx.principal.id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    let chunk_id = Uuid::new_v5(&node_id, b"leaf-0");
    let tags = vec![
        ctx.ns().file(&node_id.to_string()).as_str().to_string(),
        folder_tag.to_string(),
    ];
    sqlx::query(
        "insert into rag_chunk \
             (id, tenant_id, org, node_id, version, parent_id, kind, ordinal, page, heading_path, \
              content, char_count, authz_tags, embedding_model_version) \
         values ($1, $2, $3, $4, 1, null, 'leaf', 0, 1, '{}', $5, 10, $6, 'fake-model')",
    )
    .bind(chunk_id)
    .bind(&ctx.tenant_id)
    .bind(org)
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

#[tokio::test]
async fn backfill_recovers_top_k_after_mass_deny() {
    let Some(env) = setup().await else { return };
    let folder_tag = env.ctx.ns().folder("shared").as_str().to_string();
    let query = "四半期売上";

    // dense スコア降順: 上位 3 件は deny 対象、下位 3 件が許可対象。
    let denied_a = seed_chunk(&env, "denied-a", query, 0.9, &folder_tag).await;
    let denied_b = seed_chunk(&env, "denied-b", query, 0.8, &folder_tag).await;
    let denied_c = seed_chunk(&env, "denied-c", query, 0.7, &folder_tag).await;
    let allowed_a = seed_chunk(&env, "allowed-a", query, 0.6, &folder_tag).await;
    let allowed_b = seed_chunk(&env, "allowed-b", query, 0.5, &folder_tag).await;
    let allowed_c = seed_chunk(&env, "allowed-c", query, 0.4, &folder_tag).await;

    let denied_files: HashSet<String> = [denied_a, denied_b, denied_c]
        .iter()
        .map(|id| env.ctx.ns().file(&id.to_string()).as_str().to_string())
        .collect();
    let authz = ScriptedAuthz {
        readable_folders: vec![folder_tag.clone()],
        readable_files: vec![],
        denied_files,
    };
    // pool_target = max(top_k=3, rerank_pool=3) = 3・over_fetch=1 → fetch_k=3。
    // 1 ラウンド目は上位 3 件が全 deny → バックフィルで残り 3 件を取得して回復する。
    let config = RagConfig {
        enabled: true,
        rerank_pool: 3,
        over_fetch_tags: 1,
        ..RagConfig::default()
    };
    let output = service(&env, authz, config)
        .search(&env.ctx, query, Some(3), SearchMode::Dense, None, None)
        .await
        .unwrap();

    // 【受入条件・PIT-2】最終引用件数が要求 top_k を下回らない。
    assert_eq!(output.results.len(), 3);
    let hit_files: HashSet<Uuid> = output.results.iter().map(|r| r.file_id).collect();
    assert_eq!(
        hit_files,
        [allowed_a, allowed_b, allowed_c]
            .into_iter()
            .collect::<HashSet<_>>(),
        "deny された上位候補は混入せず、許可候補で埋まる"
    );
    assert!(output.debug.backfill_rounds >= 2, "バックフィルが働いた");
    assert_eq!(output.debug.authz_denied_files, 3);
    assert_eq!(output.debug.prefilter_mode, "tags");
}

/// #377: 別 org の高スコアチャンクが pool を埋めても、要求 `top_k` が同 org 文書で埋まる。
///
/// org は pre-filter（`readable_set` のタグ／索引）に無く hydrate の SQL 述語で絞るため、
/// 「マルチ org テナントで別 org の file にも直接 viewer を持つ」ユーザーでは、別 org の候補が
/// pre/post-filter を通過して枠を食う。hydrate をバックフィルループ内に置き「行が引けたか」を
/// 採択条件にすることで、落ちた分を同 org 候補で埋め直す（漏洩は元々しない・fail-closed）。
#[tokio::test]
async fn backfill_recovers_top_k_when_cross_org_chunks_outrank() {
    let Some(env) = setup().await else { return };
    let folder_tag = env.ctx.ns().folder("shared").as_str().to_string();
    let query = "四半期売上";
    let other_org = format!("other-{}", Uuid::new_v4().simple());

    // dense スコア降順: 上位 3 件は **別 org**（hydrate で落ちる）、下位 3 件が自 org。
    let mut cross = Vec::new();
    for (name, weight) in [("cross-a", 0.9), ("cross-b", 0.8), ("cross-c", 0.7)] {
        cross.push(seed_chunk_in_org(&env, name, query, weight, &folder_tag, &other_org).await);
    }
    let mine_a = seed_chunk(&env, "mine-a", query, 0.6, &folder_tag).await;
    let mine_b = seed_chunk(&env, "mine-b", query, 0.5, &folder_tag).await;
    let mine_c = seed_chunk(&env, "mine-c", query, 0.4, &folder_tag).await;

    // 別 org の file にも **直接 viewer を持つ**ユーザー（これが跨ぎ viewer の稀ケース）。
    // pre-filter のタグにも post-filter の許可にも入るので、hydrate まで生き残る。
    let authz = ScriptedAuthz {
        readable_folders: vec![folder_tag.clone()],
        readable_files: cross
            .iter()
            .map(|id| env.ctx.ns().file(&id.to_string()).as_str().to_string())
            .collect(),
        denied_files: HashSet::new(),
    };
    // pool_target = max(top_k=3, rerank_pool=3) = 3・over_fetch=1 → fetch_k=3。
    // 1 ラウンド目は上位 3 件が全て別 org → バックフィルで自 org の 3 件を取得して回復する。
    let config = RagConfig {
        enabled: true,
        rerank_pool: 3,
        over_fetch_tags: 1,
        ..RagConfig::default()
    };
    let output = service(&env, authz, config)
        .search(&env.ctx, query, Some(3), SearchMode::Dense, None, None)
        .await
        .unwrap();

    assert_eq!(
        output.results.len(),
        3,
        "別 org の候補が上位を占めても top_k を下回らない（#377）"
    );
    let hit_files: HashSet<Uuid> = output.results.iter().map(|r| r.file_id).collect();
    assert_eq!(
        hit_files,
        [mine_a, mine_b, mine_c].into_iter().collect::<HashSet<_>>(),
        "自 org の文書だけで埋まる"
    );
    for id in &cross {
        assert!(
            !hit_files.contains(id),
            "別 org の文書は混入しない（org は隔離境界・PIT-45）"
        );
    }
    assert!(output.debug.backfill_rounds >= 2, "バックフィルが働いた");
    assert_eq!(
        output.debug.hydrate_dropped, 3,
        "hydrate で落ちた件数が可観測（他 org 3 件）"
    );
    assert_eq!(
        output.debug.authz_denied_files, 0,
        "post-filter では落ちていない（跨ぎ viewer なので許可される）"
    );
}

#[tokio::test]
async fn readable_set_overflow_falls_back_to_tenant_only() {
    let Some(env) = setup().await else { return };
    let folder_tag = env.ctx.ns().folder("huge-org").as_str().to_string();
    let query = "経費精算";
    let file_a = seed_chunk(&env, "keihi-a", query, 0.9, &folder_tag).await;
    let file_b = seed_chunk(&env, "keihi-b", query, 0.8, &folder_tag).await;

    // 可読集合が上限（既定 500）を超える → pre-filter 放棄・tenant-only 縮退。
    let authz = ScriptedAuthz {
        readable_folders: (0..600)
            .map(|i| format!("folder:{}|f{i}", env.ctx.tenant_id))
            .collect(),
        readable_files: vec![],
        denied_files: HashSet::new(),
    };
    let output = service(
        &env,
        authz,
        RagConfig {
            enabled: true,
            ..RagConfig::default()
        },
    )
    .search(&env.ctx, query, Some(5), SearchMode::Dense, None, None)
    .await
    .unwrap();

    assert_eq!(output.debug.prefilter_mode, "tenant_only", "縮退が起きた");
    let hit_files: HashSet<Uuid> = output.results.iter().map(|r| r.file_id).collect();
    assert_eq!(
        hit_files,
        [file_a, file_b].into_iter().collect::<HashSet<_>>(),
        "縮退時も post-filter（全許可）を経て正しく返る"
    );
}

#[tokio::test]
async fn deleted_node_is_hidden_even_if_indexes_are_stale() {
    let Some(env) = setup().await else { return };
    let folder_tag = env.ctx.ns().folder("trash").as_str().to_string();
    let query = "議事録";
    let node = seed_chunk(&env, "old-minutes", query, 0.9, &folder_tag).await;

    // 論理削除（索引はまだ残っている＝削除ジョブが追いつく前の状態）。
    sqlx::query("update node set deleted_at = now() where id = $1")
        .bind(node)
        .execute(&env.pool)
        .await
        .unwrap();

    let authz = ScriptedAuthz {
        readable_folders: vec![folder_tag],
        readable_files: vec![],
        denied_files: HashSet::new(),
    };
    let output = service(
        &env,
        authz,
        RagConfig {
            enabled: true,
            ..RagConfig::default()
        },
    )
    .search(&env.ctx, query, Some(5), SearchMode::Dense, None, None)
    .await
    .unwrap();
    assert!(
        output.results.is_empty(),
        "削除済み node は索引が stale でもハイドレーションの deleted_at ガードで消える"
    );
}
