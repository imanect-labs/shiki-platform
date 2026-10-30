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
        // updated_by は NOT NULL（migration 0045）。作成＝最初の更新なので created_by と同値。
        "insert into node (org, tenant_id, kind, name, blob_sha256, size_bytes, content_type, \
                           created_by, updated_by) \
         values ($1, $2, 'file', $3, $4, 10, 'text/plain', $5, $5) returning id",
    )
    .bind(&ctx.org)
    .bind(&ctx.tenant_id)
    .bind(name)
    .bind(&sha)
    .bind(&ctx.principal.id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    // 構造タグ: file 自身＋所属フォルダ（本物のインジェストと同じ形）。
    seed_chunk_on(env, node_id, name, query, weight, &[folder_id]).await;
    node_id
}

/// 既存の file node にチャンクを 1 つ積む。`folders` はチャンクの構造タグに載せるフォルダ
/// （索引上の祖先。木の実際の祖先と食い違わせれば「移動後の古い索引」を作れる）。
async fn seed_chunk_on(
    env: &Env,
    node_id: Uuid,
    name: &str,
    query: &str,
    weight: f32,
    folders: &[Uuid],
) {
    let ctx = &env.ctx;
    let chunk_id = Uuid::new_v5(&node_id, b"leaf-0");
    let tags: Vec<String> =
        std::iter::once(ctx.ns().file(&node_id.to_string()).as_str().to_string())
            .chain(
                folders
                    .iter()
                    .map(|f| ctx.ns().folder(&f.to_string()).as_str().to_string()),
            )
            .collect();
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

/// フォルダ/ファイルの node と closure 行（自身 depth 0 ＋親の祖先 +1）を作る。
async fn make_node(env: &Env, kind: &str, parent: Option<Uuid>) -> Uuid {
    let ctx = &env.ctx;
    let name = format!("{kind}-{}", Uuid::new_v4().simple());
    let id: Uuid = if kind == "folder" {
        sqlx::query_scalar(
            "insert into node (org, tenant_id, kind, name, parent_id, created_by, updated_by) \
             values ($1, $2, 'folder', $3, $4, $5, $5) returning id",
        )
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(&name)
        .bind(parent)
        .bind(&ctx.principal.id)
        .fetch_one(&env.pool)
        .await
        .unwrap()
    } else {
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
        sqlx::query_scalar(
            "insert into node (org, tenant_id, kind, name, parent_id, blob_sha256, size_bytes, \
                               content_type, created_by, updated_by) \
             values ($1, $2, 'file', $3, $4, $5, 10, 'text/plain', $6, $6) returning id",
        )
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(&name)
        .bind(parent)
        .bind(&sha)
        .bind(&ctx.principal.id)
        .fetch_one(&env.pool)
        .await
        .unwrap()
    };
    sqlx::query(
        "insert into node_closure (org, tenant_id, ancestor, descendant, depth) \
         select $1, $2, ancestor, $3, depth + 1 from node_closure \
          where tenant_id = $2 and descendant = $4 \
         union all select $1, $2, $3, $3, 0",
    )
    .bind(&ctx.org)
    .bind(&ctx.tenant_id)
    .bind(id)
    .bind(parent)
    .execute(&env.pool)
    .await
    .unwrap();
    id
}

fn folders(ids: &[Uuid]) -> SearchScope {
    SearchScope {
        folders: ids.to_vec(),
        files: vec![],
    }
}

fn files(ids: &[Uuid]) -> SearchScope {
    SearchScope {
        folders: vec![],
        files: ids.to_vec(),
    }
}

/// モデルの絞り込み（#423）は知識スコープとの**交差**で、スコープを広げない。
#[tokio::test]
async fn narrow_scope_intersects_with_knowledge_scope() {
    let Some(env) = setup().await else { return };
    // R ─┬─ A ── A1 ── fa1
    //    └─ B ── fb
    let r = make_node(&env, "folder", None).await;
    let a = make_node(&env, "folder", Some(r)).await;
    let a1 = make_node(&env, "folder", Some(a)).await;
    let b = make_node(&env, "folder", Some(r)).await;
    let fa1 = make_node(&env, "file", Some(a1)).await;
    let fb = make_node(&env, "file", Some(b)).await;
    let svc = service(&env, allow_all_authz(&env, &[r]), RagConfig::default());
    let ctx = &env.ctx;
    let narrow = |base: Option<SearchScope>, n: SearchScope| {
        let svc = &svc;
        async move { svc.narrow_scope(ctx, base.as_ref(), &n).await.unwrap() }
    };

    // 知識スコープ無し → 絞り込みがそのまま実効スコープ。
    assert_eq!(narrow(None, files(&[fb, fb])).await, files(&[fb]));
    // 絞り込み無し → 知識スコープのまま。
    assert_eq!(
        narrow(Some(folders(&[a])), SearchScope::default()).await,
        folders(&[a])
    );
    // 内側へ絞る → 絞り込み側。
    assert_eq!(
        narrow(Some(folders(&[a])), folders(&[a1])).await,
        folders(&[a1])
    );
    // 外側を指定しても知識スコープより広がらない。
    assert_eq!(
        narrow(Some(folders(&[a1])), folders(&[r])).await,
        folders(&[a1])
    );
    assert_eq!(
        narrow(Some(files(&[fa1])), folders(&[a])).await,
        files(&[fa1])
    );
    // 交わらない → 空（呼び出し側は検索せず 0 件にする）。
    assert!(narrow(Some(folders(&[a])), folders(&[b])).await.is_empty());
    // ファイル単位: 知識スコープ内のものだけ残る。
    assert_eq!(
        narrow(Some(folders(&[a])), files(&[fa1, fb])).await,
        files(&[fa1])
    );

    // 種別をまたいだ一致は認めない: skill がフォルダ ID を files に保存していても
    // （`file:` タグは何にも一致しない）、folder_ids で同じ ID を渡して配下へ広げられない。
    assert!(narrow(Some(files(&[a])), folders(&[a])).await.is_empty());
    assert!(narrow(Some(folders(&[fb])), files(&[fb])).await.is_empty());

    // 別テナントからは closure が見えない → 交差は空（越境で包含判定しない）。
    let other = test_ctx(&format!("t-{}", Uuid::new_v4().simple()), "alice");
    let got = svc
        .narrow_scope(&other, Some(&folders(&[a])), &folders(&[a1]))
        .await
        .unwrap();
    assert!(got.is_empty());
}

/// 交差で検索するときは、元の知識スコープで候補を検索時点の木に対して再検証する（#423・Codex）。
/// 交差が空なら検索せず 0 件だが、`rag.search` の監査は残す。
#[tokio::test]
async fn search_narrowed_rechecks_base_and_audits_empty_intersection() {
    let Some(env) = setup().await else { return };
    let query = "移動後の資料";
    // A ── B（絞り込み先）／C ── moved（索引上はまだ B 配下のタグを持つ）
    let a = make_node(&env, "folder", None).await;
    let b = make_node(&env, "folder", Some(a)).await;
    let c = make_node(&env, "folder", None).await;
    let stays = make_node(&env, "file", Some(b)).await;
    let moved = make_node(&env, "file", Some(c)).await;
    seed_chunk_on(&env, stays, "stays", query, 0.8, &[a, b]).await;
    seed_chunk_on(&env, moved, "moved", query, 0.9, &[a, b]).await;
    let svc = service(
        &env,
        allow_all_authz(&env, &[a, b, c]),
        RagConfig::default(),
    );
    let ctx = &env.ctx;

    let out = svc
        .search_narrowed(
            ctx,
            query,
            Some(5),
            SearchMode::Dense,
            Some(&folders(&[a])),
            &folders(&[b]),
            None,
        )
        .await
        .unwrap();
    let got: HashSet<Uuid> = out.results.iter().map(|r| r.file_id).collect();
    assert_eq!(
        got,
        HashSet::from([stays]),
        "木の上で A の外にあるファイルは返さない"
    );

    // 上限外の候補が取得枠を埋めても、バックフィルが上限内の候補で埋め直す（Codex）。
    // fetch_k = 1 なので 1 ラウンド目は上位の moved だけを取る。
    let narrow_pool = service(
        &env,
        allow_all_authz(&env, &[a, b, c]),
        RagConfig {
            rerank_pool: 1,
            over_fetch_tags: 1,
            ..RagConfig::default()
        },
    );
    let out = narrow_pool
        .search_narrowed(
            ctx,
            query,
            Some(1),
            SearchMode::Dense,
            Some(&folders(&[a])),
            &folders(&[b]),
            None,
        )
        .await
        .unwrap();
    let got: Vec<Uuid> = out.results.iter().map(|r| r.file_id).collect();
    assert_eq!(got, vec![stays], "上限外で落ちた分を次のラウンドで埋める");

    // 知識スコープ無しの絞り込みでも、木の上で絞り込み先の外にあるファイルは返さない
    // （moved は索引上まだ B のタグを持つが、木の上では C 配下・Codex）。
    let out = svc
        .search_narrowed(
            ctx,
            query,
            Some(5),
            SearchMode::Dense,
            None,
            &folders(&[b]),
            None,
        )
        .await
        .unwrap();
    let got: Vec<Uuid> = out.results.iter().map(|r| r.file_id).collect();
    assert_eq!(got, vec![stays], "絞り込み先も検索時点の木で確かめる");

    // 交わらない絞り込み → 0 件。監査は残る。
    let before: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND action = 'rag.search'",
    )
    .bind(&ctx.tenant_id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    let out = svc
        .search_narrowed(
            ctx,
            query,
            Some(5),
            SearchMode::Dense,
            Some(&folders(&[b])),
            &folders(&[c]),
            None,
        )
        .await
        .unwrap();
    assert!(out.results.is_empty());
    let after: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND action = 'rag.search'",
    )
    .bind(&ctx.tenant_id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(after, before + 1, "交差が空でも rag.search を監査に残す");

    // 絞り込み無しは知識スコープそのままの検索（再検証なし・従来どおり）。
    let out = svc
        .search_narrowed(
            ctx,
            query,
            Some(5),
            SearchMode::Dense,
            Some(&folders(&[a])),
            &SearchScope::default(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(out.results.len(), 2);
}

/// 課された範囲が複数（skill の知識スコープと会話の検索フォルダ）でも、交差前のまま全部で
/// 確かめる（#965・Codex）。検索フォルダが知識スコープの外へ移動された後は、索引のタグが古い
/// ままでも、検索フォルダの中の文書を返さない（知識スコープの制約が失われない）。
#[tokio::test]
async fn search_narrowed_within_keeps_every_bound() {
    let Some(env) = setup().await else { return };
    let query = "検索フォルダの資料";
    // A（skill の範囲）── inside ── kept／C ── moved_out ── left
    // moved_out は A の外へ移動済み（索引上はまだ A 配下のタグを持つ）。
    let a = make_node(&env, "folder", None).await;
    let c = make_node(&env, "folder", None).await;
    let inside = make_node(&env, "folder", Some(a)).await;
    let moved_out = make_node(&env, "folder", Some(c)).await;
    let kept = make_node(&env, "file", Some(inside)).await;
    let left = make_node(&env, "file", Some(moved_out)).await;
    seed_chunk_on(&env, kept, "kept", query, 0.8, &[a, inside]).await;
    seed_chunk_on(&env, left, "left", query, 0.9, &[a, moved_out]).await;
    let svc = service(
        &env,
        allow_all_authz(&env, &[a, c, inside, moved_out]),
        RagConfig::default(),
    );
    let ctx = &env.ctx;
    let found =
        |out: rag::SearchOutput| -> Vec<Uuid> { out.results.iter().map(|r| r.file_id).collect() };

    let out = svc
        .search_narrowed_within(
            ctx,
            query,
            Some(5),
            SearchMode::Dense,
            &[&folders(&[a]), &folders(&[inside])],
            &SearchScope::default(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(found(out), vec![kept], "両方の内側の文書は返す");

    let out = svc
        .search_narrowed_within(
            ctx,
            query,
            Some(5),
            SearchMode::Dense,
            &[&folders(&[a]), &folders(&[moved_out])],
            &SearchScope::default(),
            None,
        )
        .await
        .unwrap();
    assert!(
        out.results.is_empty(),
        "検索フォルダが知識スコープの外へ出たら何も返さない: {:?}",
        found(out)
    );

    // 検索フォルダだけなら、その中の文書は返る（範囲を課しすぎていないこと）。
    let out = svc
        .search_narrowed_within(
            ctx,
            query,
            Some(5),
            SearchMode::Dense,
            &[&folders(&[moved_out])],
            &SearchScope::default(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(found(out), vec![left]);

    // モデルの絞り込みを重ねても、課された範囲の外へは出ない。
    let out = svc
        .search_narrowed_within(
            ctx,
            query,
            Some(5),
            SearchMode::Dense,
            &[&folders(&[a]), &folders(&[moved_out])],
            &SearchScope {
                folders: Vec::new(),
                files: vec![left],
            },
            None,
        )
        .await
        .unwrap();
    assert!(out.results.is_empty());
}

/// 結果には reranker の生スコアと、設定の閾値で落とした関連度が付く（#424）。
/// 候補が 1 件でも採点する（以前は 1 件だと rerank を飛ばしていた）。
#[tokio::test]
async fn results_carry_rerank_score_and_relevance_band() {
    let Some(env) = setup().await else { return };
    let folder = Uuid::new_v4();
    // FakeReranker のスコア = 本文の文字のうちクエリに含まれる文字の数。本文は「foo の本文」。
    seed_chunk(&env, "foo", "foo本文", 0.9, folder).await;
    let config = RagConfig {
        relevance_high: 4.0,
        relevance_low: 1.0,
        ..RagConfig::default()
    };
    let svc = service(&env, allow_all_authz(&env, &[folder]), config);
    for (query, score, band) in [
        ("foo本文", 5.0, rag::Relevance::High),
        ("本文", 2.0, rag::Relevance::Medium),
        ("zzz", 0.0, rag::Relevance::Low),
    ] {
        let out = svc
            .search(&env.ctx, query, Some(5), SearchMode::Dense, None, None)
            .await
            .unwrap();
        assert_eq!(out.results.len(), 1, "{query}");
        assert_eq!(out.results[0].rerank_score, score, "{query}");
        assert_eq!(out.results[0].relevance, band, "{query}");
    }
}

/// 有限でないスコアを返す reranker（差し替え点の異常値）。
struct NanReranker;

#[async_trait::async_trait]
impl rag::rerank::Reranker for NanReranker {
    async fn rerank(
        &self,
        _ctx: &AuthContext,
        _query: &str,
        passages: &[rag::rerank::RerankPassage],
    ) -> Result<Vec<rag::rerank::RerankScore>, rag::RagError> {
        Ok(passages
            .iter()
            .map(|p| rag::rerank::RerankScore {
                id: p.id.clone(),
                score: f32::NAN,
            })
            .collect())
    }
}

/// 非有限のスコアは API へ流さず（JSON で null になる）、関連度は「低」に倒す（Codex）。
#[tokio::test]
async fn non_finite_rerank_score_is_normalized() {
    let Some(env) = setup().await else { return };
    let folder = Uuid::new_v4();
    seed_chunk(&env, "nan", "異常値", 0.9, folder).await;
    let svc = SearchService::new(
        env.pool.clone(),
        RagConfig::default(),
        Arc::new(FakeEmbedder),
        Arc::new(NanReranker),
        Arc::clone(&env.vector) as _,
        Arc::new(TantivyFulltext::new(env._tmp.path())),
        Arc::new(allow_all_authz(&env, &[folder])),
        AuditRecorder::new(env.pool.clone()),
    );
    let out = svc
        .search(&env.ctx, "異常値", Some(5), SearchMode::Dense, None, None)
        .await
        .unwrap();
    assert_eq!(out.results.len(), 1);
    assert_eq!(out.results[0].rerank_score, 0.0);
    assert_eq!(out.results[0].relevance, rag::Relevance::Low);
    let json = serde_json::to_value(&out.results[0]).unwrap();
    assert!(json["rerank_score"].is_number());
}

/// 本文の読み出し（doc_read）にも知識スコープを課す判定（#425）。
#[tokio::test]
async fn file_in_scope_follows_the_tree() {
    let Some(env) = setup().await else { return };
    let a = make_node(&env, "folder", None).await;
    let b = make_node(&env, "folder", None).await;
    let in_a = make_node(&env, "file", Some(a)).await;
    let in_b = make_node(&env, "file", Some(b)).await;
    let svc = service(&env, allow_all_authz(&env, &[a, b]), RagConfig::default());
    let ctx = &env.ctx;
    assert!(svc.file_in_scope(ctx, &folders(&[a]), in_a).await.unwrap());
    assert!(!svc.file_in_scope(ctx, &folders(&[a]), in_b).await.unwrap());
    assert!(svc.file_in_scope(ctx, &files(&[in_b]), in_b).await.unwrap());
    assert!(!svc.file_in_scope(ctx, &files(&[in_a]), in_b).await.unwrap());
    // 別テナントからは木が見えない（範囲外として扱う）。
    let other = test_ctx(&format!("t-{}", Uuid::new_v4().simple()), "alice");
    assert!(!svc
        .file_in_scope(&other, &folders(&[a]), in_a)
        .await
        .unwrap());
}

/// reranker の生スコアが下限未満のヒットは結果から落とし、件数をデバッグに出す（#100）。
#[tokio::test]
async fn min_rerank_score_drops_weak_hits() {
    let Some(env) = setup().await else { return };
    let folder = Uuid::new_v4();
    // FakeReranker のスコア = 本文（「strong の本文」「weak の本文」）の文字のうちクエリに含まれる数。
    seed_chunk(&env, "strong", "strong", 0.9, folder).await;
    seed_chunk(&env, "weak", "strong", 0.8, folder).await;
    let config = RagConfig {
        min_rerank_score: Some(6.0),
        ..RagConfig::default()
    };
    let svc = service(&env, allow_all_authz(&env, &[folder]), config);
    let out = svc
        .search(&env.ctx, "strong", Some(5), SearchMode::Dense, None, None)
        .await
        .unwrap();
    let names: Vec<&str> = out.results.iter().map(|r| r.file_name.as_str()).collect();
    assert_eq!(names, vec!["strong"], "下限未満は top_k を割っても埋めない");
    assert_eq!(out.debug.below_min_score, 1);

    // 既定（None）は足切りしない。
    let svc = service(&env, allow_all_authz(&env, &[folder]), RagConfig::default());
    let out = svc
        .search(&env.ctx, "strong", Some(5), SearchMode::Dense, None, None)
        .await
        .unwrap();
    assert_eq!(out.results.len(), 2);
    assert_eq!(out.debug.below_min_score, 0);
}
