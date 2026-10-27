//! V3: 語彙照合（ノード type・scope・event source・model・trigger scope 形状・Stage A available 集合へ）。

use super::{refs, Catalog, ValidationError};
use crate::ir::WorkflowIr;
use crate::vocab::{EventSource, NodeType, Scope};

/// llm.invoke のモデル欄の位置（右パネルのモデル欄に写す）。
const MODEL_PATH: &str = "/params/model";
/// エラーに添える使えるモデルの件数の上限（メッセージを膨らませない）。
pub(super) const MAX_MODEL_CANDIDATES: usize = 10;

/// 使えるモデルの一覧（上限まで・超えた分は件数だけ）。
fn model_candidates(models: &[String]) -> String {
    let shown = models
        .iter()
        .take(MAX_MODEL_CANDIDATES)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    match models.len().checked_sub(MAX_MODEL_CANDIDATES) {
        Some(rest) if rest > 0 => format!("使えるモデル: {shown} ほか {rest} 件"),
        _ => format!("使えるモデル: {shown}"),
    }
}

/// llm.invoke の model を組織の設定へ照合する（エラーはモデル欄に写るよう呼び出し側が path を付ける）。
///
/// - 省略・空文字は照合しない（実行のたびに組織の既定で解決する・BUG-12）。
/// - 組織が選択を禁じていれば、既定と違うモデルを拒否する（実行時の解決と同じ規則・D-30）。
/// - 組織が許可したモデルに無ければ拒否し、使えるモデルを添える（利用者も AI も候補から選び直せる）。
///   許可が 0 件の組織では、明示したモデルはすべて拒否する（照合を飛ばさない・BUG-37）。
/// - 判定用のモデル（[`Catalog::decision_models`]）は文章を生成できないので `ir.model_modality_mismatch`
///   （判定は ai.decide に書く・FEAT-1）。
pub(super) fn check_model(
    params: &serde_json::Value,
    catalog: &Catalog,
) -> Option<ValidationError> {
    let model = params
        .get("model")
        .and_then(serde_json::Value::as_str)
        .filter(|m| !m.is_empty())?;
    if catalog
        .model_lock
        .as_deref()
        .is_some_and(|lock| lock != model)
    {
        return Some(ValidationError::new(
            "ir.model_choice_disabled",
            format!(
                "この組織ではモデルを選べません: {model}（「組織の既定に従う」にして、model を書かないでください）"
            ),
        ));
    }
    let models = catalog.models.as_ref()?;
    if models.iter().any(|m| m == model) {
        return None;
    }
    if catalog
        .decision_models
        .as_ref()
        .is_some_and(|d| d.iter().any(|m| m == model))
    {
        return Some(ValidationError::new(
            "ir.model_modality_mismatch",
            format!(
                "{model} は判定用のモデルで、文章を生成できません。判定は「AI で判定」（ai.decide）に書くか、このノードには文章を生成するモデルを選んでください（{}）",
                model_candidates(models)
            ),
        ));
    }
    let candidates = if models.is_empty() {
        "組織で使えるモデルがありません".to_string()
    } else {
        model_candidates(models)
    };
    Some(ValidationError::new(
        "ir.unknown_model",
        format!("このモデルは使えません: {model}（{candidates}）"),
    ))
}

/// UUID の正規形（小文字・ハイフンつき）の文字列か（マッチャはテーブル id を文字列で照合する）。
pub(super) fn canonical_uuid(v: &serde_json::Value) -> bool {
    v.as_str()
        .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok_and(|u| u.hyphenated().to_string() == s))
}

