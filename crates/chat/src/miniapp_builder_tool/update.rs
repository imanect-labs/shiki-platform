//! `miniapp_update`: 自分が編集できるミニアプリを直す（承認カード・下書きの新しい版・D-10 の T08）。
//!
//! - **id と版で直す。** `app` に名前が来たら直さず、id と今の版を示して呼び直してもらう。`expected_version` は
//!   必須で、今の版と違えば直さない（承認カードで見た版の上にだけ積む・TOCTOU）。
//! - **省いた項目は今のまま。** `workflows` / `data_views` を書いたら、その一覧で入れ替える。今のピンにある部品で
//!   `version` を省いたら今の版を保ち、新しく束ねる部品は版の番号を書かせる（`"latest"` は受けない）。
//! - 保存は [`gui::MiniAppStore::update`] を [`gui::SaveMode::Agent`] で通す（編集できる人だけ・足した部品と版を
//!   変えた部品は本人の権限で確かめ直す・画面の書き損じと入力の食い違いは全件の誤り）。保存は下書きの版で、
//!   公開している版は変わらない（D-16）。

use std::collections::{BTreeSet, HashMap};

use agent_core::{Tool, ToolError, ToolName, ToolOutcome};
use artifact::{ArtifactError, ArtifactKind, ConflictReason};
use async_trait::async_trait;
use authz::AuthContext;
use gui::{GuiError, MiniAppBody, NamedComponentPin, SaveMode};
use serde_json::{json, Value};
use uuid::Uuid;

use super::pins::{
    read_pins, resolve_pins, DefaultVersion, PinInput, PinKind, DATA_VIEWS, WORKFLOWS,
};
use super::{internal, resolve_app, save_failure, saved, MiniappBuilderDeps, APP_NOT_FOUND};

/// ミニアプリを直す道具。
pub struct MiniappUpdateTool {
    deps: MiniappBuilderDeps,
    description: String,
}

impl MiniappUpdateTool {
    #[must_use]
    pub fn new(deps: MiniappBuilderDeps) -> Self {
        MiniappUpdateTool {
            deps,
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
        "自分が編集できるミニアプリを直す（画面・説明・束ねたワークフローと保存ビュー）。実行の前にユーザーが承認カードで\
         変更を確かめる。保存は下書きの新しい版で、公開中の版は変わらない（公開はユーザーが画面で行う）。直す前に \
         miniapp_read で今の版と本文を読む。\n\
         \n\
         入力:\n\
         - app: ミニアプリの id（名前ではなく id。miniapp_read・miniapp_create の「参照:」の行で分かる）\n\
         - expected_version: 読んだときの版（参照の行の version）。違えば直さない\n\
         - description・screen（省略可）: 書いたものだけ入れ替える（screen は画面の Markdown 全体）\n\
         - workflows・data_views（省略可）: 書いたら、その一覧で入れ替える [{{alias, workflow|view, version?}}]。\
         今束ねている部品で version を省くと今の版のまま。版を上げる・新しく束ねるときは版の番号を書く（latest は不可）\n\
         \n\
         - 保存できないときは、直す点を全部返す。直して保存し直す。\n\
         - 成功したら末尾の「参照:」の行に新しい版が出る。続けて直すときはその version を渡す。\n\
         \n\
         {}",
        gui::miniapp_screen_guide()
    )
}

/// 入力。
#[derive(Debug)]
struct UpdateInput {
    app: String,
    expected_version: i64,
    description: Option<String>,
    screen: Option<String>,
    workflows: Option<Vec<PinInput>>,
    data_views: Option<Vec<PinInput>>,
}

fn parse(input: &Value) -> Result<UpdateInput, String> {
    let Some(obj) = input.as_object() else {
        return Err("入力はオブジェクトにしてください。".into());
    };
    const KEYS: [&str; 6] = [
        "app",
        "expected_version",
        "description",
        "screen",
        "workflows",
        "data_views",
    ];
    if let Some(k) = obj.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!(
            "「{k}」は受けない入力です。使えるのは {} です（名前は変えられません）。",
            KEYS.join("・")
        ));
    }
    let Some(app) = obj
        .get("app")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Err("app（ミニアプリの id）を書いてください。".into());
    };
    let Some(expected_version) = obj.get("expected_version").and_then(Value::as_i64) else {
        return Err(
            "expected_version（読んだときの版・miniapp_read の参照の行の version）を書いてください。".into(),
        );
    };
    let text = |k: &str| obj.get(k).and_then(Value::as_str).map(str::to_string);
    let out = UpdateInput {
        app: app.to_string(),
        expected_version,
        description: text("description").map(|d| d.trim().to_string()),
        screen: text("screen"),
        workflows: read_pins(obj.get("workflows"), WORKFLOWS)?,
        data_views: read_pins(obj.get("data_views"), DATA_VIEWS)?,
    };
    if out.description.is_none()
        && out.screen.is_none()
        && out.workflows.is_none()
        && out.data_views.is_none()
    {
        return Err(
            "直す内容がありません。description・screen・workflows・data_views のどれかを書いてください。".into(),
        );
    }
    Ok(out)
}

