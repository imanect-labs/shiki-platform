//! 保存時検証パイプライン V1〜V7（ir.md §8・固定順・全件エラー収集）。
//!
//! AI 編集も dnd 保存も同一パイプラインを通す。最初の 1 件で止めず全エラーを配列で返す
//! （dnd がノード上に表示）。V4 は secret 照合＋skill 照合（10.1b・#344）。

mod agent;
pub use agent::{pattern_within, valid_host_pattern};
mod approval;
pub use approval::{CatalogTable, APPROVAL_STATUS_FIELD_SHOWN, APPROVAL_TIMEOUTS_EXCEED_RUN};
mod cache;
pub use cache::CACHE_UNSUPPORTED;
mod decide;
pub use decide::{DECIDE_LOW_CONFIDENCE, DECIDE_MANY_PORTS, DECIDE_UNPINNED_MODEL};
mod llm;
pub use llm::{EFFORT_IGNORED, MIN_STRUCTURED_MAX_TOKENS, OUTPUT_SCHEMA_FEW_TOKENS};
mod ports;
pub use ports::{node_ports, output_ports};
mod readiness;
pub use readiness::{
    empty_regions, empty_values, enable_blockers, required_pick_fields, required_text_empty,
    run_blockers, unattended_run_blockers, EMPTY_REGION, EMPTY_VALUES, LIST_FOLDER_REQUIRED,
    REQUIRED_TEXT_EMPTY, WRITE_FOLDER_REQUIRED,
};
mod refs;
pub(crate) use refs::parse_skill_ref;
mod reply;
mod transition;
pub use reply::{MAX_REPLIES, REPLY_WITHOUT_INTERACTIVE};
mod semantics;
pub use semantics::{semantics_warnings, SEMANTICS_CHANGED};
mod template;
pub use template::{TEMPLATE_ESCAPED_VAR, TEMPLATE_UNCLOSED_BRACE};
mod timeout;
mod v2_graph;
pub use v2_graph::UNCONNECTED_NODE;
mod v3_vocab;
pub use v3_vocab::{OVERLAPPING_DATA_TRIGGERS, WAIT_FILTER_ALIAS};
mod v5_dataflow;
mod warning_codes;
pub use warning_codes::SAVE_WARNING_CODES;

use std::collections::BTreeMap;

use crate::ir::{Node, WorkflowIr};
use crate::vocab::NodeType;

/// 誤りの文言でブロックを名指しする名前（付けた名前・無ければブロックの種類の名前・それも無ければ id）。
pub(super) fn block_name(node: &Node) -> String {
    match node.label.as_deref().map(str::trim) {
        Some(label) if !label.is_empty() => label.to_string(),
        _ => NodeType::parse(&node.node_type).map_or_else(
            || node.id.clone(),
            |nt| crate::vocab::label_ja(nt).to_string(),
        ),
    }
}

/// 検証エラー（コード＋メッセージ＋位置）。dnd がノード/エッジに紐付けて表示する。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS, utoipa::ToSchema)]
#[ts(export)]
pub struct ValidationError {
    /// エラーコード（例: `ir.schema_violation`）。
    pub code: String,
    /// 人向けメッセージ。
    pub message: String,
    /// 紐付くノード id（該当時）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// 紐付くエッジ（該当時・`from -> to`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge: Option<String>,
    /// IR 内の位置（`/params/...` の JSON Pointer 風・該当時）。dnd がフォームフィールドへ写像する。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl ValidationError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        ValidationError {
            code: code.into(),
            message: message.into(),
            node_id: None,
            edge: None,
            path: None,
        }
    }

    #[must_use]
    pub fn at_node(mut self, node_id: impl Into<String>) -> Self {
        self.node_id = Some(node_id.into());
        self
    }

    #[must_use]
    pub fn at_edge(mut self, edge: impl Into<String>) -> Self {
        self.edge = Some(edge.into());
        self
    }

    #[must_use]
    pub fn at_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }
}

