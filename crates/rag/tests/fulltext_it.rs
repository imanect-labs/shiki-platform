//! Tantivy＋Lindera 全文索引の結合テスト（Task 2.5 受入条件）。
//!
//! 外部サービス不要（tempdir）のため常時実行される。
//! 検証: 日本語形態素ヒット・authz_tags pre-filter・index-per-tenant の絶対遮断・
//! 差し替え（版更新）・削除・タグ再評価。

// テストコード: pedantic/安全系 lint は本番コードのみ厳格化する方針のため許容する。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic
)]

use authz::{AuthContext, Principal};
use rag::vector_store::PreFilter;
use rag::{FulltextDoc, FulltextIndex, TantivyFulltext};
use uuid::Uuid;

fn ctx(tenant: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        tenant.into(),
    )
}

fn tags(ctx: &AuthContext, file: Uuid, folder: &str) -> Vec<String> {
    vec![
        ctx.ns().file(&file.to_string()).as_str().to_string(),
        ctx.ns().folder(folder).as_str().to_string(),
    ]
}

/// node のチャンク 1 個を索引へ入れるヘルパ。
fn index_one(
    ft: &TantivyFulltext,
    ctx: &AuthContext,
    node: Uuid,
    text: &str,
    authz_tags: &[String],
) -> Uuid {
    let chunk_id = Uuid::new_v5(&node, b"chunk-0");
    ft.replace_node(
        ctx,
        node,
        &[FulltextDoc {
            chunk_id,
            node_id: node,
            version: 1,
            text,
            authz_tags,
        }],
    )
    .unwrap();
    chunk_id
}

fn search_all(ft: &TantivyFulltext, ctx: &AuthContext, q: &str) -> Vec<Uuid> {
    ft.search(ctx, q, 10, &PreFilter::TenantOnly, &[])
        .unwrap()
        .into_iter()
        .map(|h| h.chunk_id)
        .collect()
}

#[test]
fn japanese_morphological_tokenization_hits_correctly() {
    let dir = tempfile::tempdir().unwrap();
    let ft = TantivyFulltext::new(dir.path());
    let ctx = ctx("a-corp");
    let node = Uuid::new_v4();
    let t = tags(&ctx, node, "root");
    index_one(
        &ft,
        &ctx,
        node,
        "東京都の四半期売上は前年比で増加した。",
        &t,
    );

    // 形態素単位でヒットする（Task 2.5 受入条件）。
    assert_eq!(search_all(&ft, &ctx, "売上").len(), 1);
    assert_eq!(search_all(&ft, &ctx, "東京").len(), 1);
    // 「東京都」は 東京/都 に分割されるため、部分文字列「京都」ではヒットしない
    //（bi-gram との差・形態素解析の意味）。
    assert!(search_all(&ft, &ctx, "京都").is_empty());
}

#[test]
fn authz_tags_prefilter_narrows_results() {
    let dir = tempfile::tempdir().unwrap();
    let ft = TantivyFulltext::new(dir.path());
    let ctx = ctx("a-corp");
    let node_a = Uuid::new_v4();
    let node_b = Uuid::new_v4();
    let tags_a = tags(&ctx, node_a, "folder-sales");
    let tags_b = tags(&ctx, node_b, "folder-hr");
    let chunk_a = index_one(&ft, &ctx, node_a, "営業の売上報告です。", &tags_a);
    index_one(&ft, &ctx, node_b, "人事の売上関連メモです。", &tags_b);

    // folder-sales の可読タグだけ持つユーザーは node_a のみヒット。
    let readable = vec![ctx.ns().folder("folder-sales").as_str().to_string()];
    let hits = ft
        .search(&ctx, "売上", 10, &PreFilter::Tags(readable), &[])
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].chunk_id, chunk_a);

    // 可読タグゼロなら何もヒットしない（fail-closed）。
    let hits = ft
        .search(&ctx, "売上", 10, &PreFilter::Tags(vec![]), &[])
        .unwrap();
    assert!(hits.is_empty());
}

