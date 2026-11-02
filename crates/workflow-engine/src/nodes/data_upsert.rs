//! キーで追加か更新（`data.record.upsert`）・足す数（`data.record.update` の `add`）・まとめて登録の同じレコードを
//! 探す列（`data.bulk_upsert` の `match_on`）（GAP-16）。
//!
//! NodePorts の既存の口（`data_query`・`data_get`・`data_write` の作成・更新・一括 upsert）だけで組み、ポートの形は
//! 変えない。認可（二段の authz・行の決まり・追加だけの人）・状態の進め方・監査・SSE・数式とリンクの再計算は、どれも
//! data の 1 件の作成・更新（と一括書込）の入口が持つ。ここは「どの入口を、どの値で呼ぶか」だけを決める。
//!
//! - 無ければ追加は 1 件の作成（追加の権限）、あれば書き換えは 1 件の更新（書き換えの権限）。
//! - 探すのは実行主体に見えるレコードだけ。見えない同じ値のレコードがあると作成が一意の違反で断られる（1 件の作成と
//!   同じ応答で、明かすことは増えない・data-platform §16.1）。
//!
//! 原子性:
//! - 同じ値で並行に追加しても 2 件にならないのは、探す列の一意（`data_unique`）に頼る（保存時に、探す列が重複を
//!   許さない列であることを照らす）。作成が `unique_violation` で断られたら、先に追加された行を読み直して書き換えに回る。
//! - 書き換えと足す数は、読んだ rev を前提に書く。間にほかの書込があれば `conflict` になるので、読み直して書き直す
//!   （[`MAX_ATTEMPTS`] 回・10 秒まで）。読み・書きの間の更新を失わない。
//! - 1 回の実行で成功する書込は高々 1 回（成功したら返る。やり直すのは何も書かなかった誤りのときだけ）。step の
//!   冪等は呼び出し側の effect_journal（[`super::capability_data`]）が持つ。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use data::{BulkUpsertRow, CondOp, Condition, DataQuery};
use serde_json::{Map, Value};
use uuid::Uuid;

use super::ports::{DataWriteOp, DataWriteReq, ExecCtx, NodePorts, PortError};

/// 読み直して書き直す回数の上限。同じ行に多くの実行が同時に書いても（繰り返しの中で同じ合計に足す〔既定で同時に
/// 10 件〕・できごとが続けて届く）、たいていは上限の内に通るようにする。既定の step は試す回数が 1 回で、ここで
/// あきらめると実行が失敗するため。
pub(crate) const MAX_ATTEMPTS: u32 = 40;
/// 読み直して書き直すのに使う時間の上限（step の時間の上限〔既定 30 秒〕の内に返す）。
const RETRY_BUDGET: Duration = Duration::from_secs(10);
/// 待ちの単位（試すたびに倍にし、32 倍で止める。テストは短くする）。
#[cfg(not(test))]
const BACKOFF_UNIT_MS: u64 = 5;
#[cfg(test)]
const BACKOFF_UNIT_MS: u64 = 1;
/// まとめて登録で、同じレコードを探す 1 回の問い合わせに載せる値の数（data の 1 ページの上限と同じ）。
const MATCH_CHUNK: usize = data::MAX_QUERY_LIMIT as usize;

/// 探す列の見出し（誤りの文言で使う・エディタの欄の見出しと同じ）。
const MATCH_LABEL: &str = "同じレコードを探す列";
/// 足す数の見出し。
const ADD_LABEL: &str = "足す数";

/// 読み直して書き直しても、ほかの書込と重なり続けたとき（何も書いていない・やり直せば通りうる）。
fn exhausted() -> PortError {
    PortError::new(
        "conflict",
        "ほかの書き込みと重なり続けたため、書き込めませんでした。少し待ってから、もう一度実行してください。",
        true,
    )
}

/// 何も書かずに返った、読み直せば通りうる誤りか（間にほかの書込があった・行が消えた）。
fn changed_meanwhile(e: &PortError) -> bool {
    matches!(e.code.as_str(), "conflict" | "not_found")
}

