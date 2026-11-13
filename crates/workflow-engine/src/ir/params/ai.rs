//! AI ノード（llm.invoke / agent.invoke / skill.invoke）の params 契約（ir.md §7.3/§7.4/§7.7）。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ir::expr::ValueExpr;
use crate::ir::patterns::PatternBudget;
use crate::vocab::AgentTool;

/// `llm.invoke` — llm-gateway 直行（pure・課金あり）。
///
/// `model` は省略可。省略すると実行のたびに実行主体の組織の既定（Auto を含む）で解決する
/// （チャットと同じ規則・ir.md §7.3）。指定したときは保存時に組織が許可したモデル（本人専用を除く）
/// へ照合し（V3）、実行時にも照らし直す（使えなければ既定へ戻さずに失敗する）。
///
/// `output_schema` を書くと構造化出力になる（GAP-13）。出力に検証済みの `data` が付き、形に合わない出力は
/// 1 回だけ直させてから `llm_output_invalid` で失敗する（[`crate::ir::output_schema`]）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct LlmInvokeParams {
    /// モデル id（省略＝組織の既定。指定時は組織が許可したモデルへ保存時照合・実行時再照合）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
    /// ユーザープロンプト。
    pub prompt: ValueExpr,
    /// システムプロンプト（省略可）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub system: Option<ValueExpr>,
    /// 出力トークン上限（省略時は gateway 既定）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub max_tokens: Option<ValueExpr>,
    /// 思考の深さ（省略＝組織の既定。書くと組織の既定〔Auto の思考強度を含む〕より優先する。
    /// 対応しないモデルでは送らない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub effort: Option<LlmEffort>,
    /// 出力の形（JSON Schema のサブセット・リテラル）。書くと出力に検証済みの `data` が付く。根は object で、
    /// オブジェクトは閉じる（additionalProperties は false か省略）。制約は `ir::output_schema` が保存時に検査する。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "Record<string, unknown>")]
    pub output_schema: Option<serde_json::Value>,
}

/// llm.invoke の思考の深さ（3 段階・llm-gateway が各プロバイダの思考の設定へ写す・design.md §4.5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum LlmEffort {
    Low,
    Medium,
    High,
}

impl LlmInvokeParams {
    /// 項目どうしの整合（`output_schema` の形・上限）。V1 の typed 契約の照合の後に呼ぶ。`pattern` の数は、この
    /// ブロックの出力の形だけで数える（ワークフローの全体で数えるのは保存時検証の [`crate::ir::validate::validate`]）。
    ///
    /// # Errors
    /// `output_schema` が出力の形の制約に合わないとき（path は `/params/output_schema/...`）。
    pub fn check_cross_fields(&self) -> Result<(), super::ParamsIssue> {
        self.check_cross_fields_with(&mut PatternBudget::for_save())
    }

    /// [`Self::check_cross_fields`] の `pattern` の数を、ワークフローの全体で数える形（`patterns` を全部のブロックで
    /// 使い回す・BUG-226）。
    pub(crate) fn check_cross_fields_with(
        &self,
        patterns: &mut PatternBudget,
    ) -> Result<(), super::ParamsIssue> {
        let Some(schema) = &self.output_schema else {
            return Ok(());
        };
        crate::ir::output_schema::check_saved_output_schema(schema, patterns).map_err(|issue| {
            super::ParamsIssue {
                expected: Vec::new(),
                path: format!("/params/output_schema{}", issue.path),
                message: issue.message,
            }
        })
    }
}

/// `agent.invoke` — agent-core のツールループを step から 1 回走らせる（ir.md §7.4・BUG-6）。
///
/// 依頼（`instruction`）は自然文。モデルは道具（`allowed_tools`・能力 API の名前）を使って作業し、最後の
/// 回答を本文に書く。道具の呼び出しはエンジンの能力ゲートウェイ（scope 天井 → 宛先 → 監査）を通ってから
/// チョークポイント（ReBAC・二段 authz）へ届く。設定は**縮小のみ**（実効 = 実行主体 ReBAC ∩
/// declared_scopes ∩ 提示した道具の要求スコープ）。
///
/// - `allowed_tools` の省略は「道具なし」（旧 IR の到達範囲を再同意なしに広げない・D-29）。書込の道具は
///   v1 では語彙に無い（書込は後段の能力ノードで行う）。
/// - `egress_allowlist` は web.fetch の宛先にだけ効き、管理者が決める web の天井
///   （`workflow.agent.web_allowlist`）の部分集合に限る。天井が空なら web.fetch は提示しない。
/// - `model` の省略は組織の既定（D-5）。上限（手順・合計トークン・時間）はデプロイ設定を天井に縮めるだけ。
/// - `mount_scope`・`output_schema` は効くようにしてから足す（効かない設定を契約に入れない）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct AgentInvokeParams {
    /// エージェントへの依頼（自然文・文字列に解決される）。
    pub instruction: ValueExpr,
    /// 参考データ（任意の JSON）。モデルには「指示ではないデータ」として依頼とは別に渡す。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub context: Option<ValueExpr>,
    /// 使ってよい道具（リテラル・省略は道具なし）。保存時に要るスコープが declared_scopes に補われる。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub allowed_tools: Option<Vec<AgentTool>>,
    /// モデル id（省略＝組織の既定。指定時は組織が許可したモデルへ保存時照合・実行時再照合）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
    /// 手順（モデルの応答）の上限（1〜24・省略は 8）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub max_steps: Option<u32>,
    /// この実行全体で使うトークンの上限（デプロイ設定より大きくはできない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub max_tokens: Option<u64>,
    /// web.fetch で読んでよいサイト（リテラル・管理者の web の天井の部分集合・最大 20 件）。
    #[serde(default)]
    pub egress_allowlist: Vec<String>,
    /// 実行時間上限（秒・10〜3600・省略は 600）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub max_duration_sec: Option<ValueExpr>,
}

/// `skill.invoke` — インストール済み skill の実行（ir.md §7.7・#344 Task 10.1b）。
///
/// `skill` は `skill:<name>@<version>` のリテラル参照（保存時にレジストリ＋保存ユーザーの
/// インストール集合へ照合＝V4・実行時に実行主体 ReBAC で再検証＝fail-closed）。
/// skill が `.shiki` script を持てば script-runtime で実行し、instructions のみなら
/// agent.invoke 経路（サンドボックス）で実行する（新しい実行機構は作らない）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SkillInvokeParams {
    /// `skill:<name>@<version>` のリテラル参照（$from 不可・照合可能性のため）。
    pub skill: String,
    /// skill への入力（省略時はノード入力をそのまま渡す）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub input: Option<ValueExpr>,
}
