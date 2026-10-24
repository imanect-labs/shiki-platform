//! `shiki-admin secrets-reencrypt [--execute] [--batch <n>]`（#814）。
//!
//! #814 以前に AAD なしで暗号化されたシークレット（`secret.enc_version = 0`）を、行
//! （tenant_id・org・id）へ束縛した現行版へ再暗号化する。利用されたシークレットは解決時に
//! 遅延移行されるが、使われない行はこのコマンドで移す。
//!
//! - 既定は **dry-run**（旧版行の件数のみ）。`--execute` で実行。
//! - 行単位・条件付き UPDATE（`secrets::reencrypt_legacy_secrets`）。DEK は元の行のものを再利用し、
//!   途中で止めても全行が復号可能なまま（冪等・再実行で収束）。shiki-server の稼働中に流してよい。
//! - 復号できない行（鍵不一致・改竄）は**書き換えず残し**、件数を報告して非 0 で終わる。

use anyhow::{bail, Context};
use api::config::AppConfig;
use sqlx::postgres::PgPoolOptions;

/// 1 ページの既定件数。
const DEFAULT_BATCH: i64 = 500;

pub(crate) async fn run(args: &[String]) -> anyhow::Result<()> {
    let mut execute = false;
    let mut batch = DEFAULT_BATCH;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--execute" => execute = true,
            "--batch" => {
                let v = it.next().context("--batch に値が必要です")?;
                batch = v.parse().context("--batch は正の整数です")?;
                if batch <= 0 {
                    bail!("--batch は正の整数です");
                }
            }
            other => bail!("不明な引数: {other}"),
        }
    }
    let mode = if execute { "EXECUTE" } else { "DRY-RUN" };
    println!("== shiki-admin secrets-reencrypt [{mode}] ==");

    let config = AppConfig::load().context("設定のロードに失敗")?;
    let db_options: sqlx::postgres::PgConnectOptions = config
        .database
        .url
        .parse()
        .context("DATABASE_URL の解析に失敗")?;
    let db = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(db_options.application_name("shiki-admin"))
        .await
        .context("Postgres へ接続できません")?;

    let legacy = secrets::count_legacy_secrets(&db)
        .await
        .context("旧版シークレットの件数取得に失敗")?;
    println!("旧版（AAD なし）シークレット: {legacy} 件");
    if !execute {
        println!("DRY-RUN のため書き換えなし。--execute で実行します。");
        return Ok(());
    }

    // shiki-server と同じマスターキー（KeyProvider）で DEK を解く。
    let path = config
        .secrets
        .master_key_file
        .as_deref()
        .context("secrets.master_key_file が未設定です（shiki-server と同じ鍵が必要）")?;
    let provider = secrets::LocalKeyFileProvider::from_file(std::path::Path::new(path))
        .context("secrets マスターキーの読み込みに失敗")?;
    let report = secrets::reencrypt_legacy_secrets(&db, &provider, batch)
        .await
        .context("再暗号化に失敗")?;
    println!(
        "secrets: scanned={} upgraded={} skipped(並行更新済み)={} failed={}",
        report.scanned, report.upgraded, report.skipped, report.failed
    );
    if report.failed > 0 {
        bail!(
            "{} 件を復号できず旧版のまま残しました（tenant_id / secret_id はログ参照）",
            report.failed
        );
    }
    Ok(())
}
