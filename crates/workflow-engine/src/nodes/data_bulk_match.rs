//! まとめて登録の同じレコードを探す列（`data.bulk_upsert` の `match_on`・GAP-16）の取込: 照合して取り込み、
//! 照合の後に同じ値で先に追加された行を、そのレコードの更新として取り込み直す。
//!
//! 照合（[`super::data_upsert::match_bulk_rows`]）で当たらなかった行は追加になる。同じ値を同時に取り込む別の実行が
//! 照合と取込の間に追加すると、こちらの追加は探す列の一意（`data_unique`）で断られる（検証で不合格〔`rows_invalid`〕か、
//! 適用で拒否〔`rows_rejected`〕）。そのままだと値が書かれないのに step は成功で終わる。`data.record.upsert` が
//! 一意の違反の後に読み直して書き換えに回るのと同じく、断られた追加の行を探し直し、いまは見えるレコードに当たった
//! 行だけを更新として取り込み直す（1 回だけ）。
//!
//! - 取り込み直す行は、`id` を書いていない行のうち断られた行（結果の `errors` に出た行）で、探し直していま見える
//!   レコードに当たったものだけ。誤りの文言は読まない（当たらない行は、見えない行の値・ほかの列の誤りなので、誤りの
//!   まま残す。当てたのにほかの理由で書けなかった行は、取り込み直してもまた断られ、数は変わらない）。照合の結果では
//!   なく 1 回目の取込の誤りから選ぶので、step をやり直したとき（照合の結果が 1 回目と違う）も同じ行を選ぶ。
//! - `abort_on_invalid` で何も取り込まなかった（不合格の行があり、取込が `failed`）ときは、断られた行のどれかが
//!   探し直して当たったら、探し直した行で全体を取り込み直す（何も書いていないので、全体でやり直せる）。
//! - 取り込み直しの取込 ID は冪等キーに印を足したものから決まる（step をやり直しても二重に取り込まない）。
//! - 結果は 1 回の取込と同じ形で返す。件数は 2 回の取込を合わせ、取り込み直した行の前の誤りは消し、取り込み直しの
//!   誤りは元の行の番号で返す。当てた行の数（`rows_matched`）には探し直して当てた行も数える。

use std::collections::HashSet;

use data::BulkUpsertRow;
use serde_json::{json, Value};
use uuid::Uuid;

use super::data_upsert::match_bulk_rows;
use super::ports::{DataWriteOp, DataWriteReq, ExecCtx, NodePorts, PortError};

/// 取り込み直しの取込 ID を決める冪等キーの印。
const REMATCH_MARK: &str = "#rematch";

/// まとめて登録の要求（同じレコードを探す列つき）。
pub(crate) struct MatchedBulkReq<'a> {
    pub table_id: Uuid,
    pub match_on: &'a str,
    pub rows: Vec<BulkUpsertRow>,
    pub abort_on_invalid: bool,
    pub idempotency_key: &'a str,
}

/// 照合して取り込み、照合の後に同じ値で先に追加された行を更新として取り込み直す（上のモジュールの説明）。
/// 返すのは取込の結果に当てた行の数（`rows_matched`）を足したもの。
pub(crate) async fn bulk_matched(
    ports: &dyn NodePorts,
    ec: &ExecCtx,
    req: MatchedBulkReq<'_>,
) -> Result<Value, PortError> {
    let MatchedBulkReq {
        table_id,
        match_on,
        mut rows,
        abort_on_invalid,
        idempotency_key,
    } = req;
    // id を書いた行は探さない（断られても、別のレコードに当て替えない）。
    let explicit: Vec<bool> = rows.iter().map(|r| r.record_id.is_some()).collect();
    match_bulk_rows(ports, ec, table_id, match_on, &mut rows).await?;
    let first = import(
        ports,
        ec,
        table_id,
        &rows,
        abort_on_invalid,
        idempotency_key,
    )
    .await?;
    let failed = failed_rows(&first, rows.len());
    // 断られた、探して当てる行（追加に回した行と、当てたのに書けなかった行）。
    let candidates: Vec<usize> = (0..rows.len())
        .filter(|i| !explicit[*i] && failed.contains(i))
        .collect();
    if candidates.is_empty() {
        return Ok(with_matched(first, matched_count(&rows, &explicit)));
    }
    // 探し直す（照合の結果は捨て、いま見えるレコードに当て直す）。
    let mut again: Vec<BulkUpsertRow> = candidates
        .iter()
        .map(|&i| BulkUpsertRow {
            record_id: None,
            ..rows[i].clone()
        })
        .collect();
    let found = match_bulk_rows(ports, ec, table_id, match_on, &mut again).await?;
    if found == 0 {
        return Ok(with_matched(first, matched_count(&rows, &explicit)));
    }
    let retry_key = format!("{idempotency_key}{REMATCH_MARK}");
    if aborted(&first, abort_on_invalid) {
        // 何も取り込んでいない: 探し直した行で全体を取り込み直す（当たらない行は追加のまま）。
        for (&i, r) in candidates.iter().zip(again) {
            rows[i] = r;
        }
        let second = import(ports, ec, table_id, &rows, abort_on_invalid, &retry_key).await?;
        return Ok(with_matched(second, matched_count(&rows, &explicit)));
    }
    // 当たった行だけを、そのレコードの更新として取り込み直す。
    let mut retried = Vec::new();
    let mut retry_rows = Vec::new();
    for (&i, r) in candidates.iter().zip(again) {
        if r.record_id.is_some() {
            rows[i].record_id = r.record_id;
            retried.push(i);
            retry_rows.push(r);
        }
    }
    let second = import(
        ports,
        ec,
        table_id,
        &retry_rows,
        abort_on_invalid,
        &retry_key,
    )
    .await?;
    Ok(with_matched(
        merge(first, &second, &retried),
        matched_count(&rows, &explicit),
    ))
}

