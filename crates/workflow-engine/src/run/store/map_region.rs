//! map 領域の動的 fan-out・要素スコープ readiness・集約（engine.md §4.5・ir.md §5.3）。
//!
//! - **fan-out**: `control.map` 実行時に `items` 各要素へ領域ノードを `<map_step_path>[<index>].<node>` で
//!   動的挿入し、map step は `waiting_map` で待ち合わせる。
//! - **要素スコープ**: step_path の接頭辞（`m[0]` / ネスト `m[0].n[1]`）で要素を分離し、同一スコープ内で
//!   readiness/skip 伝播を評価する（要素どうしを混同しない）。
//! - **集約**: 全要素の出口 step が terminal になったら結果を `{items,errors}` へ束ね map を terminal 化する。

use std::collections::HashMap;

use serde_json::{json, Value};
use sqlx::types::Json;

use super::super::graph::RunGraph;
use super::super::model::{idempotency_key, StepStatus};
use super::super::readiness::{readiness_join, readiness_non_join, InEdge, Readiness};
use super::super::{MapFanout, OnItemError};
use super::{append_event, edge_state, map_db, RunStoreError};
use crate::ir::OnError;
use crate::vocab::{NodeType, RunEventKind};

/// map 集約の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MapOutcome {
    /// まだ全要素が終わっていない（待機継続）。
    Pending,
    /// map を terminal 化した（succeeded、または on_error=continue の failed）。後続を前進させる。
    Completed,
    /// map が fail_run で失敗した。呼び出し側が run を failed 化する。
    RunFailed,
}

/// step_path をスコープ接頭辞とノード id に分解する。
///
/// 静的ノード（`node`）→ `("", "node")`。map 要素（`m[0].node`・ネスト `m[0].n[1].leaf`）→ 接頭辞と末尾ノード。
pub(crate) fn split_step_path(step_path: &str) -> (&str, &str) {
    if !step_path.contains('[') {
        return ("", step_path);
    }
    // '[' があれば必ず '].' で node_id が続くので最後の '.' で分割する。
    match step_path.rfind('.') {
        Some(i) => (&step_path[..i], &step_path[i + 1..]),
        None => ("", step_path),
    }
}

/// 同一スコープの源ノードの step_path（`scope_prefix.node`・静的は `node`）。
pub(crate) fn scoped_path(scope_prefix: &str, node_id: &str) -> String {
    if scope_prefix.is_empty() {
        node_id.to_string()
    } else {
        format!("{scope_prefix}.{node_id}")
    }
}

/// 要素/静的スコープを考慮した readiness 判定（join 規則も同一スコープで評価）。
pub(super) fn scoped_readiness(
    scope_prefix: &str,
    node_id: &str,
    graph: &RunGraph,
    terminal_by_path: &HashMap<String, Vec<String>>,
) -> Readiness {
    let edges: Vec<InEdge> = graph
        .in_edges(node_id)
        .iter()
        .map(|(from, from_port)| {
            let src = scoped_path(scope_prefix, from);
            let terminal = terminal_by_path.contains_key(&src);
            let taken = terminal_by_path.get(&src).cloned().unwrap_or_default();
            InEdge {
                from: from.clone(),
                state: edge_state(from_port, terminal, &taken),
            }
        })
        .collect();
    match graph.node_type(node_id) {
        Some(NodeType::ControlJoin) => readiness_join(graph.join_mode(node_id), &edges),
        _ => readiness_non_join(&edges),
    }
}

/// map の meta（fan-out 時に map step の input へ格納する）。
fn map_meta(count: usize, fanout: &MapFanout, on_error: OnError, exit: &str) -> Value {
    json!({
        "map": {
            "count": count,
            "on_item_error": match fanout.on_item_error {
                OnItemError::FailMap => "fail_map",
                OnItemError::Collect => "collect",
            },
            "on_error": match on_error {
                OnError::FailRun => "fail_run",
                OnError::Continue => "continue",
            },
            "exit": exit,
        }
    })
}