/// イベントトリガの scope の形（source ごと・全購読は禁止・ir.md §6.2）。
///
/// - storage.write: `{ "folder": "<uuid>" }`（マッチャは folder キー以外を fail-closed で不一致にする）
/// - データ変更: `{ "table": "<uuid>" }`（正規形のリテラル・保存ビュー単位は v1 では入れない・GAP-07）
fn trigger_scope_error(
    source: Option<EventSource>,
    scope: &serde_json::Value,
) -> Option<ValidationError> {
    let only = |key: &str, canonical: bool| {
        scope.as_object().is_some_and(|o| {
            o.len() == 1
                && o.get(key).is_some_and(|v| {
                    if canonical {
                        canonical_uuid(v)
                    } else {
                        v.as_str().is_some_and(|s| uuid::Uuid::parse_str(s).is_ok())
                    }
                })
        })
    };
    match source {
        Some(s) if s.is_data() => (!only("table", true)).then(|| {
            ValidationError::new(
                "ir.bad_event_scope",
                format!(
                    "イベントトリガ（source {}）の scope は {{ \"table\": \"<テーブルの uuid（小文字）>\" }} が必要です（全購読は禁止）",
                    s.as_str()
                ),
            )
        }),
        Some(s) => (!only("folder", false)).then(|| {
            ValidationError::new(
                "ir.bad_event_scope",
                format!(
                    "イベントトリガ（source {}）の scope は {{ \"folder\": \"<uuid>\" }} が必要です（全購読は禁止）",
                    s.as_str()
                ),
            )
        }),
        None => None,
    }
}

/// wait(event) の source と scope（D-49・GAP-07）。
///
/// - source は閉集合のうち今のステージで有効なもの。
/// - storage.write: `{ "folder": "<uuid>" }` が必要（空の scope はテナント内のどの保存にも一致していた・
///   BUG-27。新しく保存するものは必須にし、保存済みのものは起床時の閲覧確認 `EventWaitGate` で守る）。
/// - データ変更: `{ "table": "<uuid（正規形）>", "record"?: 値式 }`（空の scope をワイルドカードにしない）。
///   data.read の宣言が要る（停止時に行を読み、変更は実行主体の見え方で受け取る）。
fn wait_event_errors(
    params: &serde_json::Value,
    declared: &std::collections::BTreeSet<&str>,
) -> Vec<ValidationError> {
    if params.get("kind").and_then(serde_json::Value::as_str) != Some("event") {
        return Vec::new();
    }
    let Some(raw) = params.get("source").and_then(serde_json::Value::as_str) else {
        return Vec::new(); // source の欠落は V1b（params の契約）が報告する。
    };
    let source = match EventSource::parse(raw) {
        Some(s) if s.available_stage_a() => s,
        _ => {
            return vec![ValidationError::new(
                "ir.unknown_event_source",
                format!("wait(event) の source {raw} は使えません"),
            )
            .at_path("/params/source")]
        }
    };
    let scope = params.get("scope").and_then(serde_json::Value::as_object);
    let mut out = Vec::new();
    let bad =
        |msg: String| ValidationError::new("ir.bad_event_scope", msg).at_path("/params/scope");
    if source.is_data() {
        let ok = scope.is_some_and(|o| {
            o.get("table").is_some_and(canonical_uuid)
                && o.keys().all(|k| k == "table" || k == "record")
        });
        if !ok {
            out.push(bad(format!(
                "wait(event)（source {raw}）の scope は {{ \"table\": \"<テーブルの uuid（小文字）>\", \"record\"?: 値式 }} が必要です"
            )));
        }
        if !declared.contains(Scope::DataRead.as_str()) {
            out.push(ValidationError::new(
                "ir.missing_scope",
                format!("wait(event)（source {raw}）は scope data.read を要しますが declared_scopes に未宣言です"),
            ));
        }
    } else {
        let ok = scope.is_some_and(|o| {
            o.len() == 1
                && o.get("folder")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|v| uuid::Uuid::parse_str(v).is_ok())
        });
        if !ok {
            out.push(bad(format!(
                "wait(event)（source {raw}）の scope は {{ \"folder\": \"<uuid>\" }} が必要です（待つフォルダを指定してください）"
            )));
        }
    }
    out
}

