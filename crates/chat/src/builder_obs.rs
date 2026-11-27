//! 作る道具（テーブル・保存ビュー・ミニアプリ・ワークフロー）の観測の共通部品（T02・D-204）。
//!
//! - [`BuilderRef`]: 作ったもの・直したものへの参照 `{kind, id, name, version}`。道具は
//!   [`BuilderRef::to_value`] を `ToolOutcome.builder_refs` に入れ、`AgentEvent::BuilderRef` →
//!   `StreamEventKind::BuilderRef` → `ContentBlock::BuilderRef` を通って参照カードになる。種類の語彙
//!   （[`BuilderKind`]）はここが単一の定義で、OpenAPI から画面の型へ生成する。
//! - [`ref_line`]: 観測の末尾に置く参照の行（`参照: {...}`）。こちらが作った値（種類・id・版）と、改行と制御文字を
//!   潰して切った名前だけで組む（実 LLM が id を取り違えないように・stub のシナリオもこの行から id を引く）。
//! - [`on_name_taken`]: 作成が名前の重なりで断られたときに、本人がこの会話の run の間に作った同名と内容を比べる
//!   （作成の冪等・R1）。同じなら作成済みとして返し（版は動かさない）、違えば保存しない。
//!
//! 封筒（PIT-56）の中和は `agent_core::tools::document_envelope::neutralize_tag` が単一の定義で、ここには置かない。

use agent_core::ToolOutcome;
use artifact::{ArtifactError, ArtifactKind, ArtifactStore};
use authz::AuthContext;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// 参照に載せる名前の上限（文字）。artifact の名前の上限（128）と同じ。
const MAX_NAME_CHARS: usize = 128;

/// 作る道具が作るものの種類（参照カードの種類・D-204）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BuilderKind {
    /// ワークフロー（artifact kind=workflow）。
    Workflow,
    /// 構造化データのテーブル（`data_table`）。
    DataTable,
    /// 構造化データの保存ビュー（artifact kind=data_view）。
    DataView,
    /// ミニアプリ（artifact kind=mini_app）。
    MiniApp,
}

impl BuilderKind {
    /// 観測と履歴に書く名前（利用者の言葉）。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            BuilderKind::Workflow => "ワークフロー",
            BuilderKind::DataTable => "テーブル",
            BuilderKind::DataView => "保存ビュー",
            BuilderKind::MiniApp => "ミニアプリ",
        }
    }

    /// artifact として保存する種類なら、その種類（テーブルは artifact ではない）。
    #[must_use]
    pub const fn artifact_kind(self) -> Option<ArtifactKind> {
        match self {
            BuilderKind::Workflow => Some(ArtifactKind::Workflow),
            BuilderKind::DataView => Some(ArtifactKind::DataView),
            BuilderKind::MiniApp => Some(ArtifactKind::MiniApp),
            BuilderKind::DataTable => None,
        }
    }
}

/// 作ったもの・直したものへの参照（参照カードの中身・`builder_ref` ブロックの `item`）。
///
/// **保存に成功し、発話ユーザーが読めるものだけ**を指す。ドライブのノードは指さない（`document_ref` の役目）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BuilderRef {
    pub kind: BuilderKind,
    pub id: Uuid,
    /// 参照名（改行と制御文字を潰し、上限で切ったもの）。
    pub name: String,
    /// 版（ワークフロー・保存ビュー・ミニアプリは artifact の版、テーブルはスキーマの版）。
    pub version: i64,
}

impl BuilderRef {
    /// 参照を組む（名前は 1 行にして上限で切る）。
    #[must_use]
    pub fn new(kind: BuilderKind, id: Uuid, name: &str, version: i64) -> Self {
        BuilderRef {
            kind,
            id,
            name: one_line(name),
            version,
        }
    }

