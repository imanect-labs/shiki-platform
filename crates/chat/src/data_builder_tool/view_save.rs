//! `data_view_save`: 保存ビュー（テーブルへの絞り込み・並べ替え・表示の設定）を作る・直す（確認なし・D-10 の T06）。
//!
//! - 保存は data のチョークポイント（[`data::DataViewStore`]）を話した本人の権限で通す。保存の前にテーブルの閲覧・
//!   クエリと表示の整合を data が確かめる。ビューは版のある artifact（直すと新しい版を積む・持ち主は作った本人）。
//!   実行はいつも閲覧する人本人の権限で行うので、ビューを保存しても誰にも行や列を見せない（`view.rs`）。
//! - 確認（承認カード）は出さない（HD-1・ビューの作成と改訂は版で戻せ、アプリは版を固定する）。
//! - **作る**: `view` を省く。名前が重なったら、本人がこの会話の run の間に作った同名と本文を比べる（作成の冪等・
//!   R1・[`crate::builder_obs::on_name_taken`]）。1 回の run で作れるのは [`MAX_VIEWS_PER_RUN`] 件まで。
//! - **直す**: `view` に id か名前、`expected_version` に読んだ版を渡す（版が違えば保存しない）。
//! - クエリは data_query と同じ [`DataQuery`]（最上位の未知のキーは断る）。

use std::sync::Arc;

use agent_core::{Tool, ToolError, ToolName, ToolOutcome};
use artifact::{ArtifactKind, ArtifactStore};
use async_trait::async_trait;
use authz::AuthContext;
use data::{DataError, DataQuery, DataStore, DataTable, DataViewBody, DataViewStore, ViewSpec};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{failure, resolve_table, shown, BuilderRun};
use crate::builder_obs::{
    already_created, created_differently, on_name_taken, BuilderKind, BuilderRef, NameTaken,
};
use crate::tool_args::{accept_object_args, ObjectArg, DATA_QUERY};

/// 1 回の会話の run で作れる保存ビューの数。
pub const MAX_VIEWS_PER_RUN: i64 = 10;

/// 表示の設定（`display`）。
const DISPLAY_ARG: ObjectArg = ObjectArg {
    key: "display",
    shape: "kind（grid・kanban・calendar・gallery）と種類ごとの設定を持つオブジェクト",
    sql_arg: None,
};

/// 保存ビューを作る・直す道具が通るチョークポイント。
#[derive(Clone)]
pub struct ViewBuilderDeps {
    pub data: DataStore,
    pub views: Arc<DataViewStore>,
    /// 作成の冪等の照合と、作った数を数える（artifact の単一チョークポイント）。
    pub artifacts: Arc<ArtifactStore>,
}

/// 保存ビューを作る・直す道具。
pub struct DataViewSaveTool {
    deps: ViewBuilderDeps,
    run: BuilderRun,
}

impl DataViewSaveTool {
    #[must_use]
    pub fn new(deps: ViewBuilderDeps, run: BuilderRun) -> Self {
        DataViewSaveTool { deps, run }
    }
}

const DESCRIPTION: &str = "テーブルの保存ビュー（絞り込み・並べ替え・表示の設定に名前を付けたもの）を作る・直す。\
アプリの画面の一覧や、よく見る絞り込みに使う。確認なしで保存し、作ったビューの持ち主はユーザー本人（共有はしない）。\
ビューを開いた人には、その人が見られる行と列だけが出る（ビューで権限は広がらない）。\n\
\n\
入力:\n\
- name: ビューの名前（作るとき必須・組織の中で重ならない名前）\n\
- table: テーブルの id か名前\n\
- query（省略可）: data_query と同じ形 {filter?, sort?, select?, group_by?, aggregate?, limit?}。省略は全部の行\n\
- display（省略可）: 表示の設定。省略は表。{\"kind\":\"grid\",\"columns\":[{\"field\":\"<列>\",\"hidden\":false}]} / \
{\"kind\":\"kanban\",\"group_field\":\"<選択肢の列>\",\"card_fields\":[…]} / {\"kind\":\"calendar\",\"date_field\":\"<日付の列>\",\"title_field\":\"<列>\"} / \
{\"kind\":\"gallery\",\"title_field\":\"<列>\",\"card_fields\":[…]}\n\
- view（直すとき）: 直すビューの id か名前。expected_version（直すとき必須）: 読んだときの版（参照の行の version）\n\
\n\
- 1 回の応答で作れるビューは 10 件まで。直すのは何度でもよい。\n\
- 成功したら末尾の「参照:」の行の id と version を、ミニアプリの部品や次の直しに渡す。";