/// 警告のコード: 同じテーブルに「行が更新されたとき」と「状態が変わったとき」の両方のトリガがある。
pub const OVERLAPPING_DATA_TRIGGERS: &str = "ir.overlapping_data_triggers";

/// 警告のコード: wait(event) の filter で、到来したイベントを互換の別名（trigger / payload）で参照している。
pub const WAIT_FILTER_ALIAS: &str = "ir.wait_filter_alias";

/// 保存を止めない警告（イベント）:
///
/// - 状態の変更は行の更新でもあるので、同じテーブルに updated と transition の両方のトリガがあると、
///   1 回の遷移で run が 2 つできる（GAP-07）。
/// - wait(event) の filter の `$from: trigger` / `payload` は、実行の入力（きっかけ）ではなく到来した
///   イベントを指す（互換の別名）。`event` と書くよう促す（GAP-07 の T3）。
pub(super) fn event_warnings(ir: &WorkflowIr) -> Vec<ValidationError> {
    let mut out = overlapping_data_triggers(ir);
    for node in &ir.nodes {
        if node.node_type != NodeType::ControlWait.as_str() {
            continue;
        }
        if let Some(filter) = node.params.get("filter") {
            if refers_alias(filter) {
                out.push(
                    ValidationError::new(
                        WAIT_FILTER_ALIAS,
                        "待つできごとの条件で trigger / payload を使っています。ここでは届いたできごとを指します\
                         （実行のきっかけではありません）。{\"$from\":\"event\"} と書いてください",
                    )
                    .at_node(&node.id)
                    .at_path("/params/filter"),
                );
            }
        }
    }
    out
}

/// JSON の中に `$from: trigger` / `$from: payload` があるか。
fn refers_alias(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Object(m) => {
            matches!(
                m.get("$from").and_then(serde_json::Value::as_str),
                Some("trigger" | "payload")
            ) || m.values().any(refers_alias)
        }
        serde_json::Value::Array(a) => a.iter().any(refers_alias),
        _ => false,
    }
}

fn overlapping_data_triggers(ir: &WorkflowIr) -> Vec<ValidationError> {
    let table_of = |source: EventSource| {
        ir.triggers
            .iter()
            .enumerate()
            .filter_map(move |(i, t)| match t {
                crate::ir::Trigger::Event(ev) if EventSource::parse(&ev.source) == Some(source) => {
                    ev.scope
                        .get("table")
                        .and_then(serde_json::Value::as_str)
                        .map(|tb| (i, tb.to_string()))
                }
                _ => None,
            })
    };
    let updated: Vec<(usize, String)> = table_of(EventSource::DataRecordUpdated).collect();
    table_of(EventSource::DataTransition)
        .filter(|(_, tb)| updated.iter().any(|(_, u)| u == tb))
        .map(|(i, _)| {
            ValidationError::new(
                OVERLAPPING_DATA_TRIGGERS,
                "同じテーブルに「行が更新されたとき」と「状態が変わったとき」の両方のきっかけがあります。\
                 状態の変更は行の更新でもあるので、1 回の変更で 2 回実行されます",
            )
            .at_path(format!("/triggers/{i}"))
        })
        .collect()
}