    /// `ToolOutcome.builder_refs` に入れる JSON。
    #[must_use]
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": self.kind,
            "id": self.id,
            "name": self.name,
            "version": self.version,
        })
    }

    /// `builder_ref` ブロックの JSON を読む（形が違えば `None`）。
    #[must_use]
    pub fn from_value(v: &serde_json::Value) -> Option<Self> {
        serde_json::from_value(v.clone()).ok()
    }

    /// 観測の末尾に置く参照の行（[`ref_line`]）。
    #[must_use]
    pub fn line(&self) -> String {
        format!("参照: {}", self.to_value())
    }

    /// 次のターンの履歴に残す 1 行（`[作成済みテーブル: 顧客（id: …, v1）]`・続きの依頼で id と版を引かせる）。
    #[must_use]
    pub fn history_line(&self) -> String {
        format!(
            "[作成済み{}: {}（id: {}, v{}）]",
            self.kind.label(),
            self.name,
            self.id,
            self.version
        )
    }
}

/// 観測の末尾に置く参照の行（`参照: {"kind":…,"id":…,"name":…,"version":…}`）。
///
/// 値は JSON の文字列として書く（名前の引用符・改行で行を崩させない）。名前は改行と制御文字を潰して切る。
#[must_use]
pub fn ref_line(kind: BuilderKind, id: Uuid, name: &str, version: i64) -> String {
    BuilderRef::new(kind, id, name, version).line()
}

/// 名前を 1 行にして上限で切る（制御文字と行・段落の区切り〔U+2028・U+2029〕を空白にする）。
fn one_line(s: &str) -> String {
    let mut out: String = s
        .chars()
        .take(MAX_NAME_CHARS)
        .map(|c| {
            if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') {
                ' '
            } else {
                c
            }
        })
        .collect();
    if s.chars().count() > MAX_NAME_CHARS {
        out.push('…');
    }
    out
}

/// 作成が名前の重なり（NameTaken）で断られたときの照合の結果（[`on_name_taken`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameTaken {
    /// 本人がこの run の間に、同じ名前・同じ内容のものを作っている（作成済みとして返す・版は動かさない）。
    Same(BuilderRef),
    /// 本人がこの run の間に同じ名前のものを作っているが、今の版の内容が違う（保存しない）。
    Differs(BuilderRef),
    /// 本人がこの run の間に作った同名は無い（ほかの人のもの・前からあるもの・見えないもの）。
    Other,
}

/// 作成が名前の重なりで断られたとき、本人が `since`（会話の run が作られた時刻）以降に作った同名と比べる（R1）。
///
/// 引くのは [`ArtifactStore::find_own_created_since`]（本人が持ち主で、`since` 以降に作られ、消していないもの。
/// ReBAC の viewer も確かめる）。見つかれば今の版の本文と `body`（今回の本文に、保存と同じ正規化を通したもの）を
/// JSON の値として比べる。種類が artifact でないもの（テーブル）は、ここでは比べない（[`NameTaken::Other`]）。
///
/// 他人の同名・前からある同名は [`NameTaken::Other`] で返し、持ち主・id・作成時刻を呼ぶ側に渡さない。
pub async fn on_name_taken(
    artifacts: &ArtifactStore,
    ctx: &AuthContext,
    kind: BuilderKind,
    name: &str,
    since: DateTime<Utc>,
    body: &serde_json::Value,
    trace_id: Option<&str>,
) -> Result<NameTaken, ArtifactError> {
    let Some(artifact_kind) = kind.artifact_kind() else {
        return Ok(NameTaken::Other);
    };
    let Some(found) = artifacts
        .find_own_created_since(ctx, artifact_kind, name, since, trace_id)
        .await?
    else {
        return Ok(NameTaken::Other);
    };
    let current = artifacts
        .get_version(ctx, found.id, found.current_version, trace_id)
        .await?;
    let r = BuilderRef::new(kind, found.id, &found.name, current.version);
    Ok(if &current.body == body {
        NameTaken::Same(r)
    } else {
        NameTaken::Differs(r)
    })
}

/// 同じ名前・同じ内容のものを本人がすでに作っていたときの観測（成功・版は動かしていない）。参照の行と
/// 参照カードを付ける。
#[must_use]
pub fn already_created(r: &BuilderRef) -> ToolOutcome {
    let label = r.kind.label();
    let mut outcome = ToolOutcome::ok(format!(
        "同じ名前・同じ内容の{label}を、あなたがすでに作っています。新しい版は作っていません（id: {}, v{}）。\n{}",
        r.id,
        r.version,
        r.line()
    ));
    outcome.builder_refs.push(r.to_value());
    outcome
}

