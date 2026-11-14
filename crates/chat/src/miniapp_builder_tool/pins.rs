//! 束ねる部品（ワークフロー・保存ビュー）の入力を読み、本人の権限で id と版へ解決する。
//!
//! - 部品は id（UUID）か名前で受ける。見えない・無い・種類が違うは同じ文言（存在を明かさない）。
//! - 版は整数だけを受ける（`"latest"` などの文字は断る・承認カードで見た版と保存する版を一致させる）。省いたときの
//!   版は呼ぶ側が決める（作るときは最新・直すときは今のピンの版）。
//! - 解決はここで行い、viewer・kind・版の実在の確かめは保存のとき [`gui::MiniAppStore`] の `validate_pins` が
//!   もう一度行う（ここは名前を id にするだけで、認可の判断をここに置かない）。

use std::collections::HashMap;

use artifact::{ArtifactError, ArtifactKind, ArtifactStore};
use authz::AuthContext;
use serde_json::{json, Value};
use uuid::Uuid;

/// 部品の種類ごとの語（入力のキー・観測の名詞）。
#[derive(Debug, Clone, Copy)]
pub(super) struct PinKind {
    /// 本文のフィールド（`workflows`・`data_views`）。
    pub(super) field: &'static str,
    /// 1 件の中の参照のキー（`workflow`・`view`）。
    pub(super) target: &'static str,
    pub(super) kind: ArtifactKind,
    pub(super) noun: &'static str,
}

pub(super) const WORKFLOWS: PinKind = PinKind {
    field: "workflows",
    target: "workflow",
    kind: ArtifactKind::Workflow,
    noun: "ワークフロー",
};

pub(super) const DATA_VIEWS: PinKind = PinKind {
    field: "data_views",
    target: "view",
    kind: ArtifactKind::DataView,
    noun: "保存ビュー",
};

/// 入力の部品 1 つ（解決の前）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PinInput {
    pub(super) alias: String,
    /// id（UUID）か名前。
    pub(super) target: String,
    pub(super) version: Option<i64>,
}

/// `workflows` / `data_views` の入力を読む（無い・null は `None`）。
pub(super) fn read_pins(
    input: Option<&Value>,
    k: PinKind,
) -> Result<Option<Vec<PinInput>>, String> {
    let Some(v) = input.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let Some(items) = v.as_array() else {
        return Err(format!(
            "{} は [{{alias, {}, version?}}] の配列にしてください。",
            k.field, k.target
        ));
    };
    let mut out = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let Some(obj) = item.as_object() else {
            return Err(format!("{}[{i}] はオブジェクトにしてください。", k.field));
        };
        if let Some(extra) = obj
            .keys()
            .find(|key| !["alias", k.target, "version"].contains(&key.as_str()))
        {
            return Err(format!(
                "{}[{i}] の「{extra}」は受けない入力です（alias・{}・version）。",
                k.field, k.target
            ));
        }
        let text = |key: &str| obj.get(key).and_then(Value::as_str).map(str::trim);
        let Some(alias) = obj.get("alias").and_then(Value::as_str) else {
            return Err(format!(
                "{}[{i}].alias（画面で使う別名）を書いてください。",
                k.field
            ));
        };
        let Some(target) = text(k.target).filter(|s| !s.is_empty()) else {
            return Err(format!(
                "{}[{i}].{}（{}の id か名前）を書いてください。",
                k.field, k.target, k.noun
            ));
        };
        let version = match obj.get("version") {
            None | Some(Value::Null) => None,
            Some(v) => match v.as_i64() {
                Some(n) => Some(n),
                None => {
                    return Err(format!(
                        "{}[{i}].version は版の番号（整数）で書いてください（「latest」などは受けません。\
                         最新の版は miniapp_read・list_workflows で分かります）。",
                        k.field
                    ))
                }
            },
        };
        out.push(PinInput {
            // 別名の前後の空白は gui の検証が断る（ここで黙って直さない）。
            alias: alias.to_string(),
            target: target.to_string(),
            version,
        });
    }
    Ok(Some(out))
}