/// 検証に必要な外部カタログ（vocab の Stage A 集合＋登録済み secret 等）。
///
/// 純粋な検証にするため、DB 参照（secret/skill 存在）は API 層が事前解決して渡す。
#[derive(Debug, Default)]
pub struct Catalog {
    /// 登録済み secret の参照名 → 許可ホスト（V4 secret 照合・宛先束縛の事前チェック）。
    pub secrets: BTreeMap<String, Vec<String>>,
    /// llm.invoke に書けるモデル（組織が許可したモデル・本人専用を除く）。
    ///
    /// `None` は照合しない（LLM が未構成）。`Some` は空でも照合する（組織が許可したモデルが 0 件なら
    /// 明示したモデルはすべて拒否する・BUG-37）。model の省略はどちらでも通る（組織の既定・BUG-12）。
    pub models: Option<Vec<String>>,
    /// 組織がモデルの選択を禁じているときの既定（`auto` かモデル ID）。`Some` なら、これと違う
    /// model を拒否する（ワークフローでもモデルを選ばせない・D-30）。判定のモデルには掛けない（判定には
    /// 組織の既定が無く、実行時の判定のチョークポイントも選択の禁止を見ない）。
    pub model_lock: Option<String>,
    /// 組織の既定が Auto か（モデルを省略した llm.invoke は Auto の思考強度で動き、ノードに書いた思考の深さを
    /// 使わない・D-75。保存時の警告 [`EFFORT_IGNORED`] に使う）。分からない構成（LLM が未構成）では false。
    pub default_is_auto: bool,
    /// ai.decide に書ける判定用のモデル（組織が許可し、元にする chat モデルも許可されているもの・FEAT-1）。
    ///
    /// [`Self::models`] と同じく、`None` は照合しない（LLM が未構成）。`Some` は空でも照合する（判定用の
    /// モデルが 0 件なら ai.decide はすべて拒否する）。llm.invoke・agent.invoke にここのモデルを書くと
    /// `ir.model_modality_mismatch`。
    pub decision_models: Option<Vec<String>>,
    /// 保存ユーザーのインストール済み skill（name → レジストリ version 集合・V4 skill 照合・
    /// #344 Task 10.1b）。インストールはユーザー単位のため保存者基準で照合し、実行時は
    /// 実行主体の ReBAC で再検証する（fail-closed が最終防衛線・ir.md §8）。
    pub skills: BTreeMap<String, std::collections::BTreeSet<String>>,
    /// 承認（human.approval）・状態を進める（data.transition）・状態のきっかけが束縛するテーブル（UUID →
    /// 保存者に見える列と FSM の状態・V4・GAP-08・BUG-105）。API 層が IR のテーブルを保存者の権限で引いて
    /// 詰める（見えないテーブルは入れない）。`None` は照合しない（テーブルを引けない構成・AI 編集の説明文の
    /// 検査など）。
    pub tables: Option<BTreeMap<String, CatalogTable>>,
    /// AI エージェントの web.fetch が読んでよいサイトの天井（管理者が決める `workflow.agent.web_allowlist`・
    /// BUG-6・D-29）。ノードの `egress_allowlist` はこの部分集合に限る。`None` は照合しない（設定を引けない
    /// 構成・AI 編集の説明文の検査など）。`Some(空)` は外部サイトを許可しない。
    pub web_ceiling: Option<Vec<String>>,
    /// 別のワークフローを実行（workflow.start）が名前で指す相手のうち、保存者に見えるもの（V4・BUG-107）。
    ///
    /// API 層が IR の workflow.start の名前（リテラル）だけを保存者の権限で引いて詰める（見えない・無い名前は
    /// 入れない＝どちらも同じ誤り）。`None` は照合しない（引けない構成・AI 編集の説明文の検査など）。
    pub workflows: Option<std::collections::BTreeSet<String>>,
}

