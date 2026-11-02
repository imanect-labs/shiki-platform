//! アーティファクトのドメイン型（DTO は単一定義・codegen が正）。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// アーティファクト種別（閉じた集合）。
///
/// Stage A で使用するのは `workflow`（IR）のみ。他 variant は Phase 6（mini_app / skill。
/// skill は旧 prompt template を統合した kind）・Phase 10 Stage B（script）の予約で、追加はこの enum と
/// migration の CHECK 制約の両方を更新する（DB と語彙の二重定義はここで閉じる）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Workflow,
    MiniApp,
    /// 構造化データの保存ビュー（宣言的クエリ＋表示設定・Task 9.4）。
    DataView,
    /// 構造化データの FSM 宣言的ガード（status 遷移＋遷移認可・Task 9.10）。
    Fsm,
    /// コードベース・ミニアプリのマニフェスト（Task 9.1）。
    MiniAppCode,
    Skill,
    Script,
}

impl ArtifactKind {
    /// DB へ保存する文字列表現（migration の CHECK 制約と一致）。
    pub const fn as_str(self) -> &'static str {
        match self {
            ArtifactKind::Workflow => "workflow",
            ArtifactKind::MiniApp => "mini_app",
            ArtifactKind::DataView => "data_view",
            ArtifactKind::Fsm => "fsm",
            ArtifactKind::MiniAppCode => "mini_app_code",
            ArtifactKind::Skill => "skill",
            ArtifactKind::Script => "script",
        }
    }

    /// 下書きと公開を分ける種類か（D-16・GAP-36）。保存は下書きの版になり、使う人（editor でない viewer）には
    /// 公開した版（`artifact.published_version`）だけを出す（[`ArtifactStore::get_version`]・
    /// [`ArtifactStore::list_versions`]）。公開は editor が行う（[`ArtifactStore::publish`]）。
    ///
    /// [`ArtifactStore::get_version`]: crate::ArtifactStore::get_version
    /// [`ArtifactStore::list_versions`]: crate::ArtifactStore::list_versions
    /// [`ArtifactStore::publish`]: crate::ArtifactStore::publish
    pub const fn has_drafts(self) -> bool {
        matches!(self, ArtifactKind::MiniApp)
    }

    /// DB から読み戻した文字列を閉じた集合へ写す（未知はフェイルクローズで `None`）。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "workflow" => Some(ArtifactKind::Workflow),
            "mini_app" => Some(ArtifactKind::MiniApp),
            "data_view" => Some(ArtifactKind::DataView),
            "fsm" => Some(ArtifactKind::Fsm),
            "mini_app_code" => Some(ArtifactKind::MiniAppCode),
            "skill" => Some(ArtifactKind::Skill),
            "script" => Some(ArtifactKind::Script),
            _ => None,
        }
    }
}

impl std::fmt::Display for ArtifactKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// アーティファクトのメタデータ（本文はバージョン側）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Artifact {
    pub id: Uuid,
    pub kind: ArtifactKind,
    /// tenant×kind 内一意の参照名。
    pub name: String,
    /// 作成者の subject local id。
    pub owner: String,
    /// 最新バージョン番号（0 = バージョン未追記は存在しない。作成時に 1 が入る）。
    pub current_version: i64,
    /// 公開した版（下書きと公開を分ける種類だけ・まだ公開していなければ `null`・D-16）。使う人に出すのはこの版。
    pub published_version: Option<i64>,
    /// 前に公開していた版（公開を戻す先・無ければ `null`）。
    pub previous_published_version: Option<i64>,
    /// 公開した時刻（`published_version` を決めた時刻）。
    pub published_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// アーティファクトの 1 バージョン（本文付き・不変）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ArtifactVersion {
    pub artifact_id: Uuid,
    pub version: i64,
    pub body: serde_json::Value,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

/// バージョン一覧用のメタ（本文なし・履歴表示用）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VersionMeta {
    pub version: i64,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

/// 共有役割（共有語彙は viewer/editor のみ・owner の横展開は防ぐ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactRole {
    Viewer,
    Editor,
}

impl ArtifactRole {
    /// OpenFGA relation へ写す。
    pub fn relation(self) -> authz::Relation {
        match self {
            ArtifactRole::Viewer => authz::Relation::Viewer,
            ArtifactRole::Editor => authz::Relation::Editor,
        }
    }

    /// relation を共有役割へ戻す（viewer/editor 以外は `None`）。
    pub fn from_relation(relation: authz::Relation) -> Option<Self> {
        match relation {
            authz::Relation::Viewer => Some(ArtifactRole::Viewer),
            authz::Relation::Editor => Some(ArtifactRole::Editor),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_str_roundtrip_all_variants() {
        for k in [
            ArtifactKind::Workflow,
            ArtifactKind::MiniApp,
            ArtifactKind::DataView,
            ArtifactKind::Fsm,
            ArtifactKind::MiniAppCode,
            ArtifactKind::Skill,
            ArtifactKind::Script,
        ] {
            assert_eq!(ArtifactKind::parse(k.as_str()), Some(k));
            assert_eq!(k.to_string(), k.as_str());
        }
    }

    /// 下書きと公開を分けるのはミニアプリだけ（ほかの種類は今のまま、最新の版を使う）。
    #[test]
    fn only_mini_apps_have_drafts() {
        assert!(ArtifactKind::MiniApp.has_drafts());
        for k in [
            ArtifactKind::Workflow,
            ArtifactKind::DataView,
            ArtifactKind::Fsm,
            ArtifactKind::MiniAppCode,
            ArtifactKind::Skill,
            ArtifactKind::Script,
        ] {
            assert!(!k.has_drafts(), "{k}");
        }
    }

    #[test]
    fn kind_parse_unknown_fails_closed() {
        assert_eq!(ArtifactKind::parse("plugin"), None);
        assert_eq!(ArtifactKind::parse(""), None);
    }

    #[test]
    fn kind_serde_snake_case() {
        assert_eq!(
            serde_json::to_string(&ArtifactKind::Skill).unwrap(),
            "\"skill\""
        );
        let k: ArtifactKind = serde_json::from_str("\"workflow\"").unwrap();
        assert_eq!(k, ArtifactKind::Workflow);
        let bad: Result<ArtifactKind, _> = serde_json::from_str("\"plugin\"");
        assert!(bad.is_err());
    }

    #[test]
    fn role_relation_roundtrip() {
        assert_eq!(ArtifactRole::Viewer.relation(), authz::Relation::Viewer);
        assert_eq!(
            ArtifactRole::from_relation(authz::Relation::Editor),
            Some(ArtifactRole::Editor)
        );
        // owner は共有役割へ戻さない（横展開防止）。
        assert_eq!(ArtifactRole::from_relation(authz::Relation::Owner), None);
    }
}