/// V3: 語彙照合（Stage A available 集合へ）。
pub(super) fn v3_vocab(ir: &WorkflowIr, catalog: &Catalog, errors: &mut Vec<ValidationError>) {
    for scope in &ir.declared_scopes {
        match Scope::parse(scope) {
            Some(s) if s.available_stage_a() => {}
            Some(_) => errors.push(ValidationError::new(
                "ir.unknown_scope",
                format!("スコープ {scope} は Stage A では未対応です"),
            )),
            None => errors.push(ValidationError::new(
                "ir.unknown_scope",
                format!("未知のスコープ: {scope}"),
            )),
        }
    }
    let declared: std::collections::BTreeSet<&str> =
        ir.declared_scopes.iter().map(String::as_str).collect();
    for node in &ir.nodes {
        match NodeType::parse(&node.node_type) {
            // 予約語彙（将来ノード・issue #180）: 閉集合には含まれるが現ステージでは保存不可。
            Some(nt) if !nt.available_stage_a() => errors.push(
                ValidationError::new(
                    "ir.unknown_node_type",
                    format!(
                        "ノード種 {} は Stage A では未対応です（予約語彙）",
                        node.node_type
                    ),
                )
                .at_node(&node.id),
            ),
            Some(nt) => {
                // 能力ノードが必要とするスコープが declared_scopes に宣言されているか（宣言天井と
                // ノードの整合・保存時に弾く。実行時の scope 天井ゲートで初めて落ちるのを防ぐ）。
                if let Some(required) = refs::required_scope_for(nt) {
                    if !declared.contains(required) {
                        errors.push(
                            ValidationError::new(
                                "ir.missing_scope",
                                format!(
                                    "ノード {} は scope {required} を要しますが declared_scopes に未宣言です",
                                    node.id
                                ),
                            )
                            .at_node(&node.id),
                        );
                    }
                }
                if nt == NodeType::LlmInvoke {
                    if let Some(e) = check_model(&node.params, catalog) {
                        errors.push(e.at_node(&node.id).at_path(MODEL_PATH));
                    }
                }
                if nt == NodeType::ControlWait {
                    for e in wait_event_errors(&node.params, &declared) {
                        errors.push(e.at_node(&node.id));
                    }
                }
            }
            None => errors.push(
                ValidationError::new(
                    "ir.unknown_node_type",
                    format!("未知/未対応のノード種: {}", node.node_type),
                )
                .at_node(&node.id),
            ),
        }
    }
    // 承認は束縛した行を読む（data.read・GAP-08）。
    super::approval::v3(ir, &declared, errors);
    // イベント source を閉集合＋Stage A available へ照合。
    for (i, t) in ir.triggers.iter().enumerate() {
        match t {
            crate::ir::Trigger::Event(ev) => {
                let source = match EventSource::parse(&ev.source) {
                    Some(s) if s.available_stage_a() => Some(s),
                    Some(_) => {
                        errors.push(ValidationError::new(
                            "ir.unknown_event_source",
                            format!("イベント source {} は Stage A では未対応です", ev.source),
                        ));
                        None
                    }
                    None => {
                        errors.push(ValidationError::new(
                            "ir.unknown_event_source",
                            format!("未知のイベント source: {}", ev.source),
                        ));
                        None
                    }
                };
                if let Some(e) = trigger_scope_error(source, &ev.scope) {
                    errors.push(e.at_path(format!("/triggers/{i}/scope")));
                }
                // データ変更は委譲元の見え方で読む（data.read の宣言が要る・GAP-07）。
                if source.is_some_and(EventSource::is_data)
                    && !declared.contains(Scope::DataRead.as_str())
                {
                    errors.push(ValidationError::new(
                        "ir.missing_scope",
                        format!(
                            "イベントトリガ（source {}）は scope data.read を要しますが declared_scopes に未宣言です",
                            ev.source
                        ),
                    ));
                }
            }
            // schedule は cron（5 フィールド）＋IANA tz をパース検証する（実行時の発火不能を保存時に弾く）。
            crate::ir::Trigger::Schedule(sc) => {
                if let Err(e) = crate::scheduler::cron::validate(&sc.cron, &sc.tz) {
                    errors.push(ValidationError::new(
                        "ir.bad_schedule",
                        format!("スケジュールが不正です: {e}"),
                    ));
                }
            }
            crate::ir::Trigger::Interactive(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::{validate, Catalog, ValidationError};

    fn llm_ir(params: &serde_json::Value) -> serde_json::Value {
        json!({
            "ir_version": 1, "name": "wf", "triggers": [{ "kind": "interactive" }],
            "nodes": [{ "id": "ask", "type": "llm.invoke", "params": params }],
            "edges": []
        })
    }

    fn models(ids: &[&str]) -> Catalog {
        Catalog {
            models: Some(ids.iter().map(ToString::to_string).collect()),
            ..Catalog::default()
        }
    }

    fn model_errors(params: &serde_json::Value, catalog: &Catalog) -> Vec<ValidationError> {
        match validate(&llm_ir(params), catalog) {
            Ok(_) => Vec::new(),
            Err(errs) => errs
                .into_iter()
                .filter(|e| e.code.contains("model"))
                .collect(),
        }
    }

    #[test]
    fn unknown_model_points_at_the_model_field_and_lists_candidates() {
        let errs = model_errors(
            &json!({ "model": "gpt-x-mini", "prompt": "hi" }),
            &models(&["a", "b"]),
        );
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(errs[0].code, "ir.unknown_model");
        assert_eq!(errs[0].node_id.as_deref(), Some("ask"));
        assert_eq!(errs[0].path.as_deref(), Some("/params/model"));
        assert!(
            errs[0].message.contains("gpt-x-mini"),
            "{}",
            errs[0].message
        );
        assert!(
            errs[0].message.contains("使えるモデル: a, b"),
            "{}",
            errs[0].message
        );
    }

    /// model の省略と空文字は組織の既定に従うので、照合しない（BUG-12）。
    #[test]
    fn omitted_or_empty_model_follows_the_organization_default() {
        for catalog in [models(&["a"]), models(&[]), Catalog::default()] {
            assert!(model_errors(&json!({ "prompt": "hi" }), &catalog).is_empty());
            assert!(model_errors(&json!({ "model": "", "prompt": "hi" }), &catalog).is_empty());
        }
    }

    #[test]
    fn known_model_passes_and_an_unconfigured_llm_skips_matching() {
        assert!(model_errors(
            &json!({ "model": "a", "prompt": "hi" }),
            &models(&["a", "b"])
        )
        .is_empty());
        // LLM が未構成（照合先が無い）なら照合しない（従来どおり）。
        assert!(model_errors(
            &json!({ "model": "anything", "prompt": "hi" }),
            &Catalog::default()
        )
        .is_empty());
    }

    /// 組織が許可したモデルが 0 件なら、明示したモデルはすべて拒否する（照合を飛ばさない・BUG-37）。
    #[test]
    fn an_organization_without_models_rejects_every_explicit_model() {
        let errs = model_errors(&json!({ "model": "a", "prompt": "hi" }), &models(&[]));
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(errs[0].code, "ir.unknown_model");
        assert_eq!(errs[0].path.as_deref(), Some("/params/model"));
        assert!(
            errs[0].message.contains("組織で使えるモデルがありません"),
            "{}",
            errs[0].message
        );
    }

    /// 組織がモデルの選択を禁じていれば、既定と違うモデルを拒否する（既定と同じ・省略は通す・D-30）。
    #[test]
    fn a_locked_organization_rejects_models_other_than_its_default() {
        let locked = |lock: &str| Catalog {
            model_lock: Some(lock.into()),
            ..models(&["a", "b"])
        };
        let errs = model_errors(&json!({ "model": "b", "prompt": "hi" }), &locked("a"));
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(errs[0].code, "ir.model_choice_disabled");
        assert_eq!(errs[0].node_id.as_deref(), Some("ask"));
        assert_eq!(errs[0].path.as_deref(), Some("/params/model"));
        assert!(
            errs[0].message.contains("組織の既定に従う"),
            "{}",
            errs[0].message
        );
        assert!(model_errors(&json!({ "model": "a", "prompt": "hi" }), &locked("a")).is_empty());
        assert!(model_errors(&json!({ "prompt": "hi" }), &locked("a")).is_empty());
        // 既定が Auto の組織では、どのモデルも書けない。
        let errs = model_errors(&json!({ "model": "a", "prompt": "hi" }), &locked("auto"));
        assert_eq!(errs[0].code, "ir.model_choice_disabled");
        // Auto の文字列はモデルではない（実行時の解決と同じ）。
        let errs = model_errors(&json!({ "model": "auto", "prompt": "hi" }), &locked("auto"));
        assert_eq!(errs[0].code, "ir.unknown_model");
    }

    const TABLE: &str = "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1";

    fn trigger_errors(trigger: &serde_json::Value, scopes: &[&str]) -> Vec<ValidationError> {
        let ir = json!({
            "ir_version": 1, "name": "wf", "declared_scopes": scopes, "triggers": [trigger],
            "nodes": [{ "id": "log", "type": "control.join", "params": {} }], "edges": []
        });
        match validate(&ir, &Catalog::default()) {
            Ok(_) => Vec::new(),
            Err(errs) => errs,
        }
    }

    /// データ変更トリガは { table: <uuid> } に必須束縛し、data.read を宣言させる（GAP-07）。
    #[test]
    fn data_change_triggers_bind_a_table_and_need_data_read() {
        for source in [
            "data.record.created",
            "data.record.updated",
            "data.transition",
        ] {
            let ok = json!({ "kind": "event", "source": source, "scope": { "table": TABLE } });
            assert!(trigger_errors(&ok, &["data.read"]).is_empty(), "{source}");
            let missing = trigger_errors(&ok, &[]);
            assert!(
                missing.iter().any(|e| e.code == "ir.missing_scope"),
                "{source}: {missing:?}"
            );
            for scope in [
                json!({}),
                json!({ "folder": TABLE }),
                json!({ "table": "not-a-uuid" }),
                json!({ "table": TABLE.to_uppercase() }),
                json!({ "table": { "$from": "input", "path": "/t" } }),
                json!({ "table": TABLE, "view": TABLE }),
            ] {
                let t = json!({ "kind": "event", "source": source, "scope": scope });
                let errs = trigger_errors(&t, &["data.read"]);
                assert!(
                    errs.iter().any(|e| e.code == "ir.bad_event_scope"
                        && e.path.as_deref() == Some("/triggers/0/scope")),
                    "{source} {scope}: {errs:?}"
                );
            }
        }
        // ファイル保存のトリガは従来どおり folder（table では起動しない）。
        let t = json!({ "kind": "event", "source": "storage.write", "scope": { "table": TABLE } });
        assert!(trigger_errors(&t, &[])
            .iter()
            .any(|e| e.code == "ir.bad_event_scope"));
    }

    fn wait_errors(params: &serde_json::Value, scopes: &[&str]) -> Vec<ValidationError> {
        let ir = json!({
            "ir_version": 1, "name": "wf", "declared_scopes": scopes,
            "triggers": [{ "kind": "interactive" }],
            "nodes": [{ "id": "w", "type": "control.wait", "params": params }], "edges": []
        });
        match validate(&ir, &Catalog::default()) {
            Ok(_) => Vec::new(),
            Err(errs) => errs,
        }
    }

    /// 新しく保存する wait(event) は待つ範囲が必須（storage.write はフォルダ・データ変更はテーブル・D-49）。
    #[test]
    fn wait_event_requires_a_folder_or_a_table() {
        let folder =
            json!({ "kind": "event", "source": "storage.write", "scope": { "folder": TABLE } });
        assert!(wait_errors(&folder, &[]).is_empty());
        for scope in [json!({}), json!({ "table": TABLE })] {
            let p = json!({ "kind": "event", "source": "storage.write", "scope": scope });
            let errs = wait_errors(&p, &[]);
            assert!(
                errs.iter().any(|e| e.code == "ir.bad_event_scope"
                    && e.node_id.as_deref() == Some("w")
                    && e.path.as_deref() == Some("/params/scope")),
                "{scope}: {errs:?}"
            );
        }
        let no_scope = json!({ "kind": "event", "source": "storage.write" });
        assert!(wait_errors(&no_scope, &[])
            .iter()
            .any(|e| e.code == "ir.bad_event_scope"));

        let row = json!({ "kind": "event", "source": "data.record.updated",
            "scope": { "table": TABLE, "record": { "$from": "input", "path": "/id" } } });
        assert!(wait_errors(&row, &["data.read"]).is_empty());
        assert!(wait_errors(&row, &[])
            .iter()
            .any(|e| e.code == "ir.missing_scope"));
        for scope in [
            json!({}),
            json!({ "folder": TABLE }),
            json!({ "table": TABLE, "x": 1 }),
        ] {
            let p = json!({ "kind": "event", "source": "data.transition", "scope": scope });
            assert!(
                wait_errors(&p, &["data.read"])
                    .iter()
                    .any(|e| e.code == "ir.bad_event_scope"),
                "{scope}"
            );
        }
        let unknown = json!({ "kind": "event", "source": "webhook.received", "scope": {} });
        assert!(wait_errors(&unknown, &[])
            .iter()
            .any(|e| e.code == "ir.unknown_event_source"));
    }

    /// 同じテーブルの updated と transition の組は警告する（保存は止めない）。
    #[test]
    fn updated_and_transition_on_the_same_table_warn() {
        let ir = crate::ir::WorkflowIr::from_json(&json!({
            "ir_version": 1, "name": "wf", "declared_scopes": ["data.read"],
            "triggers": [
                { "kind": "event", "source": "data.record.updated", "scope": { "table": TABLE } },
                { "kind": "event", "source": "data.transition", "scope": { "table": TABLE } },
                { "kind": "event", "source": "data.transition",
                  "scope": { "table": "11111111-2222-3333-4444-555555555555" } }
            ],
            "nodes": [], "edges": []
        }))
        .unwrap();
        let w = super::event_warnings(&ir);
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!(w[0].code, super::OVERLAPPING_DATA_TRIGGERS);
        assert_eq!(w[0].path.as_deref(), Some("/triggers/1"));
    }

    /// wait(event) の filter の trigger / payload は別名として警告する（event は警告しない）。
    #[test]
    fn wait_filter_aliases_warn() {
        let ir_with = |from: &str| {
            crate::ir::WorkflowIr::from_json(&json!({
                "ir_version": 1, "name": "wf",
                "nodes": [{ "id": "w", "type": "control.wait", "params": {
                    "kind": "event", "source": "storage.write", "scope": { "folder": TABLE },
                    "filter": { "cmp": { "left": { "$from": from, "path": "/op" }, "op": "eq", "right": "create" } }
                } }],
                "edges": []
            }))
            .unwrap()
        };
        for alias in ["trigger", "payload"] {
            let w = super::event_warnings(&ir_with(alias));
            assert_eq!(w.len(), 1, "{alias}: {w:?}");
            assert_eq!(w[0].code, super::WAIT_FILTER_ALIAS);
            assert_eq!(w[0].node_id.as_deref(), Some("w"));
        }
        assert!(super::event_warnings(&ir_with("event")).is_empty());
    }

    #[test]
    fn candidates_are_capped_at_ten() {
        let ids: Vec<String> = (0..13).map(|i| format!("m{i}")).collect();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let errs = model_errors(&json!({ "model": "x", "prompt": "hi" }), &models(&refs));
        let msg = &errs[0].message;
        assert!(msg.contains("m0, m1") && msg.contains("m9"), "{msg}");
        assert!(!msg.contains("m10"), "11 件目以降は名前を出さない: {msg}");
        assert!(msg.contains("ほか 3 件"), "{msg}");
    }
}