/// 保存を止めない警告をすべて返す（保存 API・検証 API・チャットの emit_workflow が利用者へ示す）。
///
/// 条件の意味論の切替で振る舞いが変わり得る条件（[`SEMANTICS_CHANGED`]・[`semantics_warnings`]）と、
/// vars のキーを `{{key}}` と書いたテンプレート（[`TEMPLATE_ESCAPED_VAR`]）と、閉じの無い `{` のある
/// テンプレート（[`TEMPLATE_UNCLOSED_BRACE`]・D-37）と、同じテーブルで重なる
/// データ変更トリガ（[`OVERLAPPING_DATA_TRIGGERS`]）と、wait(event) の filter の別名（[`WAIT_FILTER_ALIAS`]）と、
/// 続けて待つ承認の期限の合計が実行の期限以上のもの（[`APPROVAL_TIMEOUTS_EXCEED_RUN`]）と、手動で動かす
/// きっかけの無い IR の画面に返す結果（[`REPLY_WITHOUT_INTERACTIVE`]）と、出力の形を書いた llm.invoke の
/// 小さすぎる上限トークン（[`OUTPUT_SCHEMA_FEW_TOKENS`]・GAP-13）と、組織の設定に従うので使われない思考の深さ
/// （[`EFFORT_IGNORED`]・D-75・組織の設定は `catalog` から読む）と、判定のしきい値が低い・出口が多い・
/// 版が固定されていないもの（[`DECIDE_LOW_CONFIDENCE`]・[`DECIDE_MANY_PORTS`]・[`DECIDE_UNPINNED_MODEL`]・
/// FEAT-1）と、線が 1 本も無いブロック（[`UNCONNECTED_NODE`]・実行の始めに動く・BUG-106）と、必須の文字の欄が空のもの
/// （[`REQUIRED_TEXT_EMPTY`]・有効化と実行の前には止める・D-43）と、中にブロックの無い繰り返し（[`EMPTY_REGION`]・
/// 有効化と実行の前には止める・D-85）と、値を書く欄の中身の無い入れ物（[`EMPTY_VALUES`]・何も止めない）。
/// 出しうるコードは [`SAVE_WARNING_CODES`] に載せる（web はその表から生成した定数で受ける・BUG-48）。
#[must_use]
pub fn save_warnings(ir: &WorkflowIr, catalog: &Catalog) -> Vec<ValidationError> {
    let mut out = semantics_warnings(ir);
    out.extend(template::warnings(ir));
    out.extend(readiness::required_text_empty(ir));
    out.extend(readiness::empty_regions(ir));
    out.extend(readiness::empty_values(ir));
    out.extend(v3_vocab::event_warnings(ir));
    out.extend(approval::warnings(ir));
    out.extend(reply::warnings(ir));
    out.extend(llm::warnings(ir));
    out.extend(llm::org_warnings(ir, catalog));
    out.extend(decide::warnings(ir));
    out.extend(v2_graph::warnings(ir));
    // 表に無い警告は web の定数に出ない（警告を足したら表にも足す・テストのどの経路で起きても止める）。
    debug_assert!(
        out.iter()
            .all(|w| SAVE_WARNING_CODES.iter().any(|(_, c)| *c == w.code)),
        "SAVE_WARNING_CODES に無い警告: {out:?}"
    );
    out
}

/// params の契約の誤りの文言（V1b）。
fn params_message(message: &str) -> String {
    format!("params が不正です: {message}")
}

