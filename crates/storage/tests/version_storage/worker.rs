use super::*;

#[tokio::test]
async fn proposals_do_not_skip_the_previous_current_blob() {
    let Some(env) = setup().await else { return };
    let (id, versions) = history(&env, 1).await;
    sqlx::query("UPDATE blob SET storage_checked_at = now() WHERE tenant_id = $1 AND org = $2")
        .bind(env.ctx.tenant_id())
        .bind(env.ctx.org())
        .execute(&env.pool)
        .await
        .unwrap();
    sqlx::query(
        "DELETE FROM job_queue WHERE tenant_id = $1 AND queue = 'storage_version_optimize'",
    )
    .bind(env.ctx.tenant_id())
    .execute(&env.pool)
    .await
    .unwrap();
    let proposal = env
        .service
        .propose_file_content_internal(&env.ctx, id, b"proposal", "text/plain", None)
        .await
        .unwrap();
    assert_eq!(proposal.version, 2);
    env.service
        .update_file_content_internal(&env.ctx, id, b"normal after proposal", "text/plain", None)
        .await
        .unwrap();
    let after: i64 = sqlx::query_scalar("SELECT (payload->>'after_version')::bigint FROM job_queue \
        WHERE tenant_id = $1 AND queue = 'storage_version_optimize' AND attempts = 0 ORDER BY id LIMIT 1")
        .bind(env.ctx.tenant_id()).fetch_one(&env.pool).await.unwrap();
    assert_eq!(after, 0, "現在版を変えない提案をcursorの起点にしない");
    let batch = env
        .archive
        .compact_node(env.ctx.tenant(), env.ctx.org(), id, after, env.options)
        .await
        .unwrap();
    assert_eq!(batch.optimized, 1);
    let optimized: bool = sqlx::query_scalar("SELECT storage_manifest IS NOT NULL FROM blob WHERE tenant_id = $1 AND org = $2 AND sha256 = $3")
        .bind(env.ctx.tenant_id()).bind(env.ctx.org()).bind(sha256_hex(&versions[0])).fetch_one(&env.pool).await.unwrap();
    assert!(optimized);
}