/// 解決した部品（本文に入れる形と、観測に出す名前）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedPin {
    pub(super) alias: String,
    pub(super) id: Uuid,
    pub(super) version: i64,
    pub(super) name: String,
}

impl ResolvedPin {
    pub(super) fn to_value(&self) -> Value {
        json!({ "alias": self.alias, "artifact_id": self.id, "version": self.version })
    }
}

/// 版を省いたときの決め方。
pub(super) enum DefaultVersion<'a> {
    /// 本人に見える最新の版（作るとき）。
    Latest,
    /// 今のピンの版（直すとき）。今のピンに無い部品は版を書かせる。
    Current(&'a HashMap<Uuid, i64>),
}

/// 部品を本人の権限で id と版へ解決する。誤りは AI 向けの文（全件ではなく最初の 1 件）。
pub(super) async fn resolve_pins(
    artifacts: &ArtifactStore,
    ctx: &AuthContext,
    k: PinKind,
    pins: &[PinInput],
    default: &DefaultVersion<'_>,
    trace_id: Option<&str>,
) -> Result<Vec<ResolvedPin>, String> {
    let mut out = Vec::with_capacity(pins.len());
    for (i, p) in pins.iter().enumerate() {
        let found = match Uuid::parse_str(&p.target) {
            Ok(id) => artifacts.get(ctx, id, trace_id).await,
            Err(_) => {
                artifacts
                    .get_by_name(ctx, k.kind, &p.target, trace_id)
                    .await
            }
        };
        let meta = match found {
            Ok(m) if m.kind == k.kind => m,
            Ok(_) | Err(ArtifactError::NotFound | ArtifactError::Forbidden) => {
                return Err(format!(
                    "{}[{i}]: 「{}」という{}が見つからないか、見る権限がありません。",
                    k.field,
                    crate::data_tool::one_line(&p.target, 60),
                    k.noun
                ));
            }
            Err(e) => {
                tracing::warn!(error = %e, "束ねる部品を解決できない");
                return Err(format!("{}[{i}]: 読み取りに失敗しました。", k.field));
            }
        };
        let version = match (p.version, default) {
            (Some(v), _) => v,
            (None, DefaultVersion::Latest) => meta.current_version,
            (None, DefaultVersion::Current(current)) => match current.get(&meta.id) {
                Some(v) => *v,
                None => {
                    return Err(format!(
                        "{}[{i}]: 新しく束ねる{}「{}」は version（版の番号）を書いてください（最新は v{}）。",
                        k.field,
                        k.noun,
                        crate::data_tool::one_line(&meta.name, 60),
                        meta.current_version
                    ))
                }
            },
        };
        out.push(ResolvedPin {
            alias: p.alias.clone(),
            id: meta.id,
            version,
            name: meta.name,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_pins_and_rejects_words_for_versions() {
        let v = json!([{ "alias": "申請", "workflow": " expense ", "version": 3 }, { "alias": "b", "workflow": "x" }]);
        assert_eq!(
            read_pins(Some(&v), WORKFLOWS).unwrap().unwrap(),
            vec![
                PinInput {
                    alias: "申請".into(),
                    target: "expense".into(),
                    version: Some(3)
                },
                PinInput {
                    alias: "b".into(),
                    target: "x".into(),
                    version: None
                },
            ]
        );
        assert_eq!(read_pins(None, WORKFLOWS).unwrap(), None);
        assert_eq!(read_pins(Some(&Value::Null), DATA_VIEWS).unwrap(), None);
        let latest = json!([{ "alias": "a", "workflow": "x", "version": "latest" }]);
        assert!(read_pins(Some(&latest), WORKFLOWS)
            .unwrap_err()
            .contains("整数"));
        let wrong_key = json!([{ "alias": "a", "workflow": "x" }]);
        assert!(read_pins(Some(&wrong_key), DATA_VIEWS)
            .unwrap_err()
            .contains("「workflow」は受けない"));
        let no_target = json!([{ "alias": "a" }]);
        assert!(read_pins(Some(&no_target), DATA_VIEWS)
            .unwrap_err()
            .contains("view"));
        assert!(read_pins(Some(&json!({})), WORKFLOWS).is_err());
    }
}
