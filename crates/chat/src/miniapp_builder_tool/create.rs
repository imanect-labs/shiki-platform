//! `miniapp_create`: 画面と、束ねるワークフロー・保存ビューからミニアプリを作る（確認なし・下書き・D-10 の T08）。
//!
//! - 部品は名前か id で受け、版を省いたら本人に見える最新の版に固定する。
//! - 保存は [`gui::MiniAppStore::create`] を [`gui::SaveMode::Agent`] で通す（構造・ピンの viewer と版・画面の書き損じ・
//!   部品とワークフローの入力の食い違いを、全件の誤りとして返す）。作ったアプリの持ち主は本人で、共有はしない。
//! - **作成の冪等**（R1）: 名前が重なったら、本人がこの会話の run の間に作った同名と本文を比べる。
//! - **上限**: 1 回の会話の run で [`MAX_APPS_PER_RUN`] 件まで。

use agent_core::{Tool, ToolError, ToolName, ToolOutcome};
use artifact::{ArtifactError, ArtifactKind, ConflictReason};
use async_trait::async_trait;
use authz::AuthContext;
use gui::{GuiError, SaveMode};
use serde_json::{json, Value};

use super::pins::{read_pins, resolve_pins, DefaultVersion, DATA_VIEWS, WORKFLOWS};
use super::{internal, save_failure, saved, MiniappBuilderDeps, MAX_APPS_PER_RUN};
use crate::builder_obs::{
    already_created, created_differently, on_name_taken, BuilderKind, NameTaken,
};
use crate::data_builder_tool::BuilderRun;

/// ミニアプリを作る道具。
pub struct MiniappCreateTool {
    deps: MiniappBuilderDeps,
    run: BuilderRun,
    description: String,
}

impl MiniappCreateTool {
    #[must_use]
    pub fn new(deps: MiniappBuilderDeps, run: BuilderRun) -> Self {
        MiniappCreateTool {
            deps,
            run,
            description: description(),
        }
    }
}

#[cfg(test)]
pub(super) fn description_for_test() -> String {
    description()
}

fn description() -> String {
    format!(
        "ミニアプリ（入力フォームとボタン・一覧の画面を持つ業務アプリ）を新しく作る。確認なしで下書きとして保存し、\
         作ったアプリの持ち主はユーザー本人（共有はしない）。ボタンはワークフローを、表は保存ビューを使うので、\
         先にワークフロー（emit_workflow）と保存ビュー（data_view_save）を作っておく。\n\
         \n\
         入力:\n\
         - name: アプリの名前（組織の中で重ならない名前）\n\
         - description: アプリの説明（1〜2 文）\n\
         - screen: 画面の Markdown（下の書き方）\n\
         - workflows: 束ねるワークフロー [{{alias, workflow, version?}}]。workflow は id か名前、version を省くと最新の版\n\
         - data_views（省略可）: 束ねる保存ビュー [{{alias, view, version?}}]。view は id か名前\n\
         \n\
         - 保存できないときは、直す点を全部返す（画面の行・部品のキー・ワークフローの入力の食い違い）。直して保存し直す。\n\
         - 1 回の応答で作れるアプリは {MAX_APPS_PER_RUN} 件まで。作ったアプリを直すのは miniapp_update。\n\
         - 成功したら末尾の「参照:」の行の id と version を、次の直し（miniapp_update）に渡す。\n\
         \n\
         {}",
        gui::miniapp_screen_guide()
    )
}

/// 入力。
struct CreateInput {
    name: String,
    description: String,
    screen: String,
    workflows: Vec<super::pins::PinInput>,
    data_views: Vec<super::pins::PinInput>,
}

fn parse(input: &Value) -> Result<CreateInput, String> {
    let Some(obj) = input.as_object() else {
        return Err("入力はオブジェクトにしてください。".into());
    };
    const KEYS: [&str; 5] = ["name", "description", "screen", "workflows", "data_views"];
    if let Some(k) = obj.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!(
            "「{k}」は受けない入力です。使えるのは {} です。",
            KEYS.join("・")
        ));
    }
    let text = |k: &str| obj.get(k).and_then(Value::as_str).unwrap_or_default();
    let name = text("name").trim().to_string();
    if name.is_empty() {
        return Err("name（アプリの名前）を書いてください。".into());
    }
    Ok(CreateInput {
        name,
        description: text("description").trim().to_string(),
        screen: text("screen").to_string(),
        workflows: read_pins(obj.get("workflows"), WORKFLOWS)?.unwrap_or_default(),
        data_views: read_pins(obj.get("data_views"), DATA_VIEWS)?.unwrap_or_default(),
    })
}

