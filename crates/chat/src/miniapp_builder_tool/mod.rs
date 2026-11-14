//! 会話の AI がミニアプリを読む・作る・直す道具（D-10 の T08・GAP-32 のアプリ側）。
//!
//! - [`MiniappReadTool`]（`miniapp_read`）: 本文（画面・説明）・今の版・公開した版・束ねた部品の固定版と最新版を返す。
//!   本人が持ち主でないアプリの画面と説明は `<shared_artifact>` で包む（PIT-56）。
//! - [`MiniappCreateTool`]（`miniapp_create`）: 画面と束ねるワークフロー・保存ビューからアプリを作る。**確認なし**
//!   （HD-1 の改訂 (c)・版のある artifact で、保存は下書きの版になり、公開するまで使う人には出ない・D-16）。
//!   1 回の会話の run で作れるのは [`MAX_APPS_PER_RUN`] まで。名前が重なったら作成の冪等（R1）で照合する。
//! - [`MiniappUpdateTool`]（`miniapp_update`）: 自分が編集できるアプリを直す。**承認カードを経る**（オートでも残す・
//!   HD-1）。アプリは id で、版は `expected_version` で指定し、上げる部品の版は具体的な番号で書かせる（承認カードで
//!   見たものと保存するものを一致させる・TOCTOU）。
//!
//! どれも話した本人の `AuthContext` のまま gui のチョークポイント（[`MiniAppStore`]）と artifact のストアを通す
//! （昇格しない）。保存は [`SaveMode::Agent`] で、画面の書き損じと、部品とワークフローの入力の食い違いを誤りとして
//! 返す（T07）。画面の書き方は [`gui::miniapp_screen_guide`]（パーサーの表から生成）を説明文に載せる。
//!
//! [`SaveMode::Agent`]: gui::SaveMode::Agent

mod create;
mod outline;
mod pins;
mod read;
mod update;

use std::sync::Arc;

use agent_core::ToolOutcome;
use artifact::{Artifact, ArtifactError, ArtifactKind, ArtifactStore};
use authz::AuthContext;
use gui::{GuiError, GuiValidationError, MiniAppStore};
use uuid::Uuid;

pub use create::MiniappCreateTool;
pub use read::MiniappReadTool;
pub use update::MiniappUpdateTool;

use crate::builder_obs::{BuilderKind, BuilderRef};

/// 1 回の会話の run で作れるミニアプリの数。
pub const MAX_APPS_PER_RUN: i64 = 3;

/// 観測に載せる検証の誤りの数の上限（直せば次の保存で続きが出る）。
const MAX_ERRORS_SHOWN: usize = 20;

/// 道具が通るチョークポイント（どれも話した本人の権限で通す）。
#[derive(Clone)]
pub struct MiniappBuilderDeps {
    pub apps: Arc<MiniAppStore>,
    /// 名前の解決・作成の冪等の照合・作った数・部品の最新の版（artifact の単一チョークポイント）。
    pub artifacts: Arc<ArtifactStore>,
}

/// アプリが見つからない・読めないときの文言（無い・見えないを区別しない）。
const APP_NOT_FOUND: &str =
    "ミニアプリが見つからないか、見る権限がありません。名前か id を確かめてください。";

/// アプリの指定（id か名前）を、閲覧できるミニアプリへ。見えない・無い・種類が違うは同じ文言。
async fn resolve_app(
    artifacts: &ArtifactStore,
    ctx: &AuthContext,
    app: &str,
    trace_id: Option<&str>,
) -> Result<Artifact, ToolOutcome> {
    let a = app.trim();
    let found = match Uuid::parse_str(a) {
        Ok(id) => artifacts.get(ctx, id, trace_id).await,
        Err(_) => {
            artifacts
                .get_by_name(ctx, ArtifactKind::MiniApp, a, trace_id)
                .await
        }
    };
    match found {
        Ok(meta) if meta.kind == ArtifactKind::MiniApp => Ok(meta),
        Ok(_) | Err(ArtifactError::NotFound | ArtifactError::Forbidden) => {
            Err(ToolOutcome::error(APP_NOT_FOUND))
        }
        Err(e) => Err(internal(&e)),
    }
}

