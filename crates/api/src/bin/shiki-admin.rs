//! shiki-admin — テナント運用 CLI（#89）。
//!
//! ```text
//! shiki-admin retenant (--legacy | --from <tenant>) --to <tenant> [--actor <id>] [--execute]
//! ```
//!
//! - `--legacy`: SAAS.1（#84）以前の**旧無印 FGA 識別子/オブジェクトキー**を tenant 名前空間形式へ
//!   移す（DB 行は day-1 で tenant_id を持つため不変・blob.object_key のみ書き換え）。
//! - `--from <tenant>`: cell→pool 移行（SAAS.5）。tenant_id のリネーム＝DB 全テーブル・FGA タプル・
//!   オブジェクトキー・セッションを一括で移す。
//! - 既定は **dry-run**（件数レポートのみ）。`--execute` で実行。全段冪等（再実行で収束）。
//! - `--actor <id>`: 監査に刻む実行者（省略時は `$SUDO_USER` → `$USER` → `unknown`）。
//!
//! 設定は shiki-server と同じ（env / TOML）。データプレーンの静止（メンテナンスウィンドウ）中の
//! 実行を前提とする（オンライン移行の整合は保証しない）。

// CLI バイナリ: 標準出力/標準エラーへの出力は正当な用途のため print 系 lint を許容する。
#![allow(clippy::print_stdout, clippy::print_stderr)]

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

#[path = "shiki_admin/keys.rs"]
mod keys;
use keys::{pre_migration_object_key, renamespace_object_key};

// 移行対象テーブルは `storage::tenant_scope` が information_schema から導出する（#420）。
// 手で列挙していた頃は tenant_id を持つ 51 テーブル中 11 しか移行せず、チャット履歴・RAG 本文・
// 構造化データ・ワークフロー履歴・利用量が旧テナントに取り残されていた。

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some("retenant") = args.first().map(String::as_str) {
        retenant(&args[1..]).await
    } else {
        eprintln!(
            "usage: shiki-admin retenant (--legacy | --from <tenant>) --to <tenant> [--actor <id>] [--execute]"
        );
        bail!("不明なサブコマンド");
    }
}