/// map step を `waiting_map` にし meta を格納、領域ノードを要素ごと動的挿入する。要素数を返す。
///
/// 領域入口は `ready`・他は `pending`。各要素ノードの input に `each`（item/index）を載せる（PIT-31:
/// idempotency_key は step_path 依存で要素ごとに分離）。
pub(super) async fn insert_fanout(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: &str,
    run_id: uuid::Uuid,
    graph: &RunGraph,
    map_step_path: &str,
    on_error: OnError,
    fanout: &MapFanout,
) -> Result<usize, RunStoreError> {
    let (_, map_id) = split_step_path(map_step_path);
    let exit = graph.region_exit_node(map_id).unwrap_or("");
    let count = fanout.items.len();

    // map step → waiting_map（meta を input に格納）。
    sqlx::query(
        "UPDATE step_execution SET status = 'waiting_map', input = $4, \
         lease_owner = NULL, lease_expires_at = NULL, updated_at = now() \
         WHERE tenant_id = $1 AND run_id = $2 AND step_path = $3",
    )
    .bind(tenant_id)
    .bind(run_id)
    .bind(map_step_path)
    .bind(Json(map_meta(count, fanout, on_error, exit)))
    .execute(&mut **tx)
    .await
    .map_err(map_db)?;
    append_event(
        tx,
        tenant_id,
        run_id,
        RunEventKind::StepWaiting,
        &json!({ "step": map_step_path, "kind": "map", "count": count }),
    )
    .await?;

    let region_nodes = graph.region_nodes(map_id);
    for (index, item) in fanout.items.iter().enumerate() {
        let each = json!({ "each": { "item": item, "index": index } });
        for node in &region_nodes {
            let step_path = format!("{map_step_path}[{index}].{node}");
            let status = if graph.in_edges(node).is_empty() {
                StepStatus::Ready
            } else {
                StepStatus::Pending
            };
            let idem = idempotency_key(tenant_id, run_id, &step_path);
            sqlx::query(
                "INSERT INTO step_execution \
                 (tenant_id, run_id, step_path, node_id, status, idempotency_key, input) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT DO NOTHING",
            )
            .bind(tenant_id)
            .bind(run_id)
            .bind(&step_path)
            .bind(node)
            .bind(status.as_str())
            .bind(&idem)
            .bind(Json(&each))
            .execute(&mut **tx)
            .await
            .map_err(map_db)?;
        }
    }
    Ok(count)
}

/// 要素ステップ 1 行（集約の材料）。
struct ElemStep {
    status: StepStatus,
    output: Value,
    error: Value,
}