/// 内部の誤り（詳細はログにだけ出す）。
fn internal(err: &dyn std::fmt::Display) -> ToolOutcome {
    tracing::warn!(error = %err, "ミニアプリの道具が内部の誤りで止まった");
    ToolOutcome::error(
        "ミニアプリを扱えませんでした（内部の誤り）。少し待ってからやり直してください。",
    )
}

/// 検証の誤りの一覧（`- [code] path: message`・上限つき）。
fn format_errors(errors: &[GuiValidationError]) -> String {
    let mut out: Vec<String> = errors
        .iter()
        .take(MAX_ERRORS_SHOWN)
        .map(|e| match &e.path {
            Some(p) => format!("- [{}] {p}: {}", e.code, e.message),
            None => format!("- [{}] {}", e.code, e.message),
        })
        .collect();
    if errors.len() > MAX_ERRORS_SHOWN {
        out.push(format!(
            "- ほか {} 件（上を直して保存し直すと出ます）",
            errors.len() - MAX_ERRORS_SHOWN
        ));
    }
    out.join("\n")
}

/// 保存の誤りの観測（検証の誤りは全件・名前の重なりは呼ぶ側が先に扱う）。
fn save_failure(err: &GuiError, what: &str) -> ToolOutcome {
    match err {
        GuiError::Validation(errors) => ToolOutcome::error(format!(
            "ミニアプリを{what}できませんでした。次を直して、もう一度保存してください（保存はしていません）。\n{}",
            format_errors(errors)
        )),
        GuiError::Artifact(ArtifactError::NotFound | ArtifactError::Forbidden) => {
            ToolOutcome::error(
                "ミニアプリが見つからないか、編集の権限がありません。直せるのは編集できる人だけです。",
            )
        }
        GuiError::Artifact(ArtifactError::Invalid(m)) => {
            ToolOutcome::error(format!("ミニアプリを{what}できませんでした: {m}"))
        }
        GuiError::Artifact(e) => internal(e),
        GuiError::Internal(m) => internal(m),
    }
}

/// 保存したアプリの観測（下書き・開く場所・画面の要約・参照の行）。
fn saved(
    id: Uuid,
    name: &str,
    version: i64,
    published: Option<i64>,
    body: &gui::MiniAppBody,
    created: bool,
) -> ToolOutcome {
    let r = BuilderRef::new(BuilderKind::MiniApp, id, name, version);
    let publish = match published {
        None => "まだ公開していないので、使えるのは編集できる人だけです。共有した人に使ってもらうには、\
                 ユーザーがアプリの画面で「公開」を押します。"
            .to_string(),
        Some(p) => format!(
            "使う人に出ているのは公開中の v{p} のままです。この版を出すには、ユーザーがアプリの画面で「公開」を押します。"
        ),
    };
    let mut outcome =
        ToolOutcome::ok(format!(
        "ミニアプリ「{}」を{}（id: {id}, v{version}・下書き）。{publish}\n開く: /apps/{id}\n{}\n{}",
        crate::data_tool::one_line(name, 60),
        if created { "作りました" } else { "直しました" },
        outline::outline(body),
        r.line()
    ));
    outcome.builder_refs.push(r.to_value());
    outcome
}

/// 本人が持ち主でない artifact の文字列を `<shared_artifact>` で包む（閉じタグを中和する・PIT-56）。
fn wrap_shared(owner_is_me: bool, text: &str) -> String {
    if owner_is_me {
        return text.to_string();
    }
    format!(
        "<shared_artifact>{}</shared_artifact>",
        agent_core::tools::document_envelope::neutralize_tag(text, "shared_artifact")
    )
}

#[cfg(test)]
mod tests;
