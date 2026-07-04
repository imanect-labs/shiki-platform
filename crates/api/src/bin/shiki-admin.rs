//! shiki-admin — テナント運用 CLI（#89）。
//!
//! ```text
//! shiki-admin retenant (--legacy | --from <tenant>) --to <tenant> [--execute]
//! ```
//!
//! - `--legacy`: SAAS.1（#84）以前の**旧無印 FGA 識別子/オブジェクトキー**を tenant 名前空間形式へ
//!   移す（DB 行は day-1 で tenant_id を持つため不変・blob.object_key のみ書き換え）。
//! - `--from <tenant>`: cell→pool 移行（SAAS.5）。tenant_id のリネーム＝DB 全テーブル・FGA タプル・
//!   オブジェクトキー・セッションを一括で移す。
//! - 既定は **dry-run**（件数レポートのみ）。`--execute` で実行。全段冪等（再実行で収束）。
//!
//! 設定は shiki-server と同じ（env / TOML）。データプレーンの静止（メンテナンスウィンドウ）中の
//! 実行を前提とする（オンライン移行の整合は保証しない）。

use anyhow::{bail, Context};
use api::{config::AppConfig, keycloak_admin::KeycloakAdmin};
use authz::{
    client::{OpenFgaClient, OpenFgaConfig},
    migrate::{retenant_object_tuples, FromNs},
    model,
    vocab::ObjectType,
};
use authz::{AuthContext, Principal};
use sqlx::postgres::PgPoolOptions;
use storage::{ObjectStore, S3ObjectStore};
use uuid::Uuid;

/// tenant_id 列で移行対象になるテーブル（リネームモード）。FK 順は不要（同一 txn 内 UPDATE）。
const TENANT_TABLES: &[&str] = &[
    "node",
    "node_version",
    "pending_upload",
    "storage_event_outbox",
    "directory_user",
    "directory_role",
    "audit_log",
    "blob",
    "tenant",
];

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("retenant") => retenant(&args[1..]).await,
        _ => {
            eprintln!(
                "usage: shiki-admin retenant (--legacy | --from <tenant>) --to <tenant> [--execute]"
            );
            bail!("不明なサブコマンド");
        }
    }
}

