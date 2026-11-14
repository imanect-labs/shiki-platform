//! まとめて登録の同じレコードを探す列（`data.bulk_upsert` の `match_on`・GAP-16）の取込: 照合して取り込み、
//! 照合の後に同じ値で先に追加された行を、そのレコードの更新として取り込み直す。
//!
//! 照合（[`super::data_upsert::match_bulk_rows`]）で当たらなかった行は追加になる。同じ値を同時に取り込む別の実行が
//! 照合と取込の間に追加すると、こちらの追加は探す列の一意（`data_unique`）で断られる（検証で不合格〔`rows_invalid`〕か、
//! 適用で拒否〔`rows_rejected`〕）。そのままだと値が書かれないのに step は成功で終わる。`data.record.upsert` が
//! 一意の違反の後に読み直して書き換えに回るのと同じく、断られた行を探し直し、いまは見えるレコードに当たった
//! 行だけを更新として取り込み直す（1 回だけ）。
//!
//! - 断られた行は、ポートが返す断られた行の全部（[`BULK_FAILED_ROWS`]・行番号・断られた段・書き換えようとした
//!   レコード）から選ぶ（結果の `errors` は先頭の一部だけなので、それだけで選ぶと 101 行目からが取り込み直されない）。
//!   ポートが返さなければ（テストの fake）`errors` から選ぶ。誤りの文は読まない。
//! - 取り込み直すのは、`id` を書いていない断られた行のうち、探し直していま見えるレコードに当たり、そのレコードが
//!   1 回目に書き換えようとしたものと違う行だけ（追加の行と、書き換えるはずのレコードが消えて同じ値の別の
//!   レコードができた行）。同じレコードの更新が断られた行（行の決まり・ほかの列の誤り）は、取り込み直しても同じ理由で
//!   断られるので取り込み直さない。当たらない行（見えない行の値・ほかの列の誤り）は誤りのまま残す。どれも 1 回目の
//!   取込の記録から選ぶので、step をやり直したとき（照合の結果が 1 回目と違う）も同じ行を選ぶ。
//! - `abort_on_invalid` で何も取り込まなかった（不合格の行があり、取込が `failed`）ときは、断られた行のどれかが
//!   探し直して当たったら、探し直した行で全体を取り込み直す（何も書いていないので、全体でやり直せる）。1 回目の取込が
//!   書いた後の取り込み直しは、断られた行の続きなので、何も取り込まない設定にしない（1 行の不合格で、ほかの取り込み
//!   直す行を捨てない）。
//! - 取り込み直しの取込 ID は、冪等キーに印と「取り込み直す行と当てたレコード」の要約を足したものから決まる。step を
//!   やり直しても、探し直した結果が同じなら同じ取込を返し（二重に取り込まない）、違えば（間に見え方が変わった）いまの
//!   行で取り込み直す（前の回が書いた行には同じ値をもう一度書く）。結果はいつも、返した取込が実際にしたことと合う。
//! - 1 回目の取込はもう書いているので、探し直し・取り込み直しの誤りは、やり直して直るもの（step をやり直す。取込は
//!   冪等）だけを返し、ほかは 1 回目の結果のまま終える（断られた行は誤りのまま返る）。
//! - 結果は 1 回の取込と同じ形で返す。件数は 2 回の取込を合わせ（取り込み直した行は 1 回目に断られた段の数から外す）、
//!   取り込み直しで決着した行の前の誤りは消し、取り込み直しの誤りは元の行の番号で返す。取り込み直しが途中で失敗した
//!   （書く権限を外された など）ときは、状態と最後の誤りを取り込み直しのものにし、書けたかが分からない行の前の誤りは
//!   残す。当てた行の数（`rows_matched`）には探し直して当てた行も数える。

use std::collections::HashSet;

use data::BulkUpsertRow;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::data_upsert::match_bulk_rows;
use super::ports::{DataWriteOp, DataWriteReq, ExecCtx, NodePorts, PortError, BULK_FAILED_ROWS};
use super::resolver::as_uuid;

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

