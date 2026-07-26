//! IR 検証の補助チェック（V4 参照・スコープ天井整合・名前形式・V6 script）。500 行ゲート対応で分離。

use super::{Catalog, ValidationError};
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

/// V4: 参照存在（secret の宛先束縛事前チェック＋skill のインストール済み照合・#344）。
pub(super) fn v4_refs(ir: &WorkflowIr, catalog: &Catalog, errors: &mut Vec<ValidationError>) {
    for node in &ir.nodes {
        // skill.invoke: `skill:<name>@<version>` を保存ユーザーのインストール集合へ照合する
        // （10.1b。未インストール/存在しない skill を参照する IR は保存時に拒否）。
        if node.node_type == NodeType::SkillInvoke.as_str() {
            let Some(skill_ref) = node.params.get("skill").and_then(|v| v.as_str()) else {
                continue; // params 型は V1b が検証済み（欠落はそちらで拒否）。
            };
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