// CLI サブコマンド本体: 引数パース → 各種前提チェック → dry-run/実行の分岐を
// 直列に記述するため長く分岐も多い。運用 CLI の一処理を一望できる利点を優先し許容する。
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
async fn retenant(args: &[String]) -> anyhow::Result<()> {
    // --- 引数パース ---
    let mut from: Option<FromNs> = None;
    let mut to: Option<String> = None;
    let mut execute = false;
    let mut actor: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--legacy" => from = Some(FromNs::Legacy),
            "--from" => {
                let t = it.next().context("--from に値が必要です")?;
                from = Some(FromNs::Tenant(t.clone()));
            }
            "--to" => to = Some(it.next().context("--to に値が必要です")?.clone()),
            "--actor" => actor = Some(it.next().context("--actor に値が必要です")?.clone()),
            "--execute" => execute = true,
            other => bail!("不明な引数: {other}"),
        }
    }
    let from = from.context("--legacy か --from <tenant> のいずれかが必要です")?;
    let to = to.context("--to <tenant> が必要です")?;
    // 文字ポリシーは authz::validate_tenant_id が単一定義（#91 M-3）。CLI だけ緩いと
    // `/` 入り tenant がオブジェクトキーの prefix 境界を壊し、API から削除もできなくなる。
    if let Err(violation) = authz::validate_tenant_id(&to) {
        bail!("--to が tenant_id として不正です: {violation}");
    }
    if let FromNs::Tenant(f) = &from {
        if let Err(violation) = authz::validate_tenant_id(f) {
            bail!("--from が tenant_id として不正です: {violation}");
        }
        if f == &to {
            bail!("--from と --to が同一です");
        }
    }
    // 監査の実行者。`--actor` も $USER も **認証された識別子ではない**（自己申告・偽装可能）ため、
    // 監査 subject には据えない（CodeRabbit）。subject は「CLI 経由の管理操作」を表す固定値とし、
    // 申告値は出所つきで metadata に残す。CLI に OpenFGA 認可を課しても境界にはならない
    // （実行者は DB/FGA/S3 の資格情報を直接持ち生 SQL で同じことができる）が、**誰が実行したと
    // 主張したか**は追えるようにする。
    let (actor_claimed, actor_source) = actor.map_or_else(
        || {
            std::env::var("SUDO_USER")
                .map(|v| (v, "sudo_user"))
                .or_else(|_| std::env::var("USER").map(|v| (v, "user")))
                .unwrap_or_else(|_| ("unknown".to_string(), "none"))
        },
        |a| (a, "flag"),
    );
    let mode = if execute { "EXECUTE" } else { "DRY-RUN" };
    println!("== shiki-admin retenant [{mode}] from={from:?} to={to} ==");

    // --- 依存の配線（shiki-server と同じ設定・migration は適用しない） ---
    let config = AppConfig::load().context("設定のロードに失敗")?;
    let db = PgPoolOptions::new()
        .max_connections(5)
        .connect(&config.database.url)
        .await
        .context("Postgres へ接続できません")?;
    // timeout は必須（#376。理由は main.rs の同等箇所を参照）。
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .context("HTTP クライアントの初期化に失敗")?;
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

    // --- 0. 事前検査（**外部副作用より前**・CodeRabbit） ---
    // 対象テーブルの導出・行数集計・副産物ガードは FGA タプル移送や S3 コピーの**前**に行う。
    // 後ろに置くと --execute で bail した時点で外部状態（FGA / S3）が書き換わっており、
    // DB txn のロールバックでは戻せない＝fail-closed が成立しない。
    // 対象は information_schema から導出し、dry-run でも全テーブルの件数を出す（#420。詳細は
    // storage::tenant_scope の doc）。legacy は object_key のみ書き換えるのでラベルを分ける。
    let tables = storage::tenant_scope::tenant_scoped_tables(&db)
        .await
        .context("テナント境界テーブルの導出に失敗")?;
    let counts = storage::tenant_scope::count_tenant_rows(&db, &tables, &db_tenant)
        .await
        .context("移行対象の行数集計に失敗")?;
    let rename = matches!(from, FromNs::Tenant(_));
    let label = if rename {
        "移行対象"
    } else {
        "参考: legacy は object_key のみ・tenant_id は不変"
    };
    println!(
        "{}",
        storage::tenant_scope::format_row_report(&counts, label)
    );
    // 副産物を移送できないサブシステムに行があれば dry-run でも実行でも拒否する（fail-closed）。
    if rename {
        if let Some(reason) = storage::tenant_scope::sidecar_migration_blocker(&counts) {
            bail!(reason);
        }
    }

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

    // --- 2. オブジェクトの移行（blob.object_key を正として copy のみ・旧キーは残す） ---
    // 旧キーの削除は **DB 書き換え（手順3）の commit 後**に行う（#91 M-5）: ここで削除すると
    // 手順3 の txn が失敗した場合に DB の object_key は旧キーを指すのに実体が無い
    // （FGA=新・オブジェクト=新のみ・DB=旧の三者不整合）dangling 状態になり、再実行まで
    // テナントが完全に不可用になる。copy→commit→delete の順なら途中失敗しても実体は必ず
    // DB が指すキーに存在する（余剰コピーが残るだけ・再実行/手順4 で収束）。
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
                // 冪等: コピー済み or 旧が無い（前回実行で移行済み）ならスキップ。
                if !store.exists(&new_key).await? {
                    if !store.exists(old_key).await? {
                        objects_skipped += 1;
                        continue;
                    }
                    store.copy(old_key, &new_key).await?;
                }
            }
            objects_moved += 1;
        }
    }
    println!("objects: copied={objects_moved} skipped={objects_skipped}（blob 行 {blob_rows}）");

    // --- 3. DB の書き換え（1 txn） ---
    if execute {
        let mut tx = db.begin().await?;
        // 監査メタデータ用（移行範囲の証跡）。legacy は tenant_id を動かさないので 0 のまま。
        let mut db_moved: Vec<(String, u64)> = Vec::new();
        let mut db_rows: u64 = 0;
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
                // 参照列に tenant_id を含む FK は commit 時検査へ遅延する（migration 0008/0061）。
                db_moved = storage::tenant_scope::rename_tenant_rows(&mut tx, &tables, f, &to)
                    .await
                    .context("tenant_id のリネームに失敗")?;
                db_rows = db_moved.iter().map(|(_, n)| *n).sum();
                println!("DB: {} テーブル / {db_rows} 行を移行", db_moved.len());
            }
        }
        tx.commit().await?;
        println!("DB: 書き換え完了");

        // --- 4. 旧オブジェクトキーの削除（DB commit 後・#91 M-5） ---
        // commit 済みの blob 行（新キー）から旧キーを導出して削除する。ここで失敗しても
        // DB は新キーを指しており実体も存在する（旧キーが残るだけ）。再実行で収束する。
        let mut objects_deleted: u64 = 0;
        let mut last_key: Option<String> = None;
        loop {
            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT object_key FROM blob \
                 WHERE tenant_id = $1 AND ($2::text IS NULL OR object_key > $2) \
                 ORDER BY object_key LIMIT 1000",
            )
            .bind(&to)
            .bind(last_key.as_deref())
            .fetch_all(&db)
            .await?;
            if rows.is_empty() {
                break;
            }
            last_key = rows.last().map(|(k,)| k.clone());
            for (new_key,) in &rows {
                let Some(old_key) = pre_migration_object_key(new_key, &from, &to) else {
                    continue; // 新形式でない行（想定外）は触らない。
                };
                if store.exists(&old_key).await? {
                    store.delete(&old_key).await?;
                    objects_deleted += 1;
                }
            }
        }
        println!("objects: 旧キー {objects_deleted} 件を削除");

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
                    kind: authz::PrincipalKind::User,
                    id: "cli".into(),
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
                        // 移行範囲の証跡（#420）: 何テーブル・何行動かしたか。
                        "tables": db_moved.len(), "rows": db_rows,
                        // 実行者の**申告値**（認証されていない・出所つき）。
                        "actor_claimed": actor_claimed, "actor_source": actor_source,
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
            // timeout を必ず付ける（CodeRabbit）。無期限クライアントだと Keycloak が hang した
            // ときに `retenant --execute` が終了せず、DB は更新済み・IdP は未追従の中途状態で
            // 張り付く。1 リクエストあたりの上限なので、ユーザー数が多くても打ち切られない。
            let kc_http = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .context("Keycloak 用 HTTP クライアントの初期化に失敗")?;
            match KeycloakAdmin::from_config(&kc_http, &config.auth) {
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
