//! `shiki-admin workflow-conditions [--tenant <t>] [--all-versions] [--batch <n>]`（Task 15.7）。
//!
//! 条件の意味論の切替（2026-10・query-optimizer.md §3.1）の**前に**、保存済みワークフローのうち
//! 配列値に `eq` / `neq` / `in` / `contains` / `starts_with` / `ends_with` を使う条件（空配列の `exists` / `is_null`・`1.0` との等値を含む）を
//! 一覧にする。検出規則は保存時の警告と同じ（`workflow_engine::semantics_warnings`）。
//!
//! - 既定は各ワークフローの**最新版**。`--all-versions` で過去版も（ピンした版で走る run がある場合）。
//! - 読取専用。出力は TSV（tenant・workflow_id・name・version・node・path・message）と件数。

use anyhow::{bail, Context};
use api::config::AppConfig;
use api::workflow_semantics_audit::scan_page;
use sqlx::postgres::PgPoolOptions;

const DEFAULT_BATCH: i64 = 200;

pub(crate) async fn run(args: &[String]) -> anyhow::Result<()> {
    let mut tenant: Option<String> = None;
    let mut all_versions = false;
    let mut batch = DEFAULT_BATCH;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--tenant" => tenant = Some(it.next().context("--tenant に値が必要です")?.clone()),
            "--all-versions" => all_versions = true,
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
    if let Some(t) = &tenant {
        if let Err(violation) = authz::validate_tenant_id(t) {
            bail!("--tenant が tenant_id として不正です: {violation}");
        }
    }
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

    println!("tenant_id\tworkflow_id\tname\tversion\tnode\tpath\tmessage");
    let mut after = None;
    let mut findings = 0usize;
    let mut workflows = std::collections::BTreeSet::new();
    loop {
        let (page, next) = scan_page(&db, tenant.as_deref(), all_versions, after.as_ref(), batch)
            .await
            .context("保存済みワークフローの走査に失敗")?;
        for f in page {
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                f.tenant_id,
                f.workflow_id,
                f.name,
                f.version,
                f.node_id.as_deref().unwrap_or("(trigger)"),
                f.path.as_deref().unwrap_or(""),
                f.message
            );
            findings += 1;
            workflows.insert((f.tenant_id, f.workflow_id));
        }
        match next {
            Some(n) => after = Some(n),
            None => break,
        }
    }
    eprintln!(
        "振る舞いが変わり得る条件: {findings} 件（ワークフロー {} 本）",
        workflows.len()
    );
    Ok(())
}
