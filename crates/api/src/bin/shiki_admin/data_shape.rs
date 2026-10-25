//! `shiki-admin data-table-shape --tenant <t> --table <uuid> [--out <path>]`（Task 15.12・監査 B2-5）。
//!
//! 実テナントのテーブルの**形**（行数・列の型・値の種類数・頻度の形（最頻値の頻度と Zipf の指数）・値なしの率・
//! 作成順との相関・多値の要素数・リンクのファンアウト）を、**値を含まない数値**の JSON として出す
//! （query-optimizer.md §9.3・Garoon の先例）。値・列の参照名・テーブル名は含まない（列は並び順の番号だけ）。
//! 出力を `DATA_BENCH_SHAPE=<path>` で `crates/data/tests/optimizer_bench_it.rs` に渡すと、その形から
//! 合成したデータでベンチを回せる（顧客データをベンチに持ち込まない）。
//!
//! - 形は統計収集ジョブ（`StatsWorker`）の結果から作る。統計がまだ無いテーブルは終了コード 1。
//! - 読取専用。認可を経ない運用経路（DB の資格情報を持つ運用者の CLI）なので API には出さない。

use std::path::PathBuf;

use anyhow::{bail, Context};
use api::config::AppConfig;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

/// 引数（`--tenant` / `--table` / `--out`）。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Args {
    pub tenant: String,
    pub table: Uuid,
    pub out: Option<PathBuf>,
}

pub(crate) fn parse(args: &[String]) -> anyhow::Result<Args> {
    let mut tenant: Option<String> = None;
    let mut table: Option<Uuid> = None;
    let mut out: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--tenant" => tenant = Some(it.next().context("--tenant に値が必要です")?.clone()),
            "--table" => {
                let v = it.next().context("--table に値が必要です")?;
                table = Some(Uuid::parse_str(v).context("--table はテーブル ID（UUID）です")?);
            }
            "--out" => out = Some(PathBuf::from(it.next().context("--out に値が必要です")?)),
            other => bail!("不明な引数: {other}"),
        }
    }
    let tenant = tenant.context("--tenant <t> が必要です")?;
    if let Err(violation) = authz::validate_tenant_id(&tenant) {
        bail!("--tenant が tenant_id として不正です: {violation}");
    }
    Ok(Args {
        tenant,
        table: table.context("--table <uuid> が必要です")?,
        out,
    })
}

pub(crate) async fn run(args: &[String]) -> anyhow::Result<()> {
    let args = parse(args)?;
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
    let pools = storage::PoolRouter::single(db);
    let Some(shape) = data::extract_table_shape(&pools, &args.tenant, args.table)
        .await
        .context("テーブルの形の取り出しに失敗")?
    else {
        bail!("テーブルが無いか、統計がまだありません（統計収集ジョブの完了を待ってください）");
    };
    let json = serde_json::to_string_pretty(&shape).context("JSON への直列化に失敗")?;
    match &args.out {
        Some(p) => {
            std::fs::write(p, json.as_bytes())
                .with_context(|| format!("{} へ書けません", p.display()))?;
            eprintln!(
                "テーブルの形（行数 {}・列 {}）を {} へ書きました",
                shape.n_rows,
                shape.columns.len(),
                p.display()
            );
        }
        None => println!("{json}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn parses_tenant_table_and_out() {
        let id = Uuid::new_v4();
        let a = parse(&s(&[
            "--tenant",
            "acme",
            "--table",
            &id.to_string(),
            "--out",
            "x.json",
        ]))
        .unwrap();
        assert_eq!(
            a,
            Args {
                tenant: "acme".into(),
                table: id,
                out: Some(PathBuf::from("x.json")),
            }
        );
        assert!(parse(&s(&["--tenant", "acme"])).is_err(), "--table は必須");
        assert!(
            parse(&s(&["--table", &id.to_string()])).is_err(),
            "--tenant は必須"
        );
        assert!(parse(&s(&["--tenant", "a/b", "--table", &id.to_string()])).is_err());
        assert!(parse(&s(&["--tenant", "acme", "--table", "x"])).is_err());
        assert!(parse(&s(&["--bogus"])).is_err());
    }
}
