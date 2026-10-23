//! コストモデルと単価の較正（query-optimizer.md §5.2）。
//!
//! コストは**予測レイテンシ（µs）**で表す（受付制御 §5.4 と `statement_timeout` の判断にそのまま使う）。
//! 単価の初期値は §5.2 の仮置きで、**起動時の較正ベンチ**（[`calibrate`]: 較正用の固定表
//! `data_planner_calib` の既知のデータで各操作を計測）で環境ごとに置き換える。較正の前後の単価は [`CalibrationReport`] に残し、予測と実測の
//! 相関はフィードバック（[`super::feedback`]）が記録する。
//!
//! 戦略の式（`n` = 行数・`s(X)` = X の推定選択率・`L` = 1 ページの件数）:
//!
//! ```text
//! 検査の期待コスト  probe(R) = Σᵢ ( Π_{j<i} pⱼ ) · c_probe   … よく落ちる述語から順に
//! SortDrive   = walk · (c_tuple + probe(Q∧P)) + L · c_fetch,  walk = min(n, L / s(Q∧P)) · 補正係数
//! 候補駆動    = entries · c_tuple + cand · (probe(残り) + k · c_probe) + c_sort(pass, L) + L · c_fetch
//! ```

use std::sync::RwLock;
use std::time::Instant;

use sqlx::PgConnection;

use crate::{map_db, DataError};

/// 単価（µs）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnitCosts {
    /// 索引の範囲走査 1 エントリ。
    pub c_tuple: f64,
    /// 点検索 1 回（PK 降下＋可視性確認）。
    pub c_probe: f64,
    /// 本体 1 行の取得。
    pub c_fetch: f64,
    /// DB 往復 1 回。
    pub c_rtt: f64,
    /// 上位 L 件のソートの係数（`n · log₂L · c_sort`）。
    pub c_sort: f64,
}

impl Default for UnitCosts {
    /// §5.2 の初期値（仮置き）。
    fn default() -> Self {
        UnitCosts {
            c_tuple: 1.0,
            c_probe: 5.0,
            c_fetch: 10.0,
            c_rtt: 300.0,
            c_sort: 0.05,
        }
    }
}

/// 較正の結果（単価の前後）。
#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationReport {
    pub before: UnitCosts,
    pub after: UnitCosts,
    /// 各操作の計測の反復回数。
    pub runs: usize,
}

/// プロセスの単価（較正で置き換わる）。
#[derive(Debug, Default)]
pub(crate) struct CostModel {
    units: RwLock<UnitCosts>,
    report: RwLock<Option<CalibrationReport>>,
}

impl CostModel {
    pub(crate) fn units(&self) -> UnitCosts {
        *self
            .units
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn set(&self, u: UnitCosts) {
        *self
            .units
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = u;
    }

    pub(crate) fn report(&self) -> Option<CalibrationReport> {
        self.report
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn set_report(&self, r: CalibrationReport) {
        *self
            .report
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(r);
    }
}

impl UnitCosts {
    /// 検査の期待コスト（選択率の小さい＝よく落ちる述語から順に評価する・単価は共通）。
    pub(crate) fn probe(&self, rates: &[f64]) -> f64 {
        let mut v: Vec<f64> = rates.iter().map(|s| s.clamp(0.0, 1.0)).collect();
        v.sort_by(f64::total_cmp);
        let mut pass = 1.0;
        let mut cost = 0.0;
        for s in v {
            cost += pass * self.c_probe;
            pass *= s;
        }
        cost
    }

    /// 上位 L 件のソート。
    pub(crate) fn sort(&self, n: f64, limit: f64) -> f64 {
        n.max(0.0) * limit.max(2.0).log2() * self.c_sort
    }

    /// ソート駆動: `walk` エントリを歩き、各行で全検査（`rates` は各検査の通過率）。
    pub(crate) fn sort_drive(&self, walk: f64, rates: &[f64], limit: f64) -> f64 {
        walk.max(0.0) * (self.c_tuple + self.probe(rates)) + limit * self.c_fetch + self.c_rtt
    }

    /// 候補駆動（フィルタ・OR・認可・交差）: 入口 `entries` エントリ → 候補 `cand` 行に残りの検査
    /// （`rest`）とソート値の点検索（`sort_keys`）→ 通過 `pass` 行から上位 L 件。
    pub(crate) fn cand_drive(
        &self,
        entries: f64,
        cand: f64,
        rest: &[f64],
        pass: f64,
        sort_keys: usize,
        limit: f64,
    ) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let k = sort_keys as f64;
        entries.max(0.0) * self.c_tuple
            + cand.max(0.0) * self.probe(rest)
            + pass.max(0.0) * k * self.c_probe
            + self.sort(pass, limit)
            + limit * self.c_fetch
            + self.c_rtt
    }

    /// ソート駆動の 1 エントリあたりの単価（予算の換算・§6.2）。
    pub(crate) fn per_entry(&self, n_checks: usize) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let n = n_checks as f64;
        self.c_tuple + n * self.c_probe
    }
}