#[async_trait]
impl Tool for MiniappUpdateTool {
    fn name(&self) -> &str {
        ToolName::MiniappUpdate.as_str()
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        let pin = |target: &str| {
            json!({
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "alias": { "type": "string" },
                        target: { "type": "string", "description": "id か名前" },
                        "version": { "type": "integer", "description": "版の番号（今束ねている部品は省くと今の版）" }
                    },
                    "required": ["alias", target]
                }
            })
        };
        json!({
            "type": "object",
            "properties": {
                "app": { "type": "string", "description": "ミニアプリの id（UUID）" },
                "expected_version": { "type": "integer", "description": "読んだときの版" },
                "description": { "type": "string" },
                "screen": { "type": "string", "description": "画面の Markdown 全体" },
                "workflows": pin("workflow"),
                "data_views": pin("view")
            },
            "required": ["app", "expected_version"]
        })
    }

    /// アプリの改訂は承認カードを経る（オートでも承認を残す・HD-1）。
    fn requires_confirmation(&self) -> bool {
        true
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
        // 名前なら直さず、id を示して呼び直してもらう（承認したアプリと直すアプリを一致させる）。
        let Ok(id) = Uuid::parse_str(&input.app) else {
            return Ok(match resolve_app(artifacts, ctx, &input.app, trace_id).await {
                Ok(meta) => ToolOutcome::error(format!(
                    "app にはミニアプリの id を入れてください。「{}」の id は {}（v{}）です。直す内容はそのままで、\
                     app に id を入れて呼び直してください。",
                    crate::data_tool::one_line(&meta.name, 60),
                    meta.id,
                    meta.current_version
                )),
                Err(o) => o,
            });
        };
        let meta = match artifacts.get(ctx, id, trace_id).await {
            Ok(m) if m.kind == ArtifactKind::MiniApp => m,
            Ok(_) | Err(ArtifactError::NotFound | ArtifactError::Forbidden) => {
                return Ok(ToolOutcome::error(APP_NOT_FOUND))
            }
            Err(e) => return Ok(internal(&e)),
        };
        // 直せるのは編集できる人だけ（保存の入口も確かめる。ここでは版の比べより先に、分かる言葉で断る）。
        match artifacts.allows(ctx, id, authz::Relation::Editor).await {
            Ok(true) => {}
            Ok(false) => {
                return Ok(ToolOutcome::error(
                    "このミニアプリを編集する権限がありません。直せるのは編集できる人だけです。",
                ))
            }
            Err(e) => return Ok(internal(&e)),
        }
        if meta.current_version != input.expected_version {
            return Ok(version_changed(
                &meta.name,
                meta.current_version,
                input.expected_version,
            ));
        }
        let current = match self.load(ctx, id, input.expected_version, trace_id).await {
            Ok(b) => b,
            Err(o) => return Ok(o),
        };
        let workflows = match self
            .pins(
                ctx,
                WORKFLOWS,
                input.workflows.as_deref(),
                &current.workflows,
                trace_id,
            )
            .await
        {
            Ok(p) => p,
            Err(msg) => return Ok(ToolOutcome::error(msg)),
        };
        let data_views = match self
            .pins(
                ctx,
                DATA_VIEWS,
                input.data_views.as_deref(),
                &current.data_views,
                trace_id,
            )
            .await
        {
            Ok(p) => p,
            Err(msg) => return Ok(ToolOutcome::error(msg)),
        };
        let next = MiniAppBody {
            description: input
                .description
                .unwrap_or_else(|| current.description.clone()),
            screen: input.screen.unwrap_or_else(|| current.screen.clone()),
            skill: current.skill,
            workflows,
            data_views,
        };
        let changes = changes(&current, &next);
        if changes.is_empty() {
            return Ok(ToolOutcome::error(format!(
                "今の版（v{}）と同じ内容なので、保存していません。",
                meta.current_version
            )));
        }
        let raw = match serde_json::to_value(&next) {
            Ok(v) => v,
            Err(e) => return Ok(internal(&e)),
        };
        match self
            .deps
            .apps
            .update(
                ctx,
                id,
                &raw,
                Some(input.expected_version),
                SaveMode::Agent,
                trace_id,
            )
            .await
        {
            Ok(s) => {
                let mut o = saved(
                    id,
                    &meta.name,
                    s.version,
                    s.published_version,
                    &s.body,
                    false,
                );
                o.content = format!("{}\n変えたこと: {}", o.content, changes.join(" / "));
                Ok(o)
            }
            Err(GuiError::Artifact(ArtifactError::Conflict(ConflictReason::VersionChanged {
                current,
                ..
            }))) => Ok(version_changed(&meta.name, current, input.expected_version)),
            Err(e) => Ok(save_failure(&e, "保存")),
        }
    }
}