#[test]
fn tenant_isolation_is_absolute_even_with_forged_tags() {
    let dir = tempfile::tempdir().unwrap();
    let ft = TantivyFulltext::new(dir.path());
    let a = ctx("a-corp");
    let b = ctx("b-corp");
    let node = Uuid::new_v4();
    let tags_a = tags(&a, node, "root");
    index_one(&ft, &a, node, "極秘の売上データ。", &tags_a);

    // 別テナントは TenantOnly でも 0 件（index-per-tenant の防壁）。
    assert!(search_all(&ft, &b, "売上").is_empty());
    // a-corp のタグを「改竄」して b-corp のクエリに載せても絶対に返らない
    //（Task 2.5 受入条件: tenant 境界は authz_tags と独立に必ず適用）。
    let forged = tags_a.clone();
    let hits = ft
        .search(&b, "売上", 10, &PreFilter::Tags(forged), &[])
        .unwrap();
    assert!(hits.is_empty());
    // 本来のテナントでは見える。
    assert_eq!(search_all(&ft, &a, "売上").len(), 1);
}

#[test]
fn replace_node_swaps_old_version_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let ft = TantivyFulltext::new(dir.path());
    let ctx = ctx("a-corp");
    let node = Uuid::new_v4();
    let t = tags(&ctx, node, "root");
    index_one(&ft, &ctx, node, "旧しい内容の文書。", &t);

    // 版更新: 同 node を新内容で差し替え（delete_term → add → commit）。
    let new_chunk = Uuid::new_v5(&node, b"chunk-0-v2");
    ft.replace_node(
        &ctx,
        node,
        &[FulltextDoc {
            chunk_id: new_chunk,
            node_id: node,
            version: 2,
            text: "新しい内容の文書。",
            authz_tags: &t,
        }],
    )
    .unwrap();

    assert!(
        search_all(&ft, &ctx, "旧しい").is_empty(),
        "更新で古いチャンクが残らない（Task 2.9 受入条件）"
    );
    let hits = search_all(&ft, &ctx, "新しい");
    assert_eq!(hits, vec![new_chunk]);
}

#[test]
fn delete_node_removes_all_chunks_and_purge_tenant_drops_index() {
    let dir = tempfile::tempdir().unwrap();
    let ft = TantivyFulltext::new(dir.path());
    let ctx = ctx("a-corp");
    let node = Uuid::new_v4();
    let t = tags(&ctx, node, "root");
    index_one(&ft, &ctx, node, "削除される文書。", &t);

    ft.delete_node(&ctx, node).unwrap();
    assert!(search_all(&ft, &ctx, "文書").is_empty());

    assert!(ft.tenant_index_exists("a-corp"));
    ft.purge_tenant("a-corp").unwrap();
    assert!(!ft.tenant_index_exists("a-corp"));
    assert!(search_all(&ft, &ctx, "文書").is_empty());
}

#[test]
fn move_reevaluates_tags_via_replace() {
    let dir = tempfile::tempdir().unwrap();
    let ft = TantivyFulltext::new(dir.path());
    let ctx = ctx("a-corp");
    let node = Uuid::new_v4();
    let old_tags = tags(&ctx, node, "folder-old");
    let chunk = index_one(&ft, &ctx, node, "移動する文書。", &old_tags);

    // move: authz_tags を新フォルダで再評価して差し替え（本文は rag_chunk から再投入）。
    let new_tags = tags(&ctx, node, "folder-new");
    ft.replace_node(
        &ctx,
        node,
        &[FulltextDoc {
            chunk_id: chunk,
            node_id: node,
            version: 1,
            text: "移動する文書。",
            authz_tags: &new_tags,
        }],
    )
    .unwrap();

    let old_readable = vec![ctx.ns().folder("folder-old").as_str().to_string()];
    let new_readable = vec![ctx.ns().folder("folder-new").as_str().to_string()];
    assert!(
        ft.search(&ctx, "文書", 10, &PreFilter::Tags(old_readable), &[])
            .unwrap()
            .is_empty(),
        "旧フォルダの viewer からは消える"
    );
    assert_eq!(
        ft.search(&ctx, "文書", 10, &PreFilter::Tags(new_readable), &[])
            .unwrap()
            .len(),
        1,
        "新フォルダの viewer に見える"
    );
}

#[test]
fn exclude_skips_already_fetched_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let ft = TantivyFulltext::new(dir.path());
    let ctx = ctx("a-corp");
    let node = Uuid::new_v4();
    let t = tags(&ctx, node, "root");
    let chunk = index_one(&ft, &ctx, node, "バックフィル対象の文書。", &t);

    let hits = ft
        .search(&ctx, "文書", 10, &PreFilter::TenantOnly, &[chunk])
        .unwrap();
    assert!(
        hits.is_empty(),
        "取得済み chunk は除外される（バックフィル用）"
    );
}