/// 較正ベンチの表の行数（migration 0096 の `generate_series(1, 20000)` と一致させる）。
const CALIB_ROWS: i64 = 20_000;
/// 点検索・本体取得の回数。
const CALIB_PROBES: i64 = 2_000;
/// 各操作の反復回数（中央値を採る）。
const CALIB_RUNS: usize = 5;

/// 起動時の較正ベンチ: 較正用の固定表（`data_planner_calib`・migration 0096 の既知の合成データ）で
/// 範囲走査・点検索・本体取得・往復を計測して単価を置き換える。読取だけで DDL は打たない。
/// 顧客データには触れない。
pub(crate) async fn calibrate(
    conn: &mut PgConnection,
    model: &CostModel,
) -> Result<CalibrationReport, DataError> {
    use sqlx::Connection as _;
    let before = model.units();
    let mut tx = conn.begin().await.map_err(map_db)?;
    let out = calibrate_in_tx(&mut tx, before).await;
    // SET LOCAL を捨てる（読取だけなので書き戻すものは無い）。
    let _ = tx.rollback().await;
    let after = out?;
    model.set(after);
    let r = CalibrationReport {
        before,
        after,
        runs: CALIB_RUNS,
    };
    model.set_report(r.clone());
    Ok(r)
}

async fn calibrate_in_tx(
    conn: &mut PgConnection,
    before: UnitCosts,
) -> Result<UnitCosts, DataError> {
    // 計画を固定する（索引走査・点検索の単価を測るため。この Tx の中だけ）。
    for s in [
        "SET LOCAL enable_seqscan = off",
        "SET LOCAL enable_bitmapscan = off",
        "SET LOCAL plan_cache_mode = force_custom_plan",
    ] {
        sqlx::query(s).execute(&mut *conn).await.map_err(map_db)?;
    }
    // 較正用の固定表（migration 0096 が作る・テナントに属さない合成データ）。ランタイム DDL は打たない
    // （15.3: ランタイム DDL ゼロ）。行数が想定と違えば計測が意味を持たないので較正しない。
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM data_planner_calib")
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db)?;
    if rows != CALIB_ROWS {
        return Err(DataError::Internal(format!(
            "較正用の表の行数が想定と違います（{rows} 行・想定 {CALIB_ROWS} 行）"
        )));
    }
    let rtt = median(conn, "SELECT 1", None).await?;
    let scan = median(
        conn,
        "SELECT count(*) FROM (SELECT c.r FROM data_planner_calib c WHERE c.k = 1 \
           ORDER BY c.v, c.r LIMIT $1) s",
        Some(CALIB_ROWS),
    )
    .await?;
    let probe = median(
        conn,
        "SELECT count(*) FROM generate_series(1, $1::int8) g WHERE EXISTS ( \
           SELECT 1 FROM data_planner_calib c WHERE c.k = 1 AND c.v = 'v' || (g % 97) \
             AND c.r = (g * 7919) % 20000 + 1 OFFSET 0)",
        Some(CALIB_PROBES),
    )
    .await?;
    let fetch = median(
        conn,
        "SELECT sum(length(c.d::text)) FROM data_planner_calib c \
         WHERE c.r = ANY(ARRAY(SELECT (g * 7919) % 20000 + 1 FROM generate_series(1, $1::int8) g))",
        Some(CALIB_PROBES),
    )
    .await?;
    let sort = median(
        conn,
        "SELECT r FROM (SELECT c.r, c.d ->> 'c' AS s FROM data_planner_calib c WHERE c.k = 1 \
           ORDER BY c.v, c.r LIMIT $1) z ORDER BY z.s DESC, z.r LIMIT 20",
        Some(CALIB_ROWS),
    )
    .await?;
    #[allow(clippy::cast_precision_loss)]
    let per = |t: f64, n: i64| ((t - rtt).max(0.0) / n as f64).clamp(0.01, 1_000.0);
    let c_tuple = per(scan, CALIB_ROWS);
    // 点検索・本体取得の文は駆動（generate_series / 配列）の分も含むので、範囲走査の分を差し引く。
    let c_probe = (per(probe, CALIB_PROBES) - c_tuple).max(c_tuple);
    let c_fetch = per(fetch, CALIB_PROBES).max(c_tuple);
    #[allow(clippy::cast_precision_loss)]
    let c_sort = ((sort - scan).max(0.0) / (CALIB_ROWS as f64 * 20f64.log2())).clamp(0.001, 10.0);
    Ok(UnitCosts {
        c_tuple,
        c_probe,
        c_fetch,
        c_rtt: rtt.clamp(1.0, 100_000.0),
        c_sort: if c_sort.is_finite() {
            c_sort
        } else {
            before.c_sort
        },
    })
}

