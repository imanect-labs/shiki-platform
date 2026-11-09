//! IR 検証の補助チェック（V4 参照・スコープ天井整合・名前形式・V6 script）。行数ゲート対応で分離。

use super::{Catalog, ValidationError, MODEL_UNAVAILABLE_FOR_USER};
use crate::ir::WorkflowIr;
use crate::vocab::NodeType;

/// ワークフロー名の形式検証 `^[a-z][a-z0-9-]{0,63}$`（安定参照名・regex 不使用の手書き）。
pub(super) fn is_valid_workflow_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    if !bytes[0].is_ascii_lowercase() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// 能力ノードが必要とする宣言スコープ（scope 天井の保存時整合・engine.md §9.2）。
/// 対応の正は `vocab::required_scope`（単一定義）に委譲する。
pub(super) fn required_scope_for(nt: NodeType) -> Option<&'static str> {
    crate::vocab::required_scope(nt).map(crate::vocab::Scope::as_str)
}

/// `skill:<name>@<version>` 参照をパースする（形式不正は None・#344）。
pub(crate) fn parse_skill_ref(s: &str) -> Option<(&str, &str)> {
    let rest = s.strip_prefix("skill:")?;
    let (name, version) = rest.split_once('@')?;
    if name.is_empty() || version.is_empty() {
        return None;
    }
    Some((name, version))
}

/// instructions だけのスキル（[`Catalog::script_skills`] に無いもの）を呼ぶ skill.invoke は組織の既定のモデル（`model`）で
/// 動くが、そのモデルが保存する人のアカウントでは使えないときの誤り（D-185）。スキルにはモデルを選ぶ欄が無いので、
/// 管理者に相談するか、「AI に聞く」で使えるモデルを選んで同じ指示を書くよう言う（選べなければ管理者に相談だけ）。
fn skill_default_unavailable(model: &str, catalog: &Catalog) -> ValidationError {
    let models = catalog.models.as_deref().unwrap_or_default();
    let fix = if catalog.model_lock.is_some() || models.is_empty() {
        "スキルにはモデルを選ぶ欄がないため、管理者に相談してください".to_string()
    } else {
        format!(
            "スキルにはモデルを選ぶ欄がないため、管理者に相談するか、「AI に聞く」（llm.invoke）で使えるモデルを選び、\
             スキルと同じ指示を書いてください（{}）",
            super::v3_vocab::model_candidates(models)
        )
    };
    ValidationError::new(
        MODEL_UNAVAILABLE_FOR_USER,
        format!(
            "このスキルは組織の既定のモデル（{model}）で動きますが、このモデルはあなたのアカウントでは使えません。{fix}"
        ),
    )
}

/// workflow.start の相手が見つからない（無い・見えない・BUG-107）。
pub(super) const UNKNOWN_WORKFLOW: &str = "ir.unknown_workflow";