async fn retenant(args: &[String]) -> anyhow::Result<()> {
    // --- 引数パース ---
    let mut from: Option<FromNs> = None;
    let mut to: Option<String> = None;
    let mut execute = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--legacy" => from = Some(FromNs::Legacy),
            "--from" => {
                let t = it.next().context("--from に値が必要です")?;
                from = Some(FromNs::Tenant(t.clone()));
            }
            "--to" => to = Some(it.next().context("--to に値が必要です")?.clone()),
            "--execute" => execute = true,
            other => bail!("不明な引数: {other}"),
        }
    }
    let from = from.context("--legacy か --from <tenant> のいずれかが必要です")?;
    let to = to.context("--to <tenant> が必要です")?;
    if to.contains(['|', ':', '#', '@']) || to.chars().any(char::is_whitespace) {
        bail!("--to に禁止文字（| : # @ 空白）が含まれています");
    }
    if let FromNs::Tenant(f) = &from {
        if f == &to {
            bail!("--from と --to が同一です");
        }
    }
    let mode = if execute { "EXECUTE" } else { "DRY-RUN" };
    println!("== shiki-admin retenant [{mode}] from={from:?} to={to} ==");

    // --- 依存の配線（shiki-server と同じ設定・migration は適用しない） ---
    let config = AppConfig::load().context("設定のロードに失敗")?;
    let db = PgPoolOptions::new()
        .max_connections(5)
        .connect(&config.database.url)
        .await
        .context("Postgres へ接続できません")?;
    let http = reqwest::Client::new();
    let fga = OpenFgaClient::connect(
        http,
        &OpenFgaConfig {
            base_url: config.authz.base_url.clone(),
            store_name: config.authz.store_name.clone(),
        },
        &model::default_model(),
    )
    .await
    .context("OpenFGA へ接続できません")?;
    let s3 = config
        .storage
        .s3
        .as_ref()
        .context("storage.s3 が未設定です")?;
    let store = S3ObjectStore::new(s3);

    // --- 対象の列挙は「DB 行が既に属している tenant」から行う ---
    // legacy: DB は day-1 で tenant_id=to を持つ（識別子だけが旧形式）。rename: from の行。
    let db_tenant = match &from {
        FromNs::Legacy => to.clone(),
        FromNs::Tenant(f) => f.clone(),
    };

    // --- 1. FGA タプルの移行 ---
    let mut fga_moved: u32 = 0;
    let mut fga_skipped: u32 = 0;
    let nodes: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, kind FROM node WHERE tenant_id = $1")
            .bind(&db_tenant)
            .fetch_all(&db)
            .await?;
    for (id, kind) in &nodes {
        let object_type = if kind == "folder" {
            ObjectType::Folder
        } else {
            ObjectType::File
        };
        let (m, s) =
            retenant_object_tuples(&fga, object_type, &id.to_string(), &from, &to, execute).await?;
        fga_moved += m;
        fga_skipped += s;
    }
    let roles: Vec<String> =
        sqlx::query_scalar("SELECT role_id FROM directory_role WHERE tenant_id = $1")
            .bind(&db_tenant)
            .fetch_all(&db)
            .await?;
    for role_id in &roles {
        let (m, s) =
            retenant_object_tuples(&fga, ObjectType::Role, role_id, &from, &to, execute).await?;
        fga_moved += m;
        fga_skipped += s;
    }
    let orgs: Vec<String> = sqlx::query_scalar(
        "SELECT org FROM tenant WHERE tenant_id = $1 \
         UNION SELECT DISTINCT org FROM node WHERE tenant_id = $1 \
         UNION SELECT DISTINCT org FROM directory_user WHERE tenant_id = $1",
    )
    .bind(&db_tenant)
    .fetch_all(&db)
    .await?;
    for org in &orgs {
        let (m, s) =
            retenant_object_tuples(&fga, ObjectType::Organization, org, &from, &to, execute)
                .await?;
        fga_moved += m;
        fga_skipped += s;
    }
    println!(
        "FGA: nodes={} roles={} orgs={} → tuples moved={fga_moved} skipped(他名前空間)={fga_skipped}",
        nodes.len(),
        roles.len(),
        orgs.len()
    );

    // --- 2. オブジェクトの移行（blob.object_key を正として copy→delete） ---
    // 大テナント（数百万 blob）でもメモリへ全載せしない keyset ページング。
    let mut objects_moved: u64 = 0;
    let mut objects_skipped: u64 = 0;
    let mut blob_rows: u64 = 0;
    let mut last_key: Option<String> = None;
    loop {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT object_key, org FROM blob \
             WHERE tenant_id = $1 AND ($2::text IS NULL OR object_key > $2) \
             ORDER BY object_key LIMIT 1000",
        )
        .bind(&db_tenant)
        .bind(last_key.as_deref())
        .fetch_all(&db)
        .await?;
        if rows.is_empty() {
            break;
        }
        last_key = rows.last().map(|(k, _)| k.clone());
        blob_rows += rows.len() as u64;
        for (old_key, org) in &rows {
            let Some(new_key) = renamespace_object_key(old_key, org, &from, &to) else {
                objects_skipped += 1; // 既に新形式（再実行時）。
                continue;
            };
            if execute {
                // 冪等: コピー済みならスキップ、旧が無ければ何もしない。
                if !store.exists(&new_key).await? {
                    if !store.exists(old_key).await? {
                        objects_skipped += 1;
                        continue;
                    }
                    store.copy(old_key, &new_key).await?;
                }
                store.delete(old_key).await?;
            }
            objects_moved += 1;
        }
    }
    println!("objects: moved={objects_moved} skipped={objects_skipped}（blob 行 {blob_rows}）");

    // --- 3. DB の書き換え（1 txn） ---
    if execute {
        let mut tx = db.begin().await?;
        match &from {
            FromNs::Legacy => {
                // 行は既に tenant_id=to。object_key だけ新形式へ。
                sqlx::query(
                    "UPDATE blob SET object_key = $1 || '/' || object_key \
                     WHERE tenant_id = $1 AND object_key NOT LIKE $1 || '/%'",
                )
                .bind(&to)
                .execute(&mut *tx)
                .await?;
            }
            FromNs::Tenant(f) => {
                // node/node_version → blob の FK は同一 txn 内で tenant_id を順に書き換える
                // 途中状態で違反するため、commit 時検査へ遅延させる（migration 0008 で
                // DEFERRABLE 化済み）。
                sqlx::query("SET CONSTRAINTS node_blob_fk, node_version_blob_fk DEFERRED")
                    .execute(&mut *tx)
                    .await?;
                // object_key の prefix 差し替え → 各テーブルの tenant_id リネーム。
                sqlx::query(
                    "UPDATE blob SET object_key = $2 || substring(object_key FROM length($1) + 1) \
                     WHERE tenant_id = $1 AND object_key LIKE $1 || '/%'",
                )
                .bind(f)
                .bind(&to)
                .execute(&mut *tx)
                .await?;
                for table in TENANT_TABLES {
                    sqlx::query(&format!(
                        "UPDATE {table} SET tenant_id = $2 WHERE tenant_id = $1"
                    ))
                    .bind(f)
                    .bind(&to)
                    .execute(&mut *tx)
                    .await?;
                }
            }
        }
        tx.commit().await?;
        println!("DB: 書き換え完了");
        if let FromNs::Tenant(f) = &from {
            // リネームの監査エントリを**新テナントの chain へ連結**して記録する（forensics の
            // アンカー）。⚠️ リネーム以前の chained エントリの entry_hash は**旧 tenant_id で
            // 計算**されているため、チェーン検証はリネーム境界より前を旧 tenant_id で検証する
            // 必要がある（runbook 参照）。
            let orgs_for_audit: Vec<String> =
                sqlx::query_scalar("SELECT org FROM tenant WHERE tenant_id = $1")
                    .bind(&to)
                    .fetch_all(&db)
                    .await?;
            let audit_org = orgs_for_audit
                .first()
                .cloned()
                .unwrap_or_else(|| to.clone());
            let ctx = AuthContext::new(
                Principal {
                    id: "system".into(),
                    email: None,
                    groups: vec![],
                    roles: vec![],
                    tenant_id: Some(to.clone()),
                },
                audit_org.clone(),
                to.clone(),
            );
            let mut tx = db.begin().await?;
            storage::audit::record_on(
                &mut tx,
                &ctx,
                storage::audit::AuditEntry {
                    action: "tenant.retenant",
                    object_type: "organization",
                    object_id: &audit_org,
                    decision: storage::audit::Decision::Allow,
                    trace_id: None,
                    metadata: serde_json::json!({
                        "from": f, "to": to,
                        "fga_tuples": fga_moved, "objects": objects_moved,
                    }),
                },
                storage::audit::Chain::Yes,
            )
            .await?;
            tx.commit().await?;
            println!(
                "audit: tenant.retenant を記録（⚠️ リネーム以前の chain 検証は旧 tenant_id '{f}' で行うこと）"
            );

            // IdP（Keycloak）の tenant 属性を追従更新する（残すと次ログインが旧 tenant claim で
            // 旧名前空間の空セッションになる）。provisioner 設定が無ければ手動対応を促す。
            match KeycloakAdmin::from_config(&reqwest::Client::new(), &config.auth) {
                Ok(kc) => match kc.find_users_by_tenant(f).await {
                    Ok(users) => {
                        let mut updated = 0usize;
                        for u in &users {
                            match kc.update_user_tenant(&u.id, &to).await {
                                Ok(()) => updated += 1,
                                Err(e) => eprintln!(
                                    "IdP tenant 属性の更新に失敗 user={}: {e}（手動で対応要）",
                                    u.username
                                ),
                            }
                        }
                        println!("IdP: tenant 属性を {updated}/{} 件更新", users.len());
                    }
                    Err(e) => eprintln!("IdP ユーザー検索に失敗（tenant 属性は手動更新要）: {e}"),
                },
                Err(_) => eprintln!(
                    "⚠️ provisioner 未設定のため IdP の tenant 属性は更新していません。\
                     Keycloak 側で attributes.tenant を '{f}' → '{to}' へ手動更新してください"
                ),
            }

            // 旧テナントのセッションを失効させる（再ログインで新 tenant claim を取得）。
            use api::session::{RedisSessionStore, SessionStore};
            match RedisSessionStore::connect(&config.session.redis_url).await {
                Ok(sessions) => match sessions.delete_tenant(f).await {
                    Ok(n) => println!("sessions: {n} 件失効"),
                    Err(e) => eprintln!("sessions 失効に失敗（手動で対応要）: {e}"),
                },
                Err(e) => eprintln!("Redis 接続に失敗（sessions は手動で対応要）: {e}"),
            }
        }
    } else {
        println!("DRY-RUN のため書き換えなし。--execute で実行します。");
    }
    Ok(())
}