/// IR の JSON を V1〜V7 で検証する。エラーが 1 件でもあれば `Err(全件)`。
///
/// 成功時はパース済み [`WorkflowIr`] を返す（保存に使う）。
pub fn validate(
    value: &serde_json::Value,
    catalog: &Catalog,
) -> Result<WorkflowIr, Vec<ValidationError>> {
    // V1: スキーマ（deny-unknown・型・必須）。パース失敗はここで打ち切る（後段が構造に依存）。
    let ir = match WorkflowIr::from_json(value) {
        Ok(ir) => ir,
        Err(e) => {
            return Err(vec![ValidationError::new(
                "ir.schema_violation",
                format!("スキーマ検証に失敗しました: {e}"),
            )]);
        }
    };

    let mut errors = Vec::new();
    // ワークフロー名は安定参照名の契約 `^[a-z][a-z0-9-]{0,63}$`（workflow.start の名前解決に使う）。
    if !refs::is_valid_workflow_name(&ir.name) {
        errors.push(ValidationError::new(
            "ir.bad_name",
            format!(
                "name は ^[a-z][a-z0-9-]{{0,63}}$ に一致する必要があります: {}",
                ir.name
            ),
        ));
    }
    // V1b: ノード params の typed 契約照合（deny-unknown・必須・型・ir.md §7「params の正は codegen」）。
    // 予約語彙/未知 type は V3 が拒否するため対象外。エラーはノード単位に path 付きで収集する。
    for node in &ir.nodes {
        let Some(nt) = crate::vocab::NodeType::parse(&node.node_type) else {
            continue;
        };
        if !nt.available_stage_a() {
            continue;
        }
        if let Err(issue) = crate::ir::params::check_params(nt, &node.params) {
            errors.push(
                ValidationError::new("ir.schema_violation", params_message(&issue.message))
                    .at_node(&node.id)
                    .at_path(issue.path),
            );
        }
    }
    // V1d: step 属性 `cache`（付けられる種・期限の範囲・T10）。
    cache::v1(&ir, &mut errors);
    // V1c: input_schema の項目の並びの注記 `x-order`（D-60）。起動の検証は注記として読み飛ばすので、
    // 形（properties のキーの文字列の配列・重なりなし）は保存で止める。
    if let Some(schema) = &ir.input_schema {
        for (at, message) in crate::ir::input_schema::order_errors(schema) {
            errors.push(
                ValidationError::new("ir.schema_violation", message)
                    .at_path(format!("/input_schema{at}")),
            );
        }
    }
    // V7: 上限（他段より先に軽く弾く）。
    v7_limits(value, &ir, &mut errors);
    // V2: グラフ（id 一意・エッジ参照・DAG・入エッジ制約・領域閉包・到達性・reply は本体だけ）。
    v2_graph::check(&ir, &mut errors);
    reply::v2(&ir, &mut errors);
    // V3: 語彙照合（node type・scope・event source・model）。
    v3_vocab::v3_vocab(&ir, catalog, &mut errors);
    decide::v3(&ir, catalog, &mut errors);
    agent::v3(
        &ir,
        catalog,
        &ir.declared_scopes.iter().map(String::as_str).collect(),
        &mut errors,
    );
    // V4: 参照存在（secret の宛先束縛事前チェック＋skill のインストール済み照合・#344・承認のテーブルと状態・
    // 状態を進めるテーブルと状態のきっかけのテーブル・BUG-105）。
    refs::v4_refs(&ir, catalog, &mut errors);
    approval::v4(&ir, catalog, &mut errors);
    transition::v4(&ir, catalog, &mut errors);
    // V5: データフロー（$from 祖先性・default 要否・条件木型整合・regex）。
    v5_dataflow::check(&ir, &mut errors);
    agent::v5(&ir, &mut errors);
    // V6: script コンパイル（inline の swc パース・禁止構文）。
    refs::v6_script(&ir, &mut errors);

    if errors.is_empty() {
        Ok(ir)
    } else {
        Err(errors)
    }
}