/// V4: 参照存在（secret の宛先束縛事前チェック＋skill のインストール済み照合・#344・workflow.start の相手）。
pub(super) fn v4_refs(ir: &WorkflowIr, catalog: &Catalog, errors: &mut Vec<ValidationError>) {
    for node in &ir.nodes {
        // workflow.start: 名前（リテラル）で指す相手が保存者に見えるか（無いと見えないは同じ誤り・BUG-107）。
        // 空の名前は必須の欄の空として readiness が扱う（置いたばかりでも保存できる・D-83）。値式は実行時に決まる。
        // 保存中のワークフロー自身の名前は見えるものとして扱う（自分を呼ぶ新しいワークフローを最初に保存できる）。
        if node.node_type == NodeType::WorkflowStart.as_str() {
            if let (Some(known), Some(name)) = (
                catalog.workflows.as_ref(),
                node.params.get("name").and_then(|v| v.as_str()),
            ) {
                if !name.trim().is_empty() && name != ir.name && !known.contains(name) {
                    errors.push(
                        ValidationError::new(
                            UNKNOWN_WORKFLOW,
                            crate::nodes::ports::hidden_not_found("実行するワークフロー"),
                        )
                        .at_node(&node.id)
                        .at_path("/params/name"),
                    );
                }
            }
            continue;
        }
        // skill.invoke: `skill:<name>@<version>` を保存ユーザーのインストール集合へ照合する
        // （10.1b。未インストール/存在しない skill を参照する IR は保存時に拒否）。
        if node.node_type == NodeType::SkillInvoke.as_str() {
            let Some(skill_ref) = node.params.get("skill").and_then(|v| v.as_str()) else {
                continue; // params 型は V1b が検証済み（欠落はそちらで拒否）。
            };
            // 空の参照は必須の欄の空として readiness が扱う（置いたばかりでも保存できる・D-83・BUG-151）。
            if skill_ref.trim().is_empty() {
                continue;
            }
            match parse_skill_ref(skill_ref) {
                None => {
                    errors.push(
                        ValidationError::new(
                            "ir.bad_ref",
                            format!("skill 参照は skill:<name>@<version> 形式です: {skill_ref}"),
                        )
                        .at_node(&node.id)
                        .at_path("/params/skill"),
                    );
                }
                Some((name, version)) => {
                    let known = catalog
                        .skills
                        .get(name)
                        .is_some_and(|versions| versions.contains(version));
                    if !known {
                        errors.push(
                            ValidationError::new(
                                "ir.unknown_skill",
                                format!(
                                    "skill {name}@{version} はインストールされていません\
                                     （レジストリからインストールしてください）"
                                ),
                            )
                            .at_node(&node.id)
                            .at_path("/params/skill"),
                        );
                    } else if let Some(model) = catalog.default_model_unavailable.as_deref() {
                        // shiki script の無いスキルは組織の既定のモデルで動く（実行の主体は制限しないので、保存で閉じる・
                        // D-185）。
                        if !catalog.script_skills.contains(&format!("{name}@{version}")) {
                            errors.push(
                                skill_default_unavailable(model, catalog)
                                    .at_node(&node.id)
                                    .at_path("/params/skill"),
                            );
                        }
                    }
                }
            }
            continue;
        }
        if node.node_type != NodeType::HttpRequest.as_str() {
            continue;
        }
        // http.request の secret 参照名を照合し、宛先束縛が URL ホストを許容するか事前検査。
        let Some(secret) = node.params.get("secret") else {
            continue;
        };
        let Some(name) = secret.get("name").and_then(|v| v.as_str()) else {
            errors.push(
                ValidationError::new("ir.bad_ref", "secret.name が必要です").at_node(&node.id),
            );
            continue;
        };
        let Some(allowed_hosts) = catalog.secrets.get(name) else {
            errors.push(
                ValidationError::new("ir.unknown_secret", format!("未知の secret: {name}"))
                    .at_node(&node.id),
            );
            continue;
        };
        // URL が空のうちは宛先を照合しない（実行と自動実行のオンの前に止まる・readiness・D-83）。入れた後の
        // 保存で照合する。
        if node
            .params
            .get("url")
            .and_then(|v| v.as_str())
            .is_some_and(|u| u.trim().is_empty())
        {
            continue;
        }
        // secret を添付する http.request は URL ホスト部が**リテラル必須**（ir.md §7.2）。
        // url が文字列でない（$from 等）とホストが確定できず宛先束縛を検査できない＝
        // 実行時任意ホストへ secret を添付され得るため、保存時に拒否する（fail-closed・P1）。
        let Some(host) = node
            .params
            .get("url")
            .and_then(|v| v.as_str())
            .and_then(extract_host)
        else {
            errors.push(
                ValidationError::new(
                    "ir.non_literal_url",
                    format!(
                        "secret を添付する http.request は URL ホストがリテラル必須です（node {}）",
                        node.id
                    ),
                )
                .at_node(&node.id),
            );
            continue;
        };
        let allowed = allowed_hosts
            .iter()
            .any(|pat| secrets::host_allowed(pat, &host));
        if !allowed {
            errors.push(
                ValidationError::new(
                    "ir.binding_denied",
                    format!("secret {name} は宛先 {host} への添付を許可していません"),
                )
                .at_node(&node.id),
            );
        }
    }
}