/// blob.object_key を移行先名前空間へ写す。
/// legacy: `{org}/...` → `{to}/{org}/...`。「移行済み」判定は `{to}/{org}/` **完全一致**で行う
/// （`{to}/` だけだと org == to の legacy キー `{to}/{sha}` を誤って移行済み扱いする）。
/// rename: `{from}/...` → `{to}/...`。移行対象でなければ `None`。
fn renamespace_object_key(old_key: &str, org: &str, from: &FromNs, to: &str) -> Option<String> {
    match from {
        FromNs::Legacy => {
            let migrated_prefix = format!("{to}/{org}/");
            (!old_key.starts_with(&migrated_prefix)).then(|| format!("{to}/{old_key}"))
        }
        FromNs::Tenant(f) => old_key
            .strip_prefix(&format!("{f}/"))
            .map(|rest| format!("{to}/{rest}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_key_renamespace() {
        // legacy: org 直下キーへ tenant を前置。「移行済み」は {to}/{org}/ 完全一致で判定。
        let legacy = FromNs::Legacy;
        assert_eq!(
            renamespace_object_key("acme/deadbeef", "acme", &legacy, "t1").as_deref(),
            Some("t1/acme/deadbeef")
        );
        assert_eq!(
            renamespace_object_key("t1/acme/deadbeef", "acme", &legacy, "t1"),
            None
        );
        // org == to（legacy キー "acme/sha" を tenant acme へ移行）でも誤スキップしない。
        assert_eq!(
            renamespace_object_key("acme/deadbeef", "acme", &legacy, "acme").as_deref(),
            Some("acme/acme/deadbeef")
        );
        assert_eq!(
            renamespace_object_key("acme/acme/deadbeef", "acme", &legacy, "acme"),
            None
        );
        // rename: prefix 差し替え。他テナントは対象外。
        let rename = FromNs::Tenant("default".into());
        assert_eq!(
            renamespace_object_key("default/acme/deadbeef", "acme", &rename, "t1").as_deref(),
            Some("t1/acme/deadbeef")
        );
        assert_eq!(
            renamespace_object_key("other/acme/x", "acme", &rename, "t1"),
            None
        );
    }
}