/// 取込で断られた行。
#[derive(Debug, Clone, Copy)]
struct Failed {
    /// 取込に渡した行の中の位置（0 から）。
    at: usize,
    /// 適用の段で拒否されたか（偽なら検証の段で不合格。`None` はポートが返さず分からない）。
    rejected: Option<bool>,
    /// 更新の行が書き換えようとしたレコード（作成の行と、分からないときは `None`）。
    update_of: Option<Uuid>,
}

/// 1 回の取込の結果（step の出力の形）と、断られた行。
struct Imported {
    out: Value,
    failed: Vec<Failed>,
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
    let matched = matched_count(&rows, &explicit);
    let first = import(
        ports,
        ec,
        table_id,
        &rows,
        abort_on_invalid,
        idempotency_key,
    )
    .await?;
    let candidates: Vec<Failed> = first
        .failed
        .iter()
        .copied()
        .filter(|f| !explicit[f.at])
        .collect();
    if candidates.is_empty() {
        return Ok(with_matched(first.out, matched));
    }
    // 探し直す（照合の結果は捨て、いま見えるレコードに当て直す）。
    let mut again: Vec<BulkUpsertRow> = candidates
        .iter()
        .map(|f| BulkUpsertRow {
            record_id: None,
            ..rows[f.at].clone()
        })
        .collect();
    if let Err(e) = match_bulk_rows(ports, ec, table_id, match_on, &mut again).await {
        return settle(e, first.out, matched);
    }
    // いまのレコードに当たり、1 回目に書き換えようとしたものと違う行だけ。
    let changed: Vec<(Failed, Uuid)> = candidates
        .iter()
        .zip(&again)
        .filter_map(|(f, r)| {
            let id = r.record_id?;
            (f.update_of != Some(id)).then_some((*f, id))
        })
        .collect();
    if changed.is_empty() {
        return Ok(with_matched(first.out, matched));
    }
    let retry_key = rematch_key(idempotency_key, &changed);
    if aborted(&first.out, abort_on_invalid) {
        // 何も取り込んでいない: 探し直した行で全体を取り込み直す（当たらない行は追加のまま）。
        let mut whole = rows.clone();
        for (f, id) in &changed {
            whole[f.at].record_id = Some(*id);
        }
        return match import(ports, ec, table_id, &whole, abort_on_invalid, &retry_key).await {
            Ok(second) => Ok(with_matched(second.out, matched_count(&whole, &explicit))),
            Err(e) => settle(e, first.out, matched),
        };
    }
    // 当たった行だけを、そのレコードの更新として取り込み直す（断られた行の続き・何も取り込まない設定にしない）。
    let retried: Vec<Failed> = changed.iter().map(|(f, _)| *f).collect();
    let retry_rows: Vec<BulkUpsertRow> = changed
        .iter()
        .map(|(f, id)| BulkUpsertRow {
            record_id: Some(*id),
            ..rows[f.at].clone()
        })
        .collect();
    let second = match import(ports, ec, table_id, &retry_rows, false, &retry_key).await {
        Ok(second) => second,
        Err(e) => return settle(e, first.out, matched),
    };
    let rematched = retried
        .iter()
        .filter(|f| rows[f.at].record_id.is_none())
        .count();
    Ok(with_matched(
        merge(first.out, &second, &retried),
        matched + rematched,
    ))
}

/// 1 回目の取込の後の誤り: やり直して直るものは返し（step をやり直す・取込は冪等）、ほかは 1 回目の結果で終える。
fn settle(e: PortError, first: Value, matched: usize) -> Result<Value, PortError> {
    if e.retryable {
        Err(e)
    } else {
        Ok(with_matched(first, matched))
    }
}