/// 入力（`query` と `display` は保存の前に data の型で厳密に読む）。
struct ViewInput {
    name: Option<String>,
    table: String,
    query: DataQuery,
    display: ViewSpec,
    view: Option<String>,
    expected_version: Option<i64>,
}

/// 誤りは観測（大きいので箱に入れる）。
fn parse(mut input: Value) -> Result<ViewInput, Box<ToolOutcome>> {
    accept_object_args(&mut input, &[DATA_QUERY, DISPLAY_ARG])
        .map_err(|e| Box::new(ToolOutcome::error(e)))?;
    let Some(obj) = input.as_object() else {
        return Err(Box::new(ToolOutcome::error(
            "入力はオブジェクトにしてください。",
        )));
    };
    const KEYS: [&str; 6] = [
        "name",
        "table",
        "query",
        "display",
        "view",
        "expected_version",
    ];
    if let Some(k) = obj.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(Box::new(ToolOutcome::error(format!(
            "「{k}」は受けない入力です。使えるのは {} です。",
            KEYS.join("・")
        ))));
    }
    let text = |k: &str| {
        obj.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let Some(table) = text("table") else {
        return Err(Box::new(ToolOutcome::error(
            "table（テーブルの id か名前）を書いてください。",
        )));
    };
    let query = match obj.get("query").filter(|q| !q.is_null()) {
        Some(q) => DataQuery::from_request(q.clone())
            .map_err(|e| Box::new(crate::data_tool::denied(&e)))?,
        None => DataQuery::default(),
    };
    let display = match obj.get("display").filter(|d| !d.is_null()) {
        Some(d) => serde_json::from_value(d.clone()).map_err(|e| {
            Box::new(ToolOutcome::error(format!(
                "display を読めません: {e}。説明の display の形に合わせてください。"
            )))
        })?,
        None => ViewSpec::default(),
    };
    Ok(ViewInput {
        name: text("name"),
        table,
        query,
        display,
        view: text("view"),
        expected_version: obj.get("expected_version").and_then(Value::as_i64),
    })
}

/// 保存したビューの観測（参照の行つき）。
fn saved(id: Uuid, name: &str, version: i64, table: &DataTable, created: bool) -> ToolOutcome {
    let r = BuilderRef::new(BuilderKind::DataView, id, name, version);
    let mut outcome = ToolOutcome::ok(format!(
        "保存ビュー「{}」を{}（id: {id}, v{version}・テーブル「{}」）。開いた人には、その人が見られる行と列だけが出ます。\n{}",
        shown(name),
        if created { "作りました" } else { "直しました" },
        shown(&table.name),
        r.line()
    ));
    outcome.builder_refs.push(r.to_value());
    outcome
}

#[async_trait]
impl Tool for DataViewSaveTool {
    fn name(&self) -> &str {
        ToolName::DataViewSave.as_str()
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "ビューの名前（作るとき必須）" },
                "table": { "type": "string", "description": "テーブルの id か名前" },
                "query": {
                    "type": "object",
                    "description": "DataQuery（filter / sort / select / group_by / aggregate / limit）",
                    "properties": {
                        "filter": { "type": "object", "description": "条件木（cmp / all / any / not）" },
                        "sort": { "type": "array", "items": { "type": "object" }, "description": "[{field, descending}]（最大 4 キー）" },
                        "select": { "type": "array", "items": { "type": "string" } },
                        "group_by": { "type": "array", "items": { "type": "string" } },
                        "aggregate": { "type": "array", "items": { "type": "object" } },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 200 }
                    }
                },
                "display": { "type": "object", "description": "表示の設定（説明を参照・省略は表）" },
                "view": { "type": "string", "description": "直すビューの id か名前（作るときは省く）" },
                "expected_version": { "type": "integer", "description": "直すとき必須。読んだときの版" }
            },
            "required": ["table"]
        })
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let input = match parse(input) {
            Ok(i) => i,
            Err(o) => return Ok(*o),
        };
        let table = match resolve_table(&self.deps.data, ctx, &input.table, trace_id).await {
            Ok(t) => t,
            Err(o) => return Ok(o),
        };
        let body = DataViewBody {
            table_id: table.id,
            query: input.query,
            display: input.display,
        };
        if let Some(view) = input.view {
            return Ok(self
                .update(ctx, &view, input.expected_version, &body, &table, trace_id)
                .await);
        }
        let Some(name) = input.name else {
            return Ok(ToolOutcome::error(
                "name（ビューの名前）を書いてください。既存のビューを直すなら view に id か名前を書きます。",
            ));
        };
        Ok(self.create(ctx, &name, &body, &table, trace_id).await)
    }
}