/// 全要素の出口が terminal なら map を集約・terminal 化する。まだなら [`MapOutcome::Pending`]。
pub(super) async fn aggregate_map(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: &str,
    run_id: uuid::Uuid,
    map_step_path: &str,
) -> Result<MapOutcome, RunStoreError> {
    // map meta を読む（fan-out 時に格納済み）。
    let meta: Option<Json<Value>> = sqlx::query_scalar(
        "SELECT input FROM step_execution \
         WHERE tenant_id = $1 AND run_id = $2 AND step_path = $3",
    )
    .bind(tenant_id)
    .bind(run_id)
    .bind(map_step_path)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_db)?
    .flatten();
    let Some(meta) = meta.map(|j| j.0) else {
        return Ok(MapOutcome::Pending);
    };
    let m = &meta["map"];
    let count = usize::try_from(m["count"].as_u64().unwrap_or(0)).unwrap_or(0);
    let exit = m["exit"].as_str().unwrap_or("");
    let collect = m["on_item_error"].as_str() == Some("collect");
    let map_continue = m["on_error"].as_str() == Some("continue");

    // 当該 map 配下の全要素ステップを読む（プレフィックス一致・LIKE 特殊文字を避け left() で厳密比較）。
    let prefix = format!("{map_step_path}[");
    type ElemRow = (String, String, Option<Json<Value>>, Option<Json<Value>>);
    let rows: Vec<ElemRow> = sqlx::query_as(
        "SELECT step_path, status, output, error FROM step_execution \
         WHERE tenant_id = $1 AND run_id = $2 AND left(step_path, length($3)) = $3",
    )
    .bind(tenant_id)
    .bind(run_id)
    .bind(&prefix)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_db)?;
    let by_path: HashMap<String, ElemStep> = rows
        .into_iter()
        .filter_map(|(p, s, o, e)| {
            StepStatus::parse(&s).map(|st| {
                (
                    p,
                    ElemStep {
                        status: st,
                        output: o.map_or(Value::Null, |j| j.0),
                        error: e.map_or(Value::Null, |j| j.0),
                    },
                )
            })
        })
        .collect();

    // 全要素の出口が terminal か確認する。
    let mut items = Vec::with_capacity(count);
    let mut errors = Vec::new();
    for i in 0..count {
        let exit_path = format!("{map_step_path}[{i}].{exit}");
        let Some(exit_step) = by_path.get(&exit_path) else {
            return Ok(MapOutcome::Pending);
        };
        if !exit_step.status.is_terminal() {
            return Ok(MapOutcome::Pending);
        }
        if exit_step.status == StepStatus::Succeeded {
            items.push(exit_step.output.clone());
        } else {
            // 要素失敗: 出口が失敗/スキップ。エラー詳細は出口 or 当該要素内の失敗ステップから拾う。
            items.push(Value::Null);
            let err = element_error(&by_path, map_step_path, i);
            errors.push(json!({ "index": i, "error": err }));
        }
    }

    let failed = !errors.is_empty();
    let output = json!({ "items": items, "errors": errors });

    if !failed || collect {
        // 全成功、または collect（失敗は errors に集約し map は成功）。
        finalize_map(
            tx,
            tenant_id,
            run_id,
            map_step_path,
            StepStatus::Succeeded,
            vec!["out".to_string()],
            output,
            None,
        )
        .await?;
        Ok(MapOutcome::Completed)
    } else if map_continue {
        // fail_map かつ失敗あり・map on_error=continue → error ポートへ。
        let err_obj = json!({
            "code": "map_item_failed",
            "message": format!("{} 要素が失敗しました", output["errors"].as_array().map_or(0, Vec::len)),
            "node_id": split_step_path(map_step_path).1,
            "attempt": 0,
            "errors": output["errors"],
        });
        let out = json!({ "error": err_obj });
        finalize_map(
            tx,
            tenant_id,
            run_id,
            map_step_path,
            StepStatus::Failed,
            vec!["error".to_string()],
            out,
            Some(err_obj),
        )
        .await?;
        Ok(MapOutcome::Completed)
    } else {
        // fail_map かつ失敗あり・map on_error=fail_run → run を失敗させる。
        let err_obj = json!({
            "code": "map_item_failed",
            "message": "map 要素が失敗しました（fail_map）",
            "node_id": split_step_path(map_step_path).1,
            "attempt": 0,
        });
        finalize_map(
            tx,
            tenant_id,
            run_id,
            map_step_path,
            StepStatus::Failed,
            Vec::new(),
            Value::Null,
            Some(err_obj),
        )
        .await?;
        Ok(MapOutcome::RunFailed)
    }
}

