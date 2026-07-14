//! `emit_ui` ツール（Task 6.4）— LLM が UI スペックを発話する唯一の手段。
//!
//! 検証（[`SpecValidator`]）を通過したスペックだけを `ToolOutcome::ui_specs` に載せ、
//! chat 側が generative_ui ブロックとして SSE 配信・永続化する。検証失敗は
//! `is_error` の観測としてモデルへ返し、モデルが自己修正するか通常テキストで回答する
//! （＝安全なテキストフォールバック。**未検証スペックがブロック化される経路は存在しない**）。

use std::sync::Arc;

use agent_core::{Tool, ToolError, ToolName, ToolOutcome};
use authz::AuthContext;

use crate::validator::SpecValidator;

/// UI スペック発話ツール。
pub struct EmitUiTool {
    validator: Arc<SpecValidator>,
}

impl EmitUiTool {
    pub fn new(validator: Arc<SpecValidator>) -> Self {
        EmitUiTool { validator }
    }
}

#[async_trait::async_trait]
impl Tool for EmitUiTool {
    fn name(&self) -> &str {
        ToolName::EmitUi.as_str()
    }

    fn description(&self) -> &'static str {
        "フォーム・テーブル・チャート・指標タイル等の UI をチャット内に描画する。spec には \
         信頼カタログのコンポーネントツリー（JSON）を渡す。使えるコンポーネント: container / \
         text / link / form（fields に text_input・select・checkbox・radio・date・slider・rating。\
         select/radio/checkbox は allow_other で自由記述可・date は range で期間・slider は min/max/step?・\
         rating は max?）/ button / table / \
         chart / stat / callout / accordion / tabs / stepper / badge_list / key_value / \
         code_block。chart の kind は bar・line・area・pie・donut・scatter・radar・radial_bar・\
         combo・funnel・treemap（データ点は {x, y, series?, xv?}、stacked=true で bar/area 積み上げ、\
         combo は line_series に線で描く系列名を列挙）。stat は KPI タイル（label・value・unit?・\
         delta?・delta_label?・trend?[数値列で sparkline]・caption?）。callout は tone \
         (info/success/warning/danger)+title?+text。accordion は items[{title,open?,children}]、\
         tabs は tabs[{label,children}]、stepper は steps[{title,status:todo/doing/done,description?}]、\
         badge_list は badges[{label,tone?}]、key_value は title?+items[{key,value}]、code_block は \
         code+language?（表示専用）。ユーザーへ確認・選択を求めたいときは question_card で \
         質問できる（id・title?・intro?・submit・questions・submit_label?）。フロントは 1 問ずつ \
         ステップ表示する。各質問は {id, header?[短い見出しチップ], question[質問文], \
         options?[各 {label, description?} の選択肢カード], multi_select?, allow_other?, \
         placeholder?}。options を空にすると自由記述（テキストエリア・長文可）になる。回答は \
         chat.submit へまとめて送信され次ターンの発話になる。選択肢に説明を添え、数値は選択肢か \
         自由記述で問う（スライダーは使わない）。地図は map で描ける（旅行/出張のルート提示など）。\
         center{lat,lng}・zoom?・markers[{lat,lng,label?,description?,kind?:place/start/end/stop/\
         lodging/food/sight}]・route?{waypoints[{lat,lng}](2 点以上・順に線で結ぶ),mode?:driving/\
         walking/transit/flight}・bounds?{south,west,north,east}・title? を渡す。緯度経度は \
         lat∈[-90,90]・lng∈[-180,180] の構造化データのみ（タイル URL はサーバ設定で注入されるため \
         指定しない）。ドメインカード（表示専用）: source_card は RAG 引用元 \
         （title?・sources[{title,snippet?,url?(https),score?,label?}]）、itinerary は旅程 \
         （title?・days[{label?,date?,items[{time?,title,description?,location?,kind?:activity/\
         travel/food/lodging/sight}]}]）、weather は天気（location・days[{label,condition:sunny/\
         partly_cloudy/cloudy/rain/storm/snow/fog,high?,low?,precipitation?(0-100)}]）、comparison \
         は比較表（columns[列見出し]・rows[{label,values[列と同数]}]・highlight?[推し列 index]）、\
         timeline は時系列イベント（events[{time?,title,description?,tone?}]）。フォーム送信やボタンは \
         spec.actions に宣言した束縛（type: handler の chat.submit、type: tool の doc_search / \
         web_search、type: workflow の name 参照）だけを action id で参照できる。検証に失敗した \
         場合はエラーを直して再試行するか、通常のテキストで回答する。"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "spec": {
                    "type": "object",
                    "description": "UI スペック。{ version: 1, actions: [...], root: { component: ... } }"
                }
            },
            "required": ["spec"]
        })
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let Some(spec) = input.get("spec") else {
            return Ok(ToolOutcome::error(
                "spec がありません。{ \"spec\": { \"version\": 1, \"root\": ... } } を渡してください。",
            ));
        };
        match self.validator.validate(ctx, spec, "emit", trace_id).await {
            Ok(resolved) => {
                let mut outcome = ToolOutcome::ok("UI を表示しました。");
                outcome.ui_specs.push(resolved.json);
                Ok(outcome)
            }
            Err(errors) => {
                // 全件をモデルへ観測として返す（自己修正 or テキスト回答へのフォールバック）。
                let detail: Vec<String> = errors
                    .iter()
                    .map(|e| match &e.path {
                        Some(p) => format!("- [{}] {} (at {})", e.code, e.message, p),
                        None => format!("- [{}] {}", e.code, e.message),
                    })
                    .collect();
                Ok(ToolOutcome::error(format!(
                    "UI スペック検証に失敗しました（UI は表示されません）:\n{}",
                    detail.join("\n")
                )))
            }
        }
    }
}
