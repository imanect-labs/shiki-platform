//! テーブルの「形」の取り出し（query-optimizer.md §9.3・Task 15.12）。
//!
//! 性能ベンチのデータは**顧客データを使わず、統計から合成する**（Garoon の先例）。ここでは実テナントの
//! テーブルの形——行数・列の型・値の種類数・頻度の形・値なしの率・作成順との相関・多値の要素数・
//! リンクのファンアウト——を、**値を含まない数値として**取り出す。値（最頻値・ヒストグラムの境界）・
//! 列の参照名・テーブル名は出さない（列は並び順の番号だけ）。統計の読取経路は planner に閉じたまま
//! （PIT-75）で、これは運用者がベンチを作るための数値であり利用者へは返さない。

use serde::{Deserialize, Serialize};

use super::catalog::{load, StatsCache};
use crate::index::IndexTable;
use crate::model::{FieldType, TableKeys, TableSchema, FIRST_USER_FIELD_ID};
use crate::DataError;

/// 列の型（合成側が値の作り方を決めるのに要る分だけ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapeType {
    /// 分類（select / user_ref / role_ref・短い値）。
    Category,
    /// 複数選択（要素ごとに索引行）。
    MultiCategory,
    /// 自由文・日付・日時（text の値索引）。
    Text,
    /// 数値。
    Number,
}

/// 1 列の形（値を含まない）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnShape {
    /// 列の並び順の番号（参照名は出さない）。
    pub ordinal: usize,
    pub ty: ShapeType,
    /// 値なしの率（0〜1）。
    pub null_rate: f64,
    /// 異なり数。
    pub distinct: u64,
    /// 最頻値の頻度の形（降順・値を持つ行に対する割合・最大 100 個）。値そのものは含まない。
    pub top_freqs: Vec<f64>,
    /// 頻度の形に当てはめた Zipf の指数（`freq ∝ rank^-s`）。
    pub zipf_s: f64,
    /// 多値の平均要素数。
    pub avg_elems: f64,
    /// 索引の長さ上限で切り詰めた値の率。
    pub truncated_rate: f64,
    /// 値の並びと作成順（record_int）の相関（-1〜1）。状態と作成日のような列間の相関の強さ。
    pub creation_correlation: f64,
}

/// リンク列の形（方向ごと）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkShape {
    pub reverse: bool,
    /// リンクを持つ行の割合。
    pub link_rate: f64,
    pub avg_fanout: f64,
    pub max_fanout: f64,
}

/// テーブルの形（ベンチの合成の入力）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableShape {
    pub n_rows: u64,
    pub columns: Vec<ColumnShape>,
    pub links: Vec<LinkShape>,
}

/// 最頻値の頻度から Zipf の指数を最小二乗で当てはめる（`ln f = c − s · ln rank`）。
pub(crate) fn fit_zipf(freqs: &[f64]) -> f64 {
    let pts: Vec<(f64, f64)> = freqs
        .iter()
        .enumerate()
        .filter(|(_, f)| **f > 0.0)
        .map(|(i, f)| {
            #[allow(clippy::cast_precision_loss)]
            let r = (i + 1) as f64;
            (r.ln(), f.ln())
        })
        .collect();
    if pts.len() < 2 {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let n = pts.len() as f64;
    let mx = pts.iter().map(|p| p.0).sum::<f64>() / n;
    let my = pts.iter().map(|p| p.1).sum::<f64>() / n;
    let sxy: f64 = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    let sxx: f64 = pts.iter().map(|p| (p.0 - mx) * (p.0 - mx)).sum();
    if sxx <= 0.0 {
        0.0
    } else {
        (-sxy / sxx).max(0.0)
    }
}

/// 統計からテーブルの形を作る（統計が無ければ `None`）。
pub(crate) async fn extract(
    conn: &mut sqlx::PgConnection,
    keys: TableKeys,
    schema: &TableSchema,
) -> Result<Option<TableShape>, DataError> {
    // 形の取り出しはキャッシュを使わない（直前の統計収集の結果を読む）。
    let Some(stats) = load(conn, &StatsCache::default(), keys).await? else {
        return Ok(None);
    };
    let n = stats.n_rows.max(1.0);
    let mut columns = Vec::new();
    let user_fields = schema
        .fields
        .iter()
        .filter(|f| f.id >= FIRST_USER_FIELD_ID && !f.field_type.is_derived());
    for (ordinal, f) in user_fields.enumerate() {
        let (ty, table) = match f.field_type {
            FieldType::Select | FieldType::UserRef | FieldType::RoleRef => {
                (ShapeType::Category, IndexTable::Text)
            }
            FieldType::MultiSelect => (ShapeType::MultiCategory, IndexTable::Text),
            FieldType::Number => (ShapeType::Number, IndexTable::Num),
            FieldType::Text | FieldType::Date | FieldType::DateTime => {
                (ShapeType::Text, IndexTable::Text)
            }
            _ => continue,
        };
        let Some(fs) = stats.field(f.id, table) else {
            continue;
        };
        let top_freqs: Vec<f64> = fs.mcv.iter().map(|(_, fr)| *fr).collect();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        columns.push(ColumnShape {
            ordinal,
            ty,
            null_rate: (1.0 - fs.n_values / n).clamp(0.0, 1.0),
            distinct: fs.n_distinct.max(0.0) as u64,
            zipf_s: fit_zipf(&top_freqs),
            top_freqs,
            avg_elems: if fs.n_values > 0.0 {
                fs.n_entries / fs.n_values
            } else {
                1.0
            },
            truncated_rate: fs.truncated_frac,
            creation_correlation: fs.correlation,
        });
    }
    let mut links: Vec<LinkShape> = stats
        .links
        .iter()
        .map(|((_, rev), l)| LinkShape {
            reverse: *rev,
            link_rate: l.link_frac,
            avg_fanout: if l.n_src > 0.0 {
                l.n_edges / l.n_src
            } else {
                0.0
            },
            max_fanout: l.max_fanout,
        })
        .collect();
    links.sort_by(|a, b| {
        a.reverse
            .cmp(&b.reverse)
            .then(a.link_rate.total_cmp(&b.link_rate))
    });
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(Some(TableShape {
        n_rows: stats.n_rows as u64,
        columns,
        links,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zipf_exponent_is_recovered_from_frequencies() {
        let f: Vec<f64> = (1..=50).map(|r| 1.0 / f64::from(r).powf(1.2)).collect();
        assert!((fit_zipf(&f) - 1.2).abs() < 1e-6);
        assert!(fit_zipf(&[0.5]).abs() < 1e-9);
        assert!(fit_zipf(&[0.25, 0.25, 0.25]).abs() < 1e-9, "一様は 0");
    }
}