/// 要素 i の失敗エラーを拾う（出口の error、無ければ要素内の失敗ステップの error、最後に汎用）。
fn element_error(by_path: &HashMap<String, ElemStep>, map_step_path: &str, index: usize) -> Value {
    let elem_prefix = format!("{map_step_path}[{index}].");
    // 出口を含め要素内の失敗ステップの error を優先的に拾う。
    let mut fallback = json!({ "code": "item_failed", "message": "要素が失敗しました" });
    for (path, step) in by_path {
        if path.starts_with(&elem_prefix)
            && step.status == StepStatus::Failed
            && !step.error.is_null()
        {
            return step.error.clone();
        }
        if path.starts_with(&elem_prefix) && step.status == StepStatus::Skipped {
            fallback =
                json!({ "code": "item_skipped", "message": "要素が skip されました（上流失敗）" });
        }
    }
    fallback
}

/// map step を terminal 化し run_event を追記する。
#[allow(clippy::too_many_arguments)]
async fn finalize_map(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: &str,
    run_id: uuid::Uuid,
    map_step_path: &str,
    status: StepStatus,
    taken_ports: Vec<String>,
    output: Value,
    error: Option<Value>,
) -> Result<(), RunStoreError> {
    sqlx::query(
        "UPDATE step_execution SET status = $4, output = $5, taken_ports = $6, error = $7, \
         updated_at = now() WHERE tenant_id = $1 AND run_id = $2 AND step_path = $3",
    )
    .bind(tenant_id)
    .bind(run_id)
    .bind(map_step_path)
    .bind(status.as_str())
    .bind(Json(&output))
    .bind(&taken_ports)
    .bind(error.as_ref().map(Json))
    .execute(&mut **tx)
    .await
    .map_err(map_db)?;
    let kind = if status == StepStatus::Succeeded {
        RunEventKind::StepSucceeded
    } else {
        RunEventKind::StepFailed
    };
    append_event(
        tx,
        tenant_id,
        run_id,
        kind,
        &json!({ "step": map_step_path, "kind": "map" }),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::readiness::Readiness;
    use crate::WorkflowIr;
    use std::collections::HashMap;

    #[test]
    fn split_static_and_element_and_nested() {
        assert_eq!(split_step_path("parse"), ("", "parse"));
        assert_eq!(
            split_step_path("map_files[3].parse"),
            ("map_files[3]", "parse")
        );
        assert_eq!(split_step_path("m1[0].m2[1].leaf"), ("m1[0].m2[1]", "leaf"));
        // map ノード自身（領域内のノードだが map）: 接頭辞は外側要素。
        assert_eq!(split_step_path("m1[0].m2"), ("m1[0]", "m2"));
    }

    #[test]
    fn scoped_path_static_and_element() {
        assert_eq!(scoped_path("", "a"), "a");
        assert_eq!(scoped_path("m[2]", "a"), "m[2].a");
    }

    #[test]
    fn scoped_readiness_isolates_elements() {
        // 領域 region: in(entry) -> out(exit)。map_files が親。
        let ir = WorkflowIr::from_json(&serde_json::json!({
            "ir_version": 1, "name": "wf",
            "nodes": [
                { "id": "list", "type": "storage.list", "params": {} },
                { "id": "map_files", "type": "control.map", "params": { "items": { "$from": "nodes.list.output" } } },
                { "id": "instep", "type": "storage.read", "params": {}, "parent": "map_files" },
                { "id": "outstep", "type": "storage.write", "params": {}, "parent": "map_files" }
            ],
            "edges": [
                { "from": "list", "to": "map_files" },
                { "from": "instep", "to": "outstep" }
            ]
        }))
        .unwrap();
        let graph = RunGraph::build(&ir);
        // 要素 0 の instep が out を出したら要素 0 の outstep のみ Ready、要素 1 は影響なし。
        let mut term: HashMap<String, Vec<String>> = HashMap::new();
        term.insert("map_files[0].instep".into(), vec!["out".into()]);
        assert_eq!(
            scoped_readiness("map_files[0]", "outstep", &graph, &term),
            Readiness::Ready
        );
        assert_eq!(
            scoped_readiness("map_files[1]", "outstep", &graph, &term),
            Readiness::NotYet,
            "別要素の outstep は影響を受けない"
        );
    }
}