impl MiniappUpdateTool {
    /// 直す元の版の本文（今の検証を通るもの）。
    async fn load(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: i64,
        trace_id: Option<&str>,
    ) -> Result<MiniAppBody, ToolOutcome> {
        match self
            .deps
            .artifacts
            .get_version(ctx, id, version, trace_id)
            .await
            .map(|v| gui::miniapp::validate_miniapp_body(&v.body))
        {
            Ok(Ok(b)) => Ok(b),
            // 今の版が今の検証を通らない（古い形）なら、直す元にできない（人が画面で直す）。
            Ok(Err(_)) => Err(ToolOutcome::error(
                "このミニアプリの今の版は古い形で読めないので、ここでは直せません。ユーザーにアプリの画面で直してもらってください。",
            )),
            Err(
                ArtifactError::NotFound | ArtifactError::Forbidden | ArtifactError::NotPublished { .. },
            ) => Err(ToolOutcome::error(APP_NOT_FOUND)),
            Err(e) => Err(internal(&e)),
        }
    }

    /// 部品の一覧（書いていなければ今のまま・書いたら本人の権限で解決し直す）。
    async fn pins(
        &self,
        ctx: &AuthContext,
        k: PinKind,
        input: Option<&[PinInput]>,
        current: &[NamedComponentPin],
        trace_id: Option<&str>,
    ) -> Result<Vec<NamedComponentPin>, String> {
        let Some(input) = input else {
            return Ok(current.to_vec());
        };
        let versions: HashMap<Uuid, i64> =
            current.iter().map(|p| (p.artifact_id, p.version)).collect();
        let resolved = resolve_pins(
            &self.deps.artifacts,
            ctx,
            k,
            input,
            &DefaultVersion::Current(&versions),
            trace_id,
        )
        .await?;
        Ok(resolved
            .into_iter()
            .map(|p| NamedComponentPin {
                alias: p.alias,
                artifact_id: p.id,
                version: p.version,
            })
            .collect())
    }
}

fn version_changed(name: &str, current: i64, expected: i64) -> ToolOutcome {
    ToolOutcome::error(format!(
        "ミニアプリ「{}」は v{current} になっています（渡した版は v{expected}）。ほかの変更が入っています。miniapp_read で\
         読み直してから、expected_version に {current} を渡して直してください。",
        crate::data_tool::one_line(name, 60)
    ))
}

/// 前の版から変えたこと（観測に出す・部品は別名と版で言う）。
fn changes(before: &MiniAppBody, after: &MiniAppBody) -> Vec<String> {
    let mut out = Vec::new();
    if before.description != after.description {
        out.push("説明を変えた".to_string());
    }
    if before.screen != after.screen {
        out.push("画面を変えた".to_string());
    }
    for (noun, b, a) in [
        ("ワークフロー", &before.workflows, &after.workflows),
        ("保存ビュー", &before.data_views, &after.data_views),
    ] {
        let key = |p: &NamedComponentPin| (p.alias.clone(), p.artifact_id, p.version);
        let bs: BTreeSet<_> = b.iter().map(key).collect();
        let as_: BTreeSet<_> = a.iter().map(key).collect();
        for (alias, id, v) in as_.difference(&bs) {
            match b.iter().find(|p| &p.artifact_id == id) {
                Some(old) if old.alias == *alias => {
                    out.push(format!("{noun}「{alias}」を v{} → v{v}", old.version));
                }
                _ => out.push(format!("{noun}「{alias}」を束ねた（v{v}）")),
            }
        }
        for (alias, id, _) in bs.difference(&as_) {
            if !a.iter().any(|p| &p.artifact_id == id && &p.alias == alias) {
                out.push(format!("{noun}「{alias}」を外した"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(alias: &str, n: u128, v: i64) -> NamedComponentPin {
        NamedComponentPin {
            alias: alias.into(),
            artifact_id: Uuid::from_u128(n),
            version: v,
        }
    }

    #[test]
    fn changes_name_versions_and_bundles() {
        let before = MiniAppBody {
            description: "d".into(),
            screen: "s".into(),
            skill: None,
            workflows: vec![pin("申請", 1, 1), pin("集計", 2, 1)],
            data_views: vec![pin("一覧", 3, 2)],
        };
        let after = MiniAppBody {
            screen: "s2".into(),
            workflows: vec![pin("申請", 1, 3), pin("通知", 4, 1)],
            ..before.clone()
        };
        assert_eq!(
            changes(&before, &after),
            vec![
                "画面を変えた",
                "ワークフロー「申請」を v1 → v3",
                "ワークフロー「通知」を束ねた（v1）",
                "ワークフロー「集計」を外した",
            ]
        );
        assert!(changes(&before, &before).is_empty());
    }

    #[test]
    fn parse_requires_id_version_and_some_change() {
        assert!(parse(&json!({ "app": "x" }))
            .unwrap_err()
            .contains("expected_version"));
        assert!(parse(&json!({ "app": "x", "expected_version": 1 }))
            .unwrap_err()
            .contains("直す内容がありません"));
        assert!(
            parse(&json!({ "app": "x", "expected_version": 1, "name": "n" }))
                .unwrap_err()
                .contains("名前は変えられません")
        );
        let ok = parse(&json!({ "app": "x", "expected_version": 2, "screen": "s" })).unwrap();
        assert_eq!((ok.expected_version, ok.screen.as_deref()), (2, Some("s")));
    }
}