/// 次の読み直しまで少し待つ（同じ行を書き合う相手と足並みをずらす・上限の半分から上限までの乱数）。
async fn backoff(attempt: u32) {
    let cap = BACKOFF_UNIT_MS << attempt.min(5);
    let jitter = u64::try_from(Uuid::new_v4().as_u128() % u128::from(cap)).unwrap_or(0);
    tokio::time::sleep(Duration::from_millis(cap / 2 + jitter / 2)).await;
}

/// 読み直して書き直す回数と時間（[`MAX_ATTEMPTS`]・[`RETRY_BUDGET`]）。
struct Attempts {
    started: Instant,
    next: u32,
}

impl Attempts {
    fn new() -> Attempts {
        Attempts {
            started: Instant::now(),
            next: 0,
        }
    }

    /// 次に試す回（0 から）。上限を使い切ったら `None`（1 回目はいつも試す）。
    fn next(&mut self) -> Option<u32> {
        let spent = self.next > 0 && self.started.elapsed() >= RETRY_BUDGET;
        if self.next >= MAX_ATTEMPTS || spent {
            return None;
        }
        self.next += 1;
        Some(self.next - 1)
    }
}

/// 「足す数」の値（解決した後）を、列 → 数のオブジェクトとして読む。項目なし・null は空。
pub(crate) fn parse_add(v: Option<Value>) -> Result<Map<String, Value>, PortError> {
    match v {
        None | Some(Value::Null) => Ok(Map::new()),
        Some(Value::Object(m)) => {
            if let Some((k, _)) = m.iter().find(|(_, v)| !v.is_number()) {
                return Err(PortError::invalid(format!(
                    "「{ADD_LABEL}」の「{k}」には数を入れてください。"
                )));
            }
            Ok(m)
        }
        Some(_) => Err(PortError::invalid(format!(
            "「{ADD_LABEL}」は、列ごとに数を並べてください。"
        ))),
    }
}

/// 値の欄（`patch` / `data`）をオブジェクトとして読む。項目なし・null・空の文字（エディタが置いたばかりの欄）は空
/// （足す数だけで書くとき。readiness も足す数があれば `patch` の空を止めない）。
pub(crate) fn parse_values(v: Value, label: &str) -> Result<Map<String, Value>, PortError> {
    match v {
        Value::Null => Ok(Map::new()),
        Value::String(s) if s.trim().is_empty() => Ok(Map::new()),
        Value::Object(m) => Ok(m),
        _ => Err(PortError::invalid(format!(
            "「{label}」は、列ごとに値を並べてください。"
        ))),
    }
}

/// 足す数を値の欄に重ねる前の照合（同じ列を両方に書いていない・探す列に足さない）。
fn check_disjoint(
    values: &Map<String, Value>,
    add: &Map<String, Value>,
    label: &str,
    match_on: Option<&str>,
) -> Result<(), PortError> {
    for k in add.keys() {
        if match_on == Some(k.as_str()) {
            return Err(PortError::invalid(format!(
                "{MATCH_LABEL}「{k}」には足せません。"
            )));
        }
        if values.contains_key(k) {
            return Err(PortError::invalid(format!(
                "「{k}」が「{label}」と「{ADD_LABEL}」の両方にあります。どちらか一方にしてください。"
            )));
        }
    }
    Ok(())
}

/// いまの値（`current` のレコードの `data`・無ければ 0）に足した値を `values` に入れる。
fn apply_add(
    values: &mut Map<String, Value>,
    current: Option<&Value>,
    add: &Map<String, Value>,
) -> Result<(), PortError> {
    for (k, delta) in add {
        let Some(delta) = delta.as_number() else {
            return Err(PortError::invalid(format!(
                "「{ADD_LABEL}」の「{k}」には数を入れてください。"
            )));
        };
        let now = current.and_then(|d| d.get(k)).filter(|v| !v.is_null());
        let sum = match now {
            None => Some(Value::Number(delta.clone())),
            Some(Value::Number(n)) => data::add_numbers(n, delta),
            Some(_) => {
                return Err(PortError::invalid(format!(
                    "「{k}」のいまの値が数ではないため、足せません。"
                )))
            }
        };
        let sum = sum.ok_or_else(|| {
            PortError::invalid(format!("「{k}」に足した数が大きすぎて扱えません。"))
        })?;
        values.insert(k.clone(), sum);
    }
    Ok(())
}