/// V7: 上限（ir.md §8）。
fn v7_limits(value: &serde_json::Value, ir: &WorkflowIr, errors: &mut Vec<ValidationError>) {
    use crate::ir::expr::MAX_CONDITION_DEPTH;

    if ir.ir_version == 0 || ir.ir_version > crate::ir::MAX_IR_VERSION {
        errors.push(ValidationError::new(
            "ir.unknown_version",
            format!("未知の ir_version: {}", ir.ir_version),
        ));
    }
    if ir.nodes.len() > 200 {
        errors.push(ValidationError::new(
            "ir.limit_exceeded",
            format!("ノード数が上限 200 を超過しています: {}", ir.nodes.len()),
        ));
    }
    if ir.edges.len() > 500 {
        errors.push(ValidationError::new(
            "ir.limit_exceeded",
            format!("エッジ数が上限 500 を超過しています: {}", ir.edges.len()),
        ));
    }
    // IR サイズ（1MB）。
    if let Ok(bytes) = serde_json::to_vec(value) {
        if bytes.len() > 1024 * 1024 {
            errors.push(ValidationError::new(
                "ir.limit_exceeded",
                format!("IR が上限 1MB を超過しています: {} bytes", bytes.len()),
            ));
        }
    }
    // policies の run_timeout 上限。
    if ir.policies.run_timeout_sec > crate::ir::MAX_RUN_TIMEOUT_SEC {
        errors.push(ValidationError::new(
            "ir.limit_exceeded",
            "run_timeout_sec が上限（30 日）を超過しています".to_string(),
        ));
    }
    // テンプレートの vars の数（ir.md §3.2・§8）。
    template::check_limits(ir, errors);
    // 承認の期限は実行の期限未満（GAP-08）。
    approval::v7(ir, errors);
    // 画面に返す結果の数とリテラルの型（GAP-09）。
    reply::v7(ir, errors);
    // AI エージェントの手順・時間・トークン・依頼の上限（BUG-6）。
    agent::v7(ir, errors);
    // ノードの時間の上限（ノード種の上限・AI エージェントは作業時間の上限以上・BUG-96）。
    timeout::v7(ir, errors);
    // 条件木の深さ（トリガ filter・branch/wait params の condition）。
    for t in &ir.triggers {
        if let crate::ir::Trigger::Event(ev) = t {
            if let Some(cond) = &ev.filter {
                if cond.depth() > MAX_CONDITION_DEPTH {
                    errors.push(ValidationError::new(
                        "ir.limit_exceeded",
                        "イベント filter の条件木が深すぎます（最大 5）".to_string(),
                    ));
                }
            }
        }
    }
    // ノード params.condition（control.branch / control.wait）の条件木深さも上限内であること。
    // V5 は深さに関わらず再帰するため、ここで弾かないと深いノード条件が上限を素通りする。
    for node in &ir.nodes {
        if let Some(cond_json) = node.params.get("condition") {
            if let Ok(cond) =
                serde_json::from_value::<crate::ir::expr::Condition>(cond_json.clone())
            {
                if cond.depth() > MAX_CONDITION_DEPTH {
                    errors.push(
                        ValidationError::new(
                            "ir.limit_exceeded",
                            format!("ノード {} の条件木が深すぎます（最大 5）", node.id),
                        )
                        .at_node(&node.id),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalog() -> Catalog {
        Catalog::default()
    }

    #[test]
    fn valid_minimal_workflow() {
        let ir = json!({
            "ir_version": 1,
            "name": "wf",
            "declared_scopes": ["storage.read"],
            "nodes": [
                { "id": "read", "type": "storage.read", "params": { "file": { "$from": "input", "path": "/id" } } }
            ],
            "edges": []
        });
        assert!(validate(&ir, &catalog()).is_ok());
    }

    #[test]
    fn v3_event_trigger_scope_must_be_folder_binding() {
        // 全購読禁止: scope 欠落/空/非 folder キー/非 uuid は保存時拒否（fail-closed の一段目）。
        for bad_scope in [
            json!({}),
            json!({ "table": "expense" }),
            json!({ "folder": "not-a-uuid" }),
            json!({ "folder": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1", "extra": 1 }),
        ] {
            let ir = json!({
                "ir_version": 1, "name": "wf",
                "triggers": [{ "kind": "event", "source": "storage.write", "scope": bad_scope }],
                "nodes": [], "edges": []
            });
            let errs = validate(&ir, &catalog()).unwrap_err();
            assert!(
                errs.iter().any(|e| e.code == "ir.bad_event_scope"),
                "{bad_scope} が拒否されるべき: {errs:?}"
            );
        }
        let ok = json!({
            "ir_version": 1, "name": "wf",
            "triggers": [{ "kind": "event", "source": "storage.write",
                           "scope": { "folder": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1" } }],
            "nodes": [], "edges": []
        });
        assert!(validate(&ok, &catalog()).is_ok());
    }

    /// 親の連鎖が輪になった繰り返しを含む IR でも、検証は止まって誤りを返す（V2 の深さの数え上げと V5 の祖先の
    /// たどりが止まらず、保存と検証の API が返らなかった）。
    #[test]
    fn a_cyclic_parent_chain_returns_errors() {
        let ir = json!({
            "ir_version": 1, "name": "wf",
            "nodes": [
                { "id": "m", "type": "control.map", "parent": "m",
                  "params": { "items": { "$from": "input", "path": "/rows" } } },
                { "id": "leaf", "type": "storage.read", "parent": "m",
                  "params": { "file": { "$from": "nodes.m.output" } } }
            ],
            "edges": []
        });
        let errs = validate(&ir, &catalog()).unwrap_err();
        assert!(errs.iter().any(|e| e.code == "ir.bad_region"), "{errs:?}");
    }

    /// BUG-151: 置いたばかりの業務データ・ファイルを読む・スキルのブロック（エディタは選ぶ欄・書く欄を空の文字で置く）は
    /// 保存でき、空の欄は保存の警告になる（実行と自動実行のオンの前に止まる）。以前は「項目 'data' がありません」・
    /// 「skill 参照は skill:<name>@<version> 形式です」で保存が止まった。
    #[test]
    fn freshly_placed_data_blocks_can_be_saved() {
        let ir = json!({
            "ir_version": 1, "name": "wf",
            "triggers": [{ "kind": "interactive" }],
            "nodes": [
                { "id": "add", "type": "data.record.create", "params": { "table": "", "data": "" } },
                { "id": "move", "type": "data.transition", "params": { "table": "", "record": "", "to": "" } },
                { "id": "bulk", "type": "data.bulk_upsert", "params": {} },
                { "id": "read", "type": "storage.read", "params": { "file": "" } },
                { "id": "skill", "type": "skill.invoke", "params": { "skill": "" } }
            ],
            "edges": [
                { "from": "add", "to": "move" }, { "from": "move", "to": "bulk" },
                { "from": "bulk", "to": "read" }, { "from": "read", "to": "skill" }
            ]
        });
        // 使ってよい操作は保存の API と同じく補う。
        let (ir, _) = crate::ir::scopes::complete_declared_scopes(&ir);
        let parsed = validate(&ir, &catalog()).expect("置いたばかりのブロックを保存できる");
        let warned: Vec<_> = save_warnings(&parsed, &catalog())
            .into_iter()
            .map(|w| (w.code, w.node_id, w.path))
            .collect();
        let at = |node: &str, field: &str| {
            (
                REQUIRED_TEXT_EMPTY.to_string(),
                Some(node.to_string()),
                Some(format!("/params/{field}")),
            )
        };
        assert_eq!(
            warned,
            vec![
                at("add", "table"),
                at("add", "data"),
                at("move", "table"),
                at("move", "record"),
                at("move", "to"),
                at("bulk", "table"),
                at("bulk", "rows"),
                at("read", "file"),
                at("skill", "skill"),
            ]
        );
        // 材料（テーブルの一覧）を引ける構成でも、空のテーブルは照合しない（見えないテーブルの誤りにしない）。
        let with_tables = Catalog {
            tables: Some(BTreeMap::new()),
            ..Catalog::default()
        };
        assert!(validate(&ir, &with_tables).is_ok());
    }

    #[test]
    fn v5_from_source_requires_exact_vocabulary() {
        // `each.item` のような「先頭一致だが実行時に解決不能」な source は保存時に弾く。
        for bad in ["each.item", "input.foo", "nodes.read", "nodes.read.out"] {
            let ir = json!({
                "ir_version": 1, "name": "wf",
                "declared_scopes": ["storage.read"],
                "nodes": [
                    { "id": "read", "type": "storage.read",
                      "params": { "file": { "$from": bad } } }
                ],
                "edges": []
            });
            let errs = validate(&ir, &catalog()).unwrap_err();
            assert!(
                errs.iter().any(|e| e.code == "ir.bad_ref"),
                "{bad} が拒否されるべき: {errs:?}"
            );
        }
    }

    /// BUG-1 の再現（AI が `{{変数}}` で書いた IR）: 保存は通すが、波括弧が出ることを警告で知らせる。
    #[test]
    fn mustache_template_saves_with_a_warning() {
        let ir = json!({
            "ir_version": 1, "name": "bug1-mustache", "declared_scopes": [],
            "triggers": [{ "kind": "interactive" }],
            "nodes": [{ "id": "echo", "type": "script.run", "params": {
                "source": { "inline": "function main(i){ return { got: i }; }" },
                "input": { "$template": "件名: {{title}} / 金額: {{amount}} 円",
                    "vars": { "title": { "$from": "input", "path": "/title" },
                              "amount": { "$from": "input", "path": "/amount" } } } } }],
            "edges": []
        });
        let parsed = validate(&ir, &catalog()).expect("保存は止めない");
        let w = save_warnings(&parsed, &Catalog::default());
        assert_eq!(w.len(), 2, "{w:#?}");
        assert!(w.iter().all(|e| e.code == TEMPLATE_ESCAPED_VAR));
        // 条件の意味論の切替の一覧（shiki-admin workflow-conditions）には混ぜない。
        assert!(semantics_warnings(&parsed).is_empty());
    }

    /// AI が書きがちな誤り（存在しない出口・vars に無い差し込み・祖先でない vars の参照）は全件まとめて返す。
    #[test]
    fn ai_style_mistakes_are_all_reported() {
        let ir = json!({
            "ir_version": 1, "name": "wf", "declared_scopes": ["storage.write"],
            "nodes": [
                { "id": "ask", "type": "llm.invoke", "params": { "prompt": "要約して" } },
                { "id": "save", "type": "storage.write", "params": {
                    "name": { "$template": "summary-{d}.md", "vars": { "date": "2026-10-25" } },
                    "content": { "$template": "{t}", "vars": { "t": { "$from": "ask" } } } } }
            ],
            "edges": [{ "from": "ask", "from_port": "next", "to": "save" }]
        });
        let errs = validate(&ir, &catalog()).unwrap_err();
        let codes: Vec<&str> = errs.iter().map(|e| e.code.as_str()).collect();
        assert!(codes.contains(&"ir.unknown_port"), "{errs:#?}");
        assert!(codes.contains(&"ir.bad_template"), "{errs:#?}");
        assert!(codes.contains(&"ir.bad_ref"), "{errs:#?}");
    }

    #[test]
    fn v1b_params_violation_has_node_and_path() {
        // typed params 契約違反が node_id＋path 付きの ir.schema_violation になる（dnd 表示契約）。
        let ir = json!({
            "ir_version": 1,
            "name": "wf",
            "declared_scopes": ["storage.read"],
            "nodes": [
                { "id": "read", "type": "storage.read", "params": { "file": "x", "surprise": 1 } }
            ],
            "edges": []
        });
        let errs = validate(&ir, &catalog()).unwrap_err();
        let e = errs
            .iter()
            .find(|e| e.code == "ir.schema_violation")
            .expect("params 契約違反が収集される");
        assert_eq!(e.node_id.as_deref(), Some("read"));
        assert!(
            e.path.as_deref().unwrap_or_default().starts_with("/params"),
            "path が /params 起点: {:?}",
            e.path
        );
    }

    #[test]
    fn v1_schema_violation() {
        let ir = json!({ "ir_version": 1, "name": "wf", "surprise": 1 });
        let errs = validate(&ir, &catalog()).unwrap_err();
        assert_eq!(errs[0].code, "ir.schema_violation");
    }

    /// D-60: input_schema の `x-order` は保存でき、形が合わなければ位置つきで保存を止める。
    #[test]
    fn v1c_input_order_is_checked_on_save() {
        let ir = |order: serde_json::Value| {
            json!({
                "ir_version": 1, "name": "wf", "triggers": [{ "kind": "interactive" }],
                "input_schema": { "type": "object", "x-order": order, "properties": {
                    "title": { "type": "string" }, "amount": { "type": "integer" } } },
                "nodes": [{ "id": "ask", "type": "llm.invoke", "params": { "prompt": "要約して" } }],
                "edges": []
            })
        };
        let saved = validate(&ir(json!(["title", "amount"])), &catalog()).expect("保存できる");
        assert_eq!(
            saved.input_schema.as_ref().and_then(|s| s.get("x-order")),
            Some(&json!(["title", "amount"]))
        );
        let errs = validate(&ir(json!(["title", "amout"])), &catalog()).unwrap_err();
        assert_eq!(errs.len(), 1, "{errs:#?}");
        assert_eq!(errs[0].code, "ir.schema_violation");
        assert_eq!(errs[0].path.as_deref(), Some("/input_schema/x-order/1"));
        assert_eq!(
            errs[0].message,
            "x-order の「amout」は properties にありません"
        );
        assert_eq!(errs[0].node_id, None);
    }

    #[test]
    fn v3_unknown_scope_and_node() {
        let ir = json!({
            "ir_version": 1, "name": "wf",
            "declared_scopes": ["sheet.read"],  // 未実装（予約語彙）
            "nodes": [{ "id": "q", "type": "notify.send", "params": {} }],
            "edges": []
        });
        let errs = validate(&ir, &catalog()).unwrap_err();
        assert!(errs.iter().any(|e| e.code == "ir.unknown_scope"));
        assert!(errs.iter().any(|e| e.code == "ir.unknown_node_type"));
    }

    #[test]
    fn v4_unknown_secret_and_binding() {
        let mut cat = Catalog::default();
        cat.secrets
            .insert("slack".into(), vec!["api.slack.com".into()]);
        // 未知 secret。
        let ir = json!({
            "ir_version": 1, "name": "wf", "declared_scopes": ["http.egress"],
            "nodes": [{ "id": "h", "type": "http.request",
                "params": { "url": "https://api.slack.com/x", "secret": { "name": "unknown" } } }],
            "edges": []
        });
        let errs = validate(&ir, &cat).unwrap_err();
        assert!(errs.iter().any(|e| e.code == "ir.unknown_secret"));

        // 宛先束縛が URL ホストを許容しない。
        let ir2 = json!({
            "ir_version": 1, "name": "wf", "declared_scopes": ["http.egress"],
            "nodes": [{ "id": "h", "type": "http.request",
                "params": { "url": "https://evil.com/x", "secret": { "name": "slack" } } }],
            "edges": []
        });
        let errs2 = validate(&ir2, &cat).unwrap_err();
        assert!(errs2.iter().any(|e| e.code == "ir.binding_denied"));
    }

    #[test]
    fn v4_skill_installed_check() {
        // 未インストール skill 参照は保存時拒否（10.1b・受け入れ条件③）。
        let ir = json!({
            "ir_version": 1, "name": "wf",
            "nodes": [{ "id": "s", "type": "skill.invoke",
                "params": { "skill": "skill:expense@1" } }],
            "edges": []
        });
        let errs = validate(&ir, &catalog()).unwrap_err();
        assert!(
            errs.iter().any(|e| e.code == "ir.unknown_skill"),
            "{errs:?}"
        );

        // インストール済みなら通る。
        let mut cat = Catalog::default();
        cat.skills
            .entry("expense".into())
            .or_default()
            .insert("1".into());
        assert!(validate(&ir, &cat).is_ok());

        // バージョン違いは拒否（version 固定・再現性）。
        let ir2 = json!({
            "ir_version": 1, "name": "wf",
            "nodes": [{ "id": "s", "type": "skill.invoke",
                "params": { "skill": "skill:expense@2" } }],
            "edges": []
        });
        let errs2 = validate(&ir2, &cat).unwrap_err();
        assert!(errs2.iter().any(|e| e.code == "ir.unknown_skill"));

        // 形式不正は ir.bad_ref。
        let ir3 = json!({
            "ir_version": 1, "name": "wf",
            "nodes": [{ "id": "s", "type": "skill.invoke",
                "params": { "skill": "expense" } }],
            "edges": []
        });
        let errs3 = validate(&ir3, &cat).unwrap_err();
        assert!(errs3.iter().any(|e| e.code == "ir.bad_ref"));
    }

    #[test]
    fn v6_script_syntax_error() {
        let ir = json!({
            "ir_version": 1, "name": "wf",
            "nodes": [{ "id": "s", "type": "script.run",
                "params": { "source": { "inline": "function main( { return }" } } }],
            "edges": []
        });
        let errs = validate(&ir, &catalog()).unwrap_err();
        assert!(errs.iter().any(|e| e.code == "ir.script_syntax"));
    }

    #[test]
    fn v7_too_many_nodes() {
        let nodes: Vec<_> = (0..201)
            .map(|i| json!({ "id": format!("n{i}"), "type": "storage.read", "params": {} }))
            .collect();
        let ir = json!({ "ir_version": 1, "name": "wf", "declared_scopes": ["storage.read"], "nodes": nodes, "edges": [] });
        let errs = validate(&ir, &catalog()).unwrap_err();
        assert!(errs.iter().any(|e| e.code == "ir.limit_exceeded"));
    }

    #[test]
    fn extract_host_variants() {
        assert_eq!(
            refs::extract_host("https://api.slack.com/x"),
            Some("api.slack.com".into())
        );
        assert_eq!(
            refs::extract_host("api.slack.com:443/x"),
            Some("api.slack.com".into())
        );
        assert_eq!(
            refs::extract_host("https://u@host.com/p"),
            Some("host.com".into())
        );
    }
}