/// 探して当てた行の数（id を書いた行を除く・最後に取り込んだときの当て方）。
fn matched_count(rows: &[BulkUpsertRow], explicit: &[bool]) -> usize {
    rows.iter()
        .zip(explicit)
        .filter(|(r, e)| !**e && r.record_id.is_some())
        .count()
}

/// 取り込み直しの冪等キー（取り込み直す行の位置と当てたレコードの要約つき・やり直しで同じなら同じ取込）。
fn rematch_key(key: &str, changed: &[(Failed, Uuid)]) -> String {
    let mut h = Sha256::new();
    for (f, id) in changed {
        h.update(format!("{}:{id};", f.at).as_bytes());
    }
    let digest = format!("{:x}", h.finalize());
    format!("{key}{REMATCH_MARK}:{}", &digest[..16])
}

/// 1 回の取込（ポートの一括書込）。断られた行を結果から取り出す（ポートの中だけの項目は外す）。
async fn import(
    ports: &dyn NodePorts,
    ec: &ExecCtx,
    table_id: Uuid,
    rows: &[BulkUpsertRow],
    abort_on_invalid: bool,
    key: &str,
) -> Result<Imported, PortError> {
    let req = DataWriteReq {
        table_id,
        op: DataWriteOp::BulkUpsert {
            rows: rows.to_vec(),
            abort_on_invalid,
        },
        idempotency_key: key.to_string(),
    };
    let mut out = ports.data_write(ec, req).await?;
    let failed = failed_rows(&mut out, rows.len());
    Ok(Imported { out, failed })
}

/// 断られた行（[`BULK_FAILED_ROWS`] を外して読む。無ければ `errors` に出た行だけ・段とレコードは分からない）。
fn failed_rows(out: &mut Value, len: usize) -> Vec<Failed> {
    let at = |e: &Value| {
        let row = usize::try_from(e.get("row")?.as_i64()?).ok()?;
        (1..=len).contains(&row).then(|| row - 1)
    };
    let reported = out.as_object_mut().and_then(|m| m.remove(BULK_FAILED_ROWS));
    if let Some(Value::Array(items)) = reported {
        return items
            .iter()
            .filter_map(|e| {
                Some(Failed {
                    at: at(e)?,
                    rejected: e.get("rejected").and_then(Value::as_bool),
                    update_of: e.get("update_of").and_then(as_uuid),
                })
            })
            .collect();
    }
    out.get("errors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|e| {
            Some(Failed {
                at: at(e)?,
                rejected: None,
                update_of: None,
            })
        })
        .collect()
}

/// 取込の数（無ければ 0）。
fn count(out: &Value, key: &str) -> i64 {
    out.get(key).and_then(Value::as_i64).unwrap_or(0)
}

/// 取込の状態。
fn status(out: &Value) -> Option<&str> {
    out.get("status").and_then(Value::as_str)
}

/// `abort_on_invalid` で何も取り込まなかったか（不合格の行があり、取込が `failed`）。
fn aborted(out: &Value, abort_on_invalid: bool) -> bool {
    abort_on_invalid && status(out) == Some("failed") && count(out, "rows_applied") == 0
}

/// 結果に当てた行の数を足す。
fn with_matched(mut out: Value, matched: usize) -> Value {
    if let Value::Object(m) = &mut out {
        m.insert("rows_matched".into(), json!(matched));
    }
    out
}

/// 誤りの文の行番号（1 から）。
fn error_row(e: &Value) -> Option<usize> {
    usize::try_from(e.get("row")?.as_i64()?).ok()
}