#[async_trait]
impl Tool for MiniappCreateTool {
    fn name(&self) -> &str {
        ToolName::MiniappCreate.as_str()
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "アプリの名前" },
                "description": { "type": "string", "description": "アプリの説明" },
                "screen": { "type": "string", "description": "画面の Markdown（```ui の app と ```data_grid）" },
                "workflows": {
                    "type": "array",
                    "description": "束ねるワークフロー",
                    "items": {
                        "type": "object",
                        "properties": {
                            "alias": { "type": "string" },
                            "workflow": { "type": "string", "description": "id か名前" },
                            "version": { "type": "integer", "description": "省くと最新の版" }
                        },
                        "required": ["alias", "workflow"]
                    }
                },
                "data_views": {
                    "type": "array",
                    "description": "束ねる保存ビュー",
                    "items": {
                        "type": "object",
                        "properties": {
                            "alias": { "type": "string" },
                            "view": { "type": "string", "description": "id か名前" },
                            "version": { "type": "integer", "description": "省くと最新の版" }
                        },
                        "required": ["alias", "view"]
                    }
                }
            },
            "required": ["name", "description", "screen", "workflows"]
        })
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let input = match parse(&input) {
            Ok(i) => i,
            Err(msg) => return Ok(ToolOutcome::error(msg)),
        };
        let artifacts = &self.deps.artifacts;
        let workflows = match resolve_pins(
            artifacts,
            ctx,
            WORKFLOWS,
            &input.workflows,
            &DefaultVersion::Latest,
            trace_id,
        )
        .await
        {
            Ok(p) => p,
            Err(msg) => return Ok(ToolOutcome::error(msg)),
        };
        let views = match resolve_pins(
            artifacts,
            ctx,
            DATA_VIEWS,
            &input.data_views,
            &DefaultVersion::Latest,
            trace_id,
        )
        .await
        {
            Ok(p) => p,
            Err(msg) => return Ok(ToolOutcome::error(msg)),
        };
        let mut body = json!({
            "description": input.description,
            "screen": input.screen,
            "workflows": workflows.iter().map(super::pins::ResolvedPin::to_value).collect::<Vec<_>>(),
        });
        if !views.is_empty() {
            body["data_views"] = views
                .iter()
                .map(super::pins::ResolvedPin::to_value)
                .collect::<Vec<_>>()
                .into();
        }
        match artifacts
            .count_own_created_since(ctx, ArtifactKind::MiniApp, self.run.since)
            .await
        {
            Ok(n) if n >= MAX_APPS_PER_RUN => {
                // 上限でも、作成済みの同名の再試行は作成済みとして返す（再試行で上限の誤りにしない）。
                return Ok(match self.name_taken(ctx, &input.name, &body, trace_id).await {
                    Some(o) => o,
                    None => ToolOutcome::error(format!(
                        "この応答ではもう {MAX_APPS_PER_RUN} 件のアプリを作りました。これ以上は作りません。\
                         作ったアプリを直すなら miniapp_update を使ってください。"
                    )),
                });
            }
            Ok(_) => {}
            Err(e) => return Ok(internal(&e)),
        }
        match self
            .deps
            .apps
            .create(ctx, &input.name, &body, SaveMode::Agent, trace_id)
            .await
        {
            Ok(s) => Ok(saved(
                s.id,
                &input.name,
                s.version,
                s.published_version,
                &s.body,
                true,
            )),
            Err(GuiError::Artifact(ArtifactError::Conflict(ConflictReason::NameTaken { .. }))) => {
                Ok(match self.name_taken(ctx, &input.name, &body, trace_id).await {
                    Some(o) => o,
                    None => ToolOutcome::error(format!(
                        "名前「{}」のアプリは作れません（同じ名前のものがあります）。別の名前にしてください。",
                        crate::data_tool::one_line(&input.name, 60)
                    )),
                })
            }
            Err(e) => Ok(save_failure(&e, "作成")),
        }
    }
}

impl MiniappCreateTool {
    /// 本人がこの会話の run の間に同じ名前で作っていたときの観測（同じ本文なら作成済み・違えば直し方）。
    async fn name_taken(
        &self,
        ctx: &AuthContext,
        name: &str,
        body: &Value,
        trace_id: Option<&str>,
    ) -> Option<ToolOutcome> {
        match on_name_taken(
            &self.deps.artifacts,
            ctx,
            BuilderKind::MiniApp,
            name,
            self.run.since,
            body,
            trace_id,
        )
        .await
        {
            Ok(NameTaken::Same(found)) => Some(already_created(&found)),
            Ok(NameTaken::Differs(found)) => Some(created_differently(
                &found,
                &format!(
                    "直すなら miniapp_update に app={} と expected_version={} を渡してください（承認カードが出ます）。\
                     別のアプリとして作るなら、name を別の名前にしてください。",
                    found.id, found.version
                ),
            )),
            Ok(NameTaken::Other) => None,
            Err(e) => {
                tracing::warn!(error = %e, "作成済みのアプリの照合に失敗（作れなかった旨を返す）");
                None
            }
        }
    }
}