/// URL 文字列からホスト部を取り出す（リテラル URL 前提・スキーム有無を許容）。
pub(super) fn extract_host(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.split('@').next_back()?; // userinfo を除去
    let host = host.split(':').next()?; // ポートを除去
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

/// V6: inline script のコンパイル検証（禁止構文・swc パース）。
pub(super) fn v6_script(ir: &WorkflowIr, errors: &mut Vec<ValidationError>) {
    for node in &ir.nodes {
        if node.node_type != NodeType::ScriptRun.as_str() {
            continue;
        }
        let Some(source) = node
            .params
            .get("source")
            .and_then(|s| s.get("inline"))
            .and_then(|v| v.as_str())
        else {
            continue; // artifact 参照は存在検証のみ（Stage A は inline を検証）。
        };
        if let Err(e) = script_runtime::compile::compile(source) {
            errors.push(
                ValidationError::new("ir.script_syntax", format!("script コンパイルエラー: {e}"))
                    .at_node(&node.id),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn start_errors(ir_name: &str, target: &str, known: &[&str]) -> Vec<ValidationError> {
        let ir = WorkflowIr::from_json(&json!({
            "ir_version": 1, "name": ir_name, "declared_scopes": ["workflow.start"],
            "nodes": [{ "id": "s", "type": "workflow.start", "params": { "name": target } }],
            "edges": []
        }))
        .unwrap();
        let catalog = Catalog {
            workflows: Some(known.iter().map(|s| (*s).to_string()).collect()),
            ..Catalog::default()
        };
        let mut errors = Vec::new();
        v4_refs(&ir, &catalog, &mut errors);
        errors
    }

    fn skill_errors(catalog: &Catalog) -> Vec<ValidationError> {
        let ir = WorkflowIr::from_json(&json!({
            "ir_version": 1, "name": "wf",
            "nodes": [{ "id": "s", "type": "skill.invoke", "params": { "skill": "skill:memo@1.0.0" } }],
            "edges": []
        }))
        .unwrap();
        let mut errors = Vec::new();
        v4_refs(&ir, catalog, &mut errors);
        errors
    }

    /// instructions だけのスキルは組織の既定のモデルで動くので、既定が保存する人に使えなければ止める（D-185）。
    /// shiki script で動くスキル・既定が使える人・インストールしていないスキル（別の誤り）には足さない。
    #[test]
    fn a_skill_on_an_unavailable_default_model_is_refused() {
        let installed = Catalog {
            skills: [("memo".to_string(), ["1.0.0".to_string()].into())].into(),
            models: Some(vec!["visible".into()]),
            ..Catalog::default()
        };
        assert!(skill_errors(&installed).is_empty());
        let restricted = Catalog {
            default_model_unavailable: Some("hidden".into()),
            ..installed
        };
        let errs = skill_errors(&restricted);
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(errs[0].code, MODEL_UNAVAILABLE_FOR_USER);
        assert_eq!(errs[0].path.as_deref(), Some("/params/skill"));
        assert_eq!(
            errs[0].message,
            "このスキルは組織の既定のモデル（hidden）で動きますが、このモデルはあなたのアカウントでは使えません。\
             スキルにはモデルを選ぶ欄がないため、管理者に相談するか、「AI に聞く」（llm.invoke）で使えるモデルを選び、\
             スキルと同じ指示を書いてください（使えるモデル: visible）"
        );
        let locked = Catalog {
            model_lock: Some("hidden".into()),
            ..Catalog {
                skills: restricted.skills.clone(),
                models: restricted.models.clone(),
                default_model_unavailable: restricted.default_model_unavailable.clone(),
                ..Catalog::default()
            }
        };
        assert!(skill_errors(&locked)[0]
            .message
            .ends_with("スキルにはモデルを選ぶ欄がないため、管理者に相談してください"));
        let script = Catalog {
            script_skills: ["memo@1.0.0".to_string()].into(),
            ..restricted
        };
        assert!(skill_errors(&script).is_empty());
        let missing = Catalog {
            default_model_unavailable: Some("hidden".into()),
            ..Catalog::default()
        };
        let errs = skill_errors(&missing);
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(errs[0].code, "ir.unknown_skill");
    }

    /// 自分を呼ぶ新しいワークフロー（まだ保存者に見える一覧に無い）も、最初の保存で止めない。ほかの無い名前は止める。
    #[test]
    fn a_workflow_may_start_itself_before_it_is_saved() {
        assert!(start_errors("retry-loop", "retry-loop", &[]).is_empty());
        let errs = start_errors("retry-loop", "other-flow", &[]);
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(errs[0].code, UNKNOWN_WORKFLOW);
        assert!(start_errors("retry-loop", "other-flow", &["other-flow"]).is_empty());
    }
}
