//! 会話の対象（ワークフロー・ミニアプリの画面の「AI に依頼」）と目的（app_builder）・D-10 T09。
//!
//! 対象の画面から作った会話は、開いていた artifact を持つ。ワーカーは run の開始時に、その対象を発話者の
//! 権限で読み直して system に注記し、編集に使う道具を常時ロードにする。対象か目的を持つ会話は、最初から
//! 作成用の予算（`chat.app_builder.*`）で動く。
//!
//! これは誘導のメタで、認可の真実源ではない（対象の権限は artifact の ReBAC・会話の閲覧権とは別）。

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// 会話の対象の種類（DB の CHECK と同じ閉集合）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OriginArtifactKind {
    Workflow,
    MiniApp,
}

impl OriginArtifactKind {
    /// DB / API で共通の文字列表現。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            OriginArtifactKind::Workflow => "workflow",
            OriginArtifactKind::MiniApp => "mini_app",
        }
    }

    /// 文字列から閉集合へ（未知は `None`・fail-closed）。
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "workflow" => Some(OriginArtifactKind::Workflow),
            "mini_app" => Some(OriginArtifactKind::MiniApp),
            _ => None,
        }
    }

    /// 対応する artifact の種類（API 層が対象を引いて照合する）。
    #[must_use]
    pub const fn artifact_kind(self) -> artifact::ArtifactKind {
        match self {
            OriginArtifactKind::Workflow => artifact::ArtifactKind::Workflow,
            OriginArtifactKind::MiniApp => artifact::ArtifactKind::MiniApp,
        }
    }
}

/// 会話の目的（DB の CHECK と同じ閉集合）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ThreadPurpose {
    /// アプリ（テーブル・ワークフロー・画面）を AI と作る会話（/apps の「AI と作る」）。
    AppBuilder,
}

impl ThreadPurpose {
    /// DB / API で共通の文字列表現。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ThreadPurpose::AppBuilder => "app_builder",
        }
    }

    /// 文字列から閉集合へ（未知は `None`・fail-closed）。
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "app_builder" => Some(ThreadPurpose::AppBuilder),
            _ => None,
        }
    }
}

/// 会話の対象（作成時に API 層が発話者の viewer 権限で解決したもの）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginArtifact {
    pub kind: OriginArtifactKind,
    pub id: Uuid,
    /// 解決した時点の表示名（履歴の表示用・改名には追随しない）。
    pub name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip_and_reject_unknown() {
        for k in [OriginArtifactKind::Workflow, OriginArtifactKind::MiniApp] {
            assert_eq!(OriginArtifactKind::parse(k.as_str()), Some(k));
            assert_eq!(k.artifact_kind().as_str(), k.as_str());
        }
        assert_eq!(OriginArtifactKind::parse("note"), None);
        assert_eq!(
            ThreadPurpose::parse(ThreadPurpose::AppBuilder.as_str()),
            Some(ThreadPurpose::AppBuilder)
        );
        assert_eq!(ThreadPurpose::parse("chat"), None);
    }

    #[test]
    fn serde_uses_the_db_spelling() {
        assert_eq!(
            serde_json::to_value(OriginArtifactKind::MiniApp).unwrap(),
            "mini_app"
        );
        assert_eq!(
            serde_json::to_value(ThreadPurpose::AppBuilder).unwrap(),
            "app_builder"
        );
    }
}