/// 文の所要時間（µs）の中央値（最初の 1 回は温めとして捨てる）。
async fn median(conn: &mut PgConnection, sql: &str, arg: Option<i64>) -> Result<f64, DataError> {
    let mut v = Vec::with_capacity(CALIB_RUNS);
    for i in 0..=CALIB_RUNS {
        let q = sqlx::query(sql);
        let q = match arg {
            Some(a) => q.bind(a),
            None => q,
        };
        let t = Instant::now();
        q.fetch_all(&mut *conn).await.map_err(map_db)?;
        if i > 0 {
            v.push(t.elapsed().as_secs_f64() * 1e6);
        }
    }
    v.sort_by(f64::total_cmp);
    Ok(v[v.len() / 2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_cost_evaluates_selective_checks_first() {
        let u = UnitCosts::default();
        // 0.1 → 0.9 の順: 5 + 0.1·5 = 5.5（並び替えで逆順にならない）。
        assert!((u.probe(&[0.9, 0.1]) - 5.5).abs() < 1e-9);
        assert!((u.probe(&[]) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn sort_drive_vs_filter_drive_follow_section_1_2() {
        // §1.2 ①: 100 万行・承認待ち 300 行（0.03%）を金額順 20 件。
        let u = UnitCosts::default();
        let s = 300.0 / 1e6;
        let sort = u.sort_drive(20.0 / s, &[s], 20.0);
        let filter = u.cand_drive(300.0, 300.0, &[], 300.0, 1, 20.0);
        assert!(sort > 50.0 * filter, "sort {sort} filter {filter}");
        // ②: 完了 90% ならソート駆動が桁違いに安い。
        let s = 0.9;
        let sort = u.sort_drive(20.0 / s, &[s], 20.0);
        let filter = u.cand_drive(9e5, 9e5, &[], 9e5, 1, 20.0);
        assert!(filter > 1_000.0 * sort);
    }
}