/// 書込の要求（ポートは作成・更新の入口を呼ぶだけ・冪等は呼び出し側）。
fn write_req(table_id: Uuid, op: DataWriteOp, key: &str) -> DataWriteReq {
    DataWriteReq {
        table_id,
        op,
        idempotency_key: key.to_string(),
    }
}

/// レコード（data の `DataRecord` の JSON）の rev と値。
fn rev_of(record: &Value) -> Result<i64, PortError> {
    record
        .get("rev")
        .and_then(Value::as_i64)
        .ok_or_else(|| PortError::unavailable("data: レコードに rev がありません"))
}

/// 書き込む人の見え方で、いまのレコードを読む（足す数の元の値と rev）。
///
/// 書き換えの入口（`data_write` の更新）に、何も変えない更新（空の patch）を渡す。data は変更の無い更新でリビジョン・
/// outbox・監査を作らず、いまのレコードを書き込む人の見え方（マスク）で返す（`DataStore::update_record`）。
/// 読取の口（`data_get`）を使わないのは、ワークフローの主体では、読む口は読む権限を委譲した人の見え方で読むため。
/// 委譲した人が読む人と書く人で別々だと、書く人に読めて読む人に読めない列を 0 から足してしまう。書き換えの権限の
/// 無い人はここで断られる（足す前に止まる）。
async fn read_as_writer(
    ports: &dyn NodePorts,
    ec: &ExecCtx,
    table_id: Uuid,
    record_id: Uuid,
    key: &str,
) -> Result<Value, PortError> {
    let op = DataWriteOp::Update {
        record_id,
        patch: Value::Object(Map::new()),
        expected_rev: None,
    };
    ports.data_write(ec, write_req(table_id, op, key)).await
}

/// `data.record.update` の足す数つきの書き換え（書き込む人の見え方で読んだ rev を前提に書き、重なったら読み直す）。
///
/// `expected_rev` を書いたときは読み直さない（その版でなければ data が断る）。
#[allow(clippy::too_many_arguments)] // ポート・実行の文脈・対象・値・足す数・版・冪等キー。
pub(crate) async fn update_adding(
    ports: &dyn NodePorts,
    ec: &ExecCtx,
    table_id: Uuid,
    record_id: Uuid,
    patch: Map<String, Value>,
    add: &Map<String, Value>,
    expected_rev: Option<i64>,
    key: &str,
) -> Result<Value, PortError> {
    check_disjoint(&patch, add, "変更する値", None)?;
    let mut attempts = Attempts::new();
    while let Some(attempt) = attempts.next() {
        let current = match read_as_writer(ports, ec, table_id, record_id, key).await {
            Ok(v) => v,
            // 読み（rev を読んでから空の更新）の間に書き換えられた。
            Err(e) if expected_rev.is_none() && e.code == "conflict" => {
                backoff(attempt).await;
                continue;
            }
            Err(e) => return Err(e),
        };
        let mut values = patch.clone();
        apply_add(&mut values, current.get("data"), add)?;
        let rev = match expected_rev {
            Some(r) => r,
            None => rev_of(&current)?,
        };
        let op = DataWriteOp::Update {
            record_id,
            patch: Value::Object(values),
            expected_rev: Some(rev),
        };
        match ports.data_write(ec, write_req(table_id, op, key)).await {
            Ok(out) => return Ok(out),
            Err(e) if expected_rev.is_none() && e.code == "conflict" => backoff(attempt).await,
            Err(e) => return Err(e),
        }
    }
    Err(exhausted())
}

/// キーで追加か更新の要求。
pub(crate) struct UpsertReq {
    pub table_id: Uuid,
    /// 同じレコードを探す列の参照名。
    pub match_on: String,
    /// 入れる値（探す列の値を含む）。
    pub data: Map<String, Value>,
    /// 足す数（列 → 数）。
    pub add: Map<String, Value>,
    pub idempotency_key: String,
}

/// 探す値（文字か数・空白だけの文字は断る）。
fn key_value(data: &Map<String, Value>, match_on: &str) -> Result<Value, PortError> {
    match data.get(match_on) {
        Some(v @ Value::Number(_)) => Ok(v.clone()),
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(Value::String(s.clone())),
        Some(Value::String(_)) => Err(PortError::invalid(format!(
            "「入れる値」の「{match_on}」が空です。{MATCH_LABEL}の値を入れてください。"
        ))),
        None | Some(Value::Null) => Err(PortError::invalid(format!(
            "「入れる値」に、{MATCH_LABEL}「{match_on}」の値がありません。"
        ))),
        Some(_) => Err(PortError::invalid(format!(
            "「入れる値」の「{match_on}」の値が、文字か数ではありません。"
        ))),
    }
}

