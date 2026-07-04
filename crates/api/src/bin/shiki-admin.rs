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
use api::config::AppConfig;
use authz::{
    client::{OpenFgaClient, OpenFgaConfig},
    migrate::{retenant_object_tuples, FromNs},
    model,
    vocab::ObjectType,
};
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
    let blob_keys: Vec<String> =
        sqlx::query_scalar("SELECT object_key FROM blob WHERE tenant_id = $1")
            .bind(&db_tenant)
            .fetch_all(&db)
            .await?;
    let mut objects_moved: u64 = 0;
    let mut objects_skipped: u64 = 0;
    for old_key in &blob_keys {
        let Some(new_key) = renamespace_object_key(old_key, &from, &to) else {
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
    println!(
        "objects: moved={objects_moved} skipped={objects_skipped}（blob 行 {}）",
        blob_keys.len()
    );

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
        // リネームでは旧テナントのセッションを失効させる（再ログインで新 tenant claim を取得）。
        if let FromNs::Tenant(f) = &from {
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
/// legacy: `{org}/...`（先頭が to/ でない）→ `{to}/{org}/...`。
/// rename: `{from}/...` → `{to}/...`。移行対象でなければ `None`。
fn renamespace_object_key(old_key: &str, from: &FromNs, to: &str) -> Option<String> {
    match from {
        FromNs::Legacy => {
            let prefix = format!("{to}/");
            (!old_key.starts_with(&prefix)).then(|| format!("{to}/{old_key}"))
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
        // legacy: org 直下キーへ tenant を前置。既に新形式ならスキップ。
        let legacy = FromNs::Legacy;
        assert_eq!(
            renamespace_object_key("acme/deadbeef", &legacy, "t1").as_deref(),
            Some("t1/acme/deadbeef")
        );
        assert_eq!(
            renamespace_object_key("t1/acme/deadbeef", &legacy, "t1"),
            None
        );
        // rename: prefix 差し替え。他テナントは対象外。
        let rename = FromNs::Tenant("default".into());
        assert_eq!(
            renamespace_object_key("default/acme/deadbeef", &rename, "t1").as_deref(),
            Some("t1/acme/deadbeef")
        );
        assert_eq!(renamespace_object_key("other/acme/x", &rename, "t1"), None);
    }
}