impl DataViewSaveTool {
    async fn create(
        &self,
        ctx: &AuthContext,
        name: &str,
        body: &DataViewBody,
        table: &DataTable,
        trace_id: Option<&str>,
    ) -> ToolOutcome {
        match self
            .deps
            .artifacts
            .count_own_created_since(ctx, ArtifactKind::DataView, self.run.since)
            .await
        {
            Ok(n) if n >= MAX_VIEWS_PER_RUN => {
                // 上限でも、作成済みの同名の再試行は作成済みとして返す（再試行で上限の誤りにしない）。
                return match self.name_taken(ctx, name, body, trace_id).await {
                    Some(o) => o,
                    None => ToolOutcome::error(format!(
                        "この応答ではもう {MAX_VIEWS_PER_RUN} 件のビューを作りました。これ以上は作りません。\
                         作ったビューを直すなら view と expected_version を渡してください。"
                    )),
                };
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, "作ったビューの数を数えられない");
                return ToolOutcome::error("保存ビューを保存できませんでした。");
            }
        }
        match self.deps.views.create(ctx, name, body, trace_id).await {
            Ok(id) => saved(id, name.trim(), 1, table, true),
            // 作るときの競合は名前の重なりだけ（本人がこの run で作った同名かを確かめる・R1）。
            Err(DataError::Conflict(_)) => match self.name_taken(ctx, name, body, trace_id).await {
                Some(o) => o,
                None => ToolOutcome::error(format!(
                    "名前「{}」のビューは作れません（同じ名前のものがあります）。別の名前にしてください。",
                    shown(name)
                )),
            },
            Err(e) => failure(&e),
        }
    }

    /// 本人がこの会話の run の間に同じ名前で作っていたときの観測（同じ本文なら作成済み・違えば直し方）。
    /// 当てはまらなければ `None`。
    async fn name_taken(
        &self,
        ctx: &AuthContext,
        name: &str,
        body: &DataViewBody,
        trace_id: Option<&str>,
    ) -> Option<ToolOutcome> {
        let raw = serde_json::to_value(body).ok()?;
        match on_name_taken(
            &self.deps.artifacts,
            ctx,
            BuilderKind::DataView,
            name,
            self.run.since,
            &raw,
            trace_id,
        )
        .await
        {
            Ok(NameTaken::Same(found)) => Some(already_created(&found)),
            Ok(NameTaken::Differs(found)) => Some(created_differently(
                &found,
                &format!(
                    "直すなら、view に {} を、expected_version に {} を付けて保存し直してください。別のビューとして\
                     作るなら、name を別の名前にしてください。",
                    found.id, found.version
                ),
            )),
            Ok(NameTaken::Other) => None,
            Err(e) => {
                tracing::warn!(error = %e, "作成済みのビューの照合に失敗（作れなかった旨を返す）");
                None
            }
        }
    }

    async fn update(
        &self,
        ctx: &AuthContext,
        view: &str,
        expected_version: Option<i64>,
        body: &DataViewBody,
        table: &DataTable,
        trace_id: Option<&str>,
    ) -> ToolOutcome {
        let Some(expected) = expected_version else {
            return ToolOutcome::error(
                "直すときは expected_version（読んだときの版・参照の行の version）を渡してください。",
            );
        };
        let found = match Uuid::parse_str(view) {
            Ok(id) => self.deps.artifacts.get(ctx, id, trace_id).await,
            Err(_) => {
                self.deps
                    .artifacts
                    .get_by_name(ctx, ArtifactKind::DataView, view, trace_id)
                    .await
            }
        };
        let found = match found {
            Ok(a) if a.kind == ArtifactKind::DataView => a,
            // 見えない・無い・種類が違うは同じ文言（存在を明かさない）。
            Ok(_) | Err(_) => {
                return ToolOutcome::error(
                    "直す保存ビューが見つからないか、編集の権限がありません。",
                );
            }
        };
        match self
            .deps
            .views
            .update(ctx, found.id, body, Some(expected), trace_id)
            .await
        {
            Ok(version) => saved(found.id, &found.name, version, table, false),
            Err(DataError::Conflict(_)) => ToolOutcome::error(format!(
                "保存ビュー「{}」は v{} になっています（渡した版は v{expected}）。ほかの変更が入っています。今の版を\
                 確かめてから、expected_version に {} を渡して直してください。",
                shown(&found.name),
                found.current_version,
                found.current_version
            )),
            Err(DataError::NotFound | DataError::Forbidden) => ToolOutcome::error(
                "直す保存ビューが見つからないか、編集の権限がありません。",
            ),
            Err(e) => failure(&e),
        }
    }
}