/// 探す列の値が同じレコード（実行主体に見えるもの・2 件まで読む）。
async fn find_by_key(
    ports: &dyn NodePorts,
    ec: &ExecCtx,
    table_id: Uuid,
    match_on: &str,
    value: &Value,
) -> Result<Vec<Value>, PortError> {
    let query = DataQuery {
        filter: Some(Condition::cmp(match_on, CondOp::Eq, Some(value.clone()))),
        limit: Some(2),
        ..DataQuery::default()
    };
    let out = ports.data_query(ec, table_id, &query).await?;
    Ok(out
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// 2 件以上当たったとき（探す列が重複を許していない・どれを書き換えるか決められない）。
fn ambiguous(match_on: &str) -> PortError {
    PortError::invalid(format!(
        "「{match_on}」の値が同じレコードが 2 件以上あるため、どれを書き換えるか決められません。{MATCH_LABEL}には、重複を許さない列を選んでください。"
    ))
}

/// 書いたレコードに、追加したかを添える。
fn with_created(mut record: Value, created: bool) -> Value {
    if let Value::Object(m) = &mut record {
        m.insert("created".into(), Value::Bool(created));
    }
    record
}

/// `data.record.upsert`: 探す列の値が同じレコードがあれば書き換え、無ければ追加する。
///
/// 返すのは書いたレコードと `created`。読み直すのは、書き換える前に行が変わった・消えた（`conflict`・`not_found`）
/// ときと、追加が一意の違反で断られた（同じ値で先に追加された）ときだけ。断られた後に読み直しても見えなければ
/// （見えない行の値・ほかの重複を許さない列の重なり）、その一意の違反を返す。
pub(crate) async fn upsert(
    ports: &dyn NodePorts,
    ec: &ExecCtx,
    req: &UpsertReq,
) -> Result<Value, PortError> {
    let match_on = req.match_on.trim();
    if match_on.is_empty() {
        return Err(PortError::invalid(format!(
            "「{MATCH_LABEL}」を選んでください。"
        )));
    }
    check_disjoint(&req.data, &req.add, "入れる値", Some(match_on))?;
    let value = key_value(&req.data, match_on)?;
    let key = req.idempotency_key.as_str();
    let mut refused: Option<PortError> = None;
    // 探して当たったのに、書く人には見つからなかったレコード（2 回続いたら、書く人に見えない行として返す）。
    let mut vanished: Option<Uuid> = None;
    let mut attempts = Attempts::new();
    while let Some(attempt) = attempts.next() {
        let found = find_by_key(ports, ec, req.table_id, match_on, &value).await?;
        match found.as_slice() {
            [] => {
                if let Some(e) = refused.take() {
                    return Err(e);
                }
                let mut values = req.data.clone();
                apply_add(&mut values, None, &req.add)?;
                let op = DataWriteOp::Create {
                    data: Value::Object(values),
                };
                match ports.data_write(ec, write_req(req.table_id, op, key)).await {
                    Ok(out) => return Ok(with_created(out, true)),
                    // 同じ値で先に追加された（読み直して書き換えに回る）。
                    Err(e) if e.code == "unique_violation" => refused = Some(e),
                    Err(e) => return Err(e),
                }
            }
            [found] => {
                refused = None;
                let id = found
                    .get("id")
                    .and_then(super::resolver::as_uuid)
                    .ok_or_else(|| PortError::unavailable("data: レコードに id がありません"))?;
                // 足す数があれば、いまの値を書き込む人の見え方で読み直す（[`read_as_writer`]）。
                let current = if req.add.is_empty() {
                    found.clone()
                } else {
                    match read_as_writer(ports, ec, req.table_id, id, key).await {
                        Ok(v) => v,
                        Err(e) if changed_meanwhile(&e) => {
                            if e.code == "not_found" && vanished.replace(id) == Some(id) {
                                return Err(e);
                            }
                            backoff(attempt).await;
                            continue;
                        }
                        Err(e) => return Err(e),
                    }
                };
                let mut values = req.data.clone();
                // 探す列は同じ値なので書かない（書き換えの権限で触る列を増やさない）。
                values.remove(match_on);
                apply_add(&mut values, current.get("data"), &req.add)?;
                let op = DataWriteOp::Update {
                    record_id: id,
                    patch: Value::Object(values),
                    expected_rev: Some(rev_of(&current)?),
                };
                match ports.data_write(ec, write_req(req.table_id, op, key)).await {
                    Ok(out) => return Ok(with_created(out, false)),
                    Err(e) if changed_meanwhile(&e) => {
                        if e.code == "not_found" && vanished.replace(id) == Some(id) {
                            return Err(e);
                        }
                        backoff(attempt).await;
                    }
                    Err(e) => return Err(e),
                }
            }
            _ => return Err(ambiguous(match_on)),
        }
    }
    Err(refused.unwrap_or_else(exhausted))
}

/// 突き合わせる値の形（文字はそのまま・数は 10 進で同じなら同じ）。文字か数でなければ `None`。
fn match_key(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(format!("s:{s}")),
        Value::Number(n) => data::number_key(n).map(|k| format!("n:{k}")),
        _ => None,
    }
}

/// まとめて登録の `id` の無い行を、探す列の値が同じレコードに当てる（当たった行は更新・残りは追加）。返すのは当てた
/// 行の数。
///
/// 行の `data` に探す列の値（文字か数）が要り、同じ値の行が 2 つあれば何も登録しない（どちらで書き換えるか決められ
/// ない）。探すのは実行主体に見えるレコードだけ（見えない同じ値は追加になり、data が一意の違反でその行を断る）。
/// 当てた行の `rev` は書いたときだけ照らす（書いていなければ行ロックの下の後勝ち・`id` を書いた行と同じ）。
pub(crate) async fn match_bulk_rows(
    ports: &dyn NodePorts,
    ec: &ExecCtx,
    table_id: Uuid,
    match_on: &str,
    rows: &mut [BulkUpsertRow],
) -> Result<usize, PortError> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut pending: Vec<(usize, Value)> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        if row.record_id.is_some() {
            continue;
        }
        let n = i + 1;
        let v = row.data.get(match_on).cloned().unwrap_or(Value::Null);
        let Some(k) = match_key(&v) else {
            return Err(PortError::invalid(if v.is_null() || v == "" {
                format!("{n} 件目に、{MATCH_LABEL}「{match_on}」の値がありません。")
            } else {
                format!("{n} 件目の「{match_on}」の値が、文字か数ではありません。")
            }));
        };
        if let Some(first) = seen.insert(k, n) {
            return Err(PortError::invalid(format!(
                "{first} 件目と {n} 件目の「{match_on}」の値が同じです。1 件にまとめてください。"
            )));
        }
        pending.push((i, v));
    }
    let mut matched = 0;
    for chunk in pending.chunks(MATCH_CHUNK) {
        let values: Vec<Value> = chunk.iter().map(|(_, v)| v.clone()).collect();
        let query = DataQuery {
            filter: Some(Condition::cmp(
                match_on,
                CondOp::In,
                Some(Value::Array(values)),
            )),
            limit: u32::try_from(MATCH_CHUNK).ok(),
            select: Some(vec![match_on.to_string()]),
            ..DataQuery::default()
        };
        let out = ports.data_query(ec, table_id, &query).await?;
        let mut by_key: HashMap<String, Uuid> = HashMap::new();
        for item in out
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let stored = item.get("data").and_then(|d| d.get(match_on));
            let id = item.get("id").and_then(super::resolver::as_uuid);
            if let (Some(k), Some(id)) = (stored.and_then(match_key), id) {
                if by_key.insert(k, id).is_some() {
                    return Err(ambiguous(match_on));
                }
            }
        }
        for (i, v) in chunk {
            if let Some(id) = match_key(v).and_then(|k| by_key.get(&k)) {
                rows[*i].record_id = Some(*id);
                matched += 1;
            }
        }
    }
    Ok(matched)
}

#[cfg(test)]
#[path = "data_upsert_tests.rs"]
mod tests;
