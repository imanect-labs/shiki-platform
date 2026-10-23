//! 全面で同一の `data::DataQuery` 型が使われている（面ごとの独自フィルタ表現が無い）ことの固定
//! （Task 15.7 受け入れ条件・data-platform §11「単一の DataQuery を全面が共有する」）。
//!
//! - **コンパイル時**: 各面のクエリの受け口の型を `data::DataQuery` に束縛する関数を書く。どこかの面が
//!   独自の型へ置き換えると、このファイルがコンパイルできなくなる。生成 UI の束縛は網羅の構造体
//!   リテラル（`..` なし）で書き、条件のフィールドを足すとコンパイルできなくなるようにする。
//! - **実行時**: LLM・画面・ワークフロー定義が書く同じ JSON が、各面の受け口で同じ `DataQuery` になる。
//!
//! DB は使わない（型と直列化だけ）。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use data::DataQuery;
use serde_json::json;
use uuid::Uuid;

/// workflow: `data.query` ノードの params（`ir/params/data.rs`）。
fn workflow_node_query(p: workflow_engine::ir::params::DataQueryParams) -> Option<DataQuery> {
    p.query
}

/// shiki script（ワークフロー/skill 経路の `Shiki.data.query`）: HostBridge は `NodePorts::data_query`
/// に `&data::DataQuery` をそのまま渡す（`nodes/capability_data.rs`）。ここでポートの引数型を束縛する。
#[allow(dead_code)]
async fn script_port_query<P: workflow_engine::NodePorts + ?Sized>(
    ports: &P,
    ec: &workflow_engine::ExecCtx,
    query: &DataQuery,
) {
    let _ = ports.data_query(ec, Uuid::nil(), query).await;
}

/// generative UI: 表の束縛は（ビュー id・ピン版）だけで、条件はピンしたビューの `DataQuery`。
fn gui_binding_has_no_own_filter(b: &gui::DataViewBinding) -> (Uuid, i64) {
    // `..` を使わない網羅の分解: 束縛に条件のフィールドが増えたらここが壊れる。
    let gui::DataViewBinding {
        id: _,
        view_id,
        pinned_version,
    } = b;
    (*view_id, *pinned_version)
}

fn gui_view_query(body: data::DataViewBody) -> DataQuery {
    body.query
}

/// 画面が送れる params はページ位置と件数だけ（網羅の分解）。
fn gui_page_has_no_filter(p: data::DataViewPage) -> (Option<String>, Option<u32>) {
    let data::DataViewPage { cursor, limit } = p;
    (cursor, limit)
}

/// chat / agent-core: `data_query` ツールの入力。
fn chat_tool_query(i: chat::data_tool::DataQueryToolInput) -> Option<DataQuery> {
    i.query
}

#[test]
fn every_surface_parses_the_same_json_into_the_same_data_query() {
    let q = json!({
        "filter": { "all": [
            { "cmp": { "field": "status", "op": "eq", "value": "承認待ち" } },
            { "cmp": { "field": "amount", "op": "gte", "value": 1000 } }
        ]},
        "sort": [{ "field": "amount", "descending": true }],
        "limit": 20
    });
    let expected: DataQuery = serde_json::from_value(q.clone()).unwrap();

    let wf: workflow_engine::ir::params::DataQueryParams = serde_json::from_value(
        json!({ "table": "00000000-0000-0000-0000-000000000001", "query": q }),
    )
    .unwrap();
    assert_eq!(workflow_node_query(wf), Some(expected.clone()));

    let view: data::DataViewBody = serde_json::from_value(json!({
        "table_id": Uuid::nil(), "query": q, "display": { "kind": "grid" }
    }))
    .unwrap();
    assert_eq!(gui_view_query(view), expected);

    let tool: chat::data_tool::DataQueryToolInput =
        serde_json::from_value(json!({ "table": "経費", "query": q })).unwrap();
    assert_eq!(chat_tool_query(tool), Some(expected));

    // 生成 UI の束縛と画面の params は条件を持てない（独自のフィルタ表現が無い）。
    let binding: gui::DataViewBinding = serde_json::from_value(json!({
        "id": "data_grid:経費", "view_id": Uuid::nil(), "pinned_version": 2
    }))
    .unwrap();
    assert_eq!(gui_binding_has_no_own_filter(&binding), (Uuid::nil(), 2));
    assert!(
        serde_json::from_value::<data::DataViewPage>(json!({ "filter": q["filter"] })).is_err()
    );
    let page: data::DataViewPage = serde_json::from_value(json!({ "limit": 10 })).unwrap();
    assert_eq!(gui_page_has_no_filter(page), (None, Some(10)));
}