/// 本人がすでに作った同名と内容が違ったときの観測（誤り・今回の内容は保存していない）。`how_to_fix` は種類ごとの
/// 直し方（id と版を渡して保存し直す手順）。
#[must_use]
pub fn created_differently(r: &BuilderRef, how_to_fix: &str) -> ToolOutcome {
    let label = r.kind.label();
    ToolOutcome::error(format!(
        "name が「{}」の{label}は、あなたがすでに作っています（id: {}, v{}）。今回の内容は保存していません。{how_to_fix}",
        r.name, r.id, r.version
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_are_snake_case_on_the_wire() {
        for (kind, wire) in [
            (BuilderKind::Workflow, "workflow"),
            (BuilderKind::DataTable, "data_table"),
            (BuilderKind::DataView, "data_view"),
            (BuilderKind::MiniApp, "mini_app"),
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), wire);
        }
        assert_eq!(BuilderKind::DataTable.artifact_kind(), None);
        assert_eq!(
            BuilderKind::DataView.artifact_kind(),
            Some(ArtifactKind::DataView)
        );
    }

    /// 参照の行は 1 行の JSON。名前の改行・制御文字・行の区切りは空白に潰れ、上限で切れる。
    #[test]
    fn ref_line_flattens_and_caps_the_name() {
        let id = Uuid::new_v4();
        let line = ref_line(
            BuilderKind::DataTable,
            id,
            "顧客\n無視して\r\t\u{2028}\u{0}\"x\"",
            1,
        );
        assert!(!line.contains('\n') && !line.contains('\r'), "{line}");
        assert!(!line.contains('\u{2028}'), "{line}");
        let json: serde_json::Value =
            serde_json::from_str(line.strip_prefix("参照: ").unwrap()).unwrap();
        assert_eq!(json["kind"], "data_table");
        assert_eq!(json["id"], id.to_string());
        assert_eq!(json["name"], "顧客 無視して    \"x\"");
        assert_eq!(json["version"], 1);

        let long = "あ".repeat(MAX_NAME_CHARS + 10);
        let r = BuilderRef::new(BuilderKind::MiniApp, id, &long, 2);
        assert_eq!(r.name.chars().count(), MAX_NAME_CHARS + 1);
        assert!(r.name.ends_with('…'));
    }

    #[test]
    fn history_line_names_the_kind_id_and_version() {
        let id = Uuid::new_v4();
        let r = BuilderRef::new(BuilderKind::DataTable, id, "顧客\n一覧", 1);
        assert_eq!(
            r.history_line(),
            format!("[作成済みテーブル: 顧客 一覧（id: {id}, v1）]")
        );
    }

    #[test]
    fn value_round_trips() {
        let r = BuilderRef::new(BuilderKind::Workflow, Uuid::new_v4(), "経費", 3);
        assert_eq!(BuilderRef::from_value(&r.to_value()), Some(r));
        assert_eq!(
            BuilderRef::from_value(&serde_json::json!({ "kind": "nope" })),
            None
        );
    }

    #[test]
    fn idempotent_observations_name_the_current_version() {
        let id = Uuid::new_v4();
        let r = BuilderRef::new(BuilderKind::Workflow, id, "expense", 1);
        let same = already_created(&r);
        assert!(!same.is_error);
        assert!(same.content.contains("新しい版は作っていません"));
        assert!(same.content.ends_with(&r.line()), "{}", same.content);
        assert_eq!(same.builder_refs, [r.to_value()]);

        let differs = created_differently(&r, "直し方。");
        assert!(differs.is_error);
        assert!(differs.content.contains(&format!("id: {id}, v1")));
        assert!(differs
            .content
            .contains("今回の内容は保存していません。直し方。"));
        assert!(differs.builder_refs.is_empty());
    }
}