/// 探して当てた行の数（id を書いた行を除く・最後に取り込んだときの当て方）。
fn matched_count(rows: &[BulkUpsertRow], explicit: &[bool]) -> usize {
    rows.iter()
        .zip(explicit)
        .filter(|(r, e)| !**e && r.record_id.is_some())
        .count()
}

/// 1 回の取込（ポートの一括書込）。
async fn import(
    ports: &dyn NodePorts,
    ec: &ExecCtx,
    table_id: Uuid,
    rows: &[BulkUpsertRow],
    abort_on_invalid: bool,
    key: &str,
) -> Result<Value, PortError> {
    let req = DataWriteReq {
        table_id,
        op: DataWriteOp::BulkUpsert {
            rows: rows.to_vec(),
            abort_on_invalid,
        },
        idempotency_key: key.to_string(),
    };
    ports.data_write(ec, req).await
}

/// 不合格・拒否の行（0 から数えた位置）。結果の `errors` に出た行だけ（多いと先頭から一部）。
fn failed_rows(out: &Value, len: usize) -> HashSet<usize> {
    out.get("errors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let row = usize::try_from(e.get("row")?.as_i64()?).ok()?;
            (1..=len).contains(&row).then(|| row - 1)
        })
        .collect()
}

/// 取込の数（無ければ 0）。
fn count(out: &Value, key: &str) -> i64 {
    out.get(key).and_then(Value::as_i64).unwrap_or(0)
}

/// `abort_on_invalid` で何も取り込まなかったか（不合格の行があり、取込が `failed`）。
fn aborted(out: &Value, abort_on_invalid: bool) -> bool {
    abort_on_invalid
        && out.get("status").and_then(Value::as_str) == Some("failed")
        && count(out, "rows_applied") == 0
}

/// 結果に当てた行の数を足す。
fn with_matched(mut out: Value, matched: usize) -> Value {
    if let Value::Object(m) = &mut out {
        m.insert("rows_matched".into(), json!(matched));
    }
    out
}

/// 1 回目の取込の結果に、断られた行（`retried`・0 から数えた位置）だけを取り込み直した結果を重ねる。
///
/// 取り込み直して書けた行は、1 回目の誤りの数から減らす。どちらの誤り（不合格か拒否か）だったかは結果に出ないので、
/// 拒否から先に減らす（同時の追加と重なるのは、たいてい検証を通った後の適用）。また断られた行は 1 回目の数のまま
/// にする。誤りは、取り込み直した行の分を取り込み直しの誤り（元の行の番号）に替える。取込の状態・取込 ID・最後の
/// 誤りは 1 回目のもの（取り込み直しは断られた行の続き）。
fn merge(mut first: Value, second: &Value, retried: &[usize]) -> Value {
    let applied_again = count(second, "rows_applied");
    let rejected = count(&first, "rows_rejected");
    let from_rejected = applied_again.min(rejected);
    let from_invalid = (applied_again - from_rejected).min(count(&first, "rows_invalid"));
    let renumbered = second
        .get("errors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let row = usize::try_from(e.get("row")?.as_i64()?).ok()?;
            let at = *retried.get(row.checked_sub(1)?)?;
            let mut e = e.clone();
            e["row"] = json!(at + 1);
            Some(e)
        });
    let mut errors: Vec<Value> = first
        .get("errors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|e| {
            e.get("row")
                .and_then(Value::as_i64)
                .and_then(|n| usize::try_from(n).ok())
                .and_then(|n| n.checked_sub(1))
                .is_none_or(|i| !retried.contains(&i))
        })
        .cloned()
        .chain(renumbered)
        .collect();
    errors.sort_by_key(|e| e.get("row").and_then(Value::as_i64).unwrap_or(i64::MAX));
    let applied = count(&first, "rows_applied") + applied_again;
    let invalid = count(&first, "rows_invalid") - from_invalid;
    if let Value::Object(m) = &mut first {
        m.insert("rows_applied".into(), json!(applied));
        m.insert("rows_invalid".into(), json!(invalid));
        m.insert("rows_rejected".into(), json!(rejected - from_rejected));
        m.insert("errors".into(), Value::Array(errors));
    }
    first
}

#[cfg(test)]
#[path = "data_bulk_match_tests.rs"]
mod tests;