/// 1 回目の取込の結果に、断られた行（`retried`）だけを取り込み直した結果を重ねる。
///
/// - 取り込み直しで決着した行（断られ直した行と、取り込み直しが終わっていれば書けた残りの行）は、1 回目に断られた
///   段の数から外す（段が分からなければ拒否から先に。同時の追加と重なるのは、たいてい検証を通った後の適用）。
///   取り込み直しが途中で失敗したときは、断られ直していない行のうち書けた数だけを外す（どの行かは分からないので、
///   拒否の段の行から先に）。
/// - 誤りの文は、決着した行の 1 回目の文を外し、取り込み直しの文（元の行の番号）を足す。書けたかが分からない行の
///   文は残す。
/// - 状態と最後の誤りは 1 回目のもの。ただし 1 回目が終わり、取り込み直しが終わらなかった（途中で失敗した）ときは、
///   取り込み直しのもの（書けていない行があることを状態で知らせる）。
fn merge(mut first: Value, second: &Imported, retried: &[Failed]) -> Value {
    let done = status(&second.out) == Some("done");
    // 取り込み直しで断られ直した行（元の行の位置）。
    let failed_again: HashSet<usize> = second
        .failed
        .iter()
        .filter_map(|f| Some(retried.get(f.at)?.at))
        .collect();
    let renumbered: Vec<Value> = second
        .out
        .get("errors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let at = retried.get(error_row(e)?.checked_sub(1)?)?.at;
            let mut e = e.clone();
            e["row"] = json!(at + 1);
            Some(e)
        })
        .collect();
    // 1 回目の誤りの数から外す行の段。
    let (again_rows, rest): (Vec<&Failed>, Vec<&Failed>) =
        retried.iter().partition(|f| failed_again.contains(&f.at));
    let mut out_of_first: Vec<Option<bool>> = again_rows.iter().map(|f| f.rejected).collect();
    if done {
        out_of_first.extend(rest.iter().map(|f| f.rejected));
    } else {
        let applied = usize::try_from(count(&second.out, "rows_applied")).unwrap_or(0);
        let mut classes: Vec<Option<bool>> = rest.iter().map(|f| f.rejected).collect();
        classes.sort_by_key(|c| match c {
            Some(true) => 0,
            None => 1,
            Some(false) => 2,
        });
        out_of_first.extend(classes.into_iter().take(applied));
    }
    let mut invalid = count(&first, "rows_invalid");
    let mut rejected = count(&first, "rows_rejected");
    let mut unknown = 0;
    for c in out_of_first {
        match c {
            Some(true) => rejected -= 1,
            Some(false) => invalid -= 1,
            None => unknown += 1,
        }
    }
    let from_rejected = unknown.min(rejected.max(0));
    rejected -= from_rejected;
    invalid -= unknown - from_rejected;
    let invalid = invalid.max(0) + count(&second.out, "rows_invalid");
    let rejected = rejected.max(0) + count(&second.out, "rows_rejected");
    let applied = count(&first, "rows_applied") + count(&second.out, "rows_applied");
    // 誤りの文: 決着した行（取り込み直しの文がある行・終わった取り込み直しで書けた行）の 1 回目の文を外す。
    let replaced: HashSet<usize> = renumbered.iter().filter_map(error_row).collect();
    let settled: HashSet<usize> = retried
        .iter()
        .filter(|f| replaced.contains(&(f.at + 1)) || (done && !failed_again.contains(&f.at)))
        .map(|f| f.at)
        .collect();
    let mut errors: Vec<Value> = first
        .get("errors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|e| {
            error_row(e)
                .and_then(|n| n.checked_sub(1))
                .is_none_or(|i| !settled.contains(&i))
        })
        .cloned()
        .chain(renumbered)
        .collect();
    errors.sort_by_key(|e| error_row(e).unwrap_or(usize::MAX));
    let first_done = status(&first) == Some("done");
    if let Value::Object(m) = &mut first {
        m.insert("rows_applied".into(), json!(applied));
        m.insert("rows_invalid".into(), json!(invalid));
        m.insert("rows_rejected".into(), json!(rejected));
        m.insert("errors".into(), Value::Array(errors));
        if first_done && !done {
            for k in ["status", "last_error"] {
                m.insert(k.into(), second.out.get(k).cloned().unwrap_or(Value::Null));
            }
        }
    }
    first
}

#[cfg(test)]
#[path = "data_bulk_match_tests.rs"]
mod tests;
