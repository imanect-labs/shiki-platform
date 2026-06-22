//! セッションストアのトレイトとレコード型（BFF セッションの中核）。
//!
//! ブラウザには不透明な session id（Cookie）しか出さず、principal/claims/OIDC token/
//! 期限といった本体は [`SessionRecord`] としてストアに保持する。
//! キーは **`tenant_id` でスコープ**し、共用プール型 Redis でもテナント越境で
//! session を引けないようにする（docs/auth/browser-token-strategy.md §7.2）。

use std::time::Duration;

use async_trait::async_trait;
use authz::Principal;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("session store backend エラー: {0}")]
    Backend(String),
    #[error("session の serialize/deserialize に失敗: {0}")]
    Serde(String),
}

/// セッション本体。ブラウザには出さず、ストアにのみ保持する。
///
/// `access_token` / `refresh_token` はサーバ側でのみ保持し、access token の期限切れ前に
/// BFF が refresh でローテーションする（downstream の token-exchange が 401 にならないため）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    /// 検証済みクレーム由来の認証主体（セッション復元時にそのまま extension へ載せる）。
    pub principal: Principal,
    /// 解決済みテナント識別子（キーのスコープと一致する。防御的に保持して照合する）。
    pub tenant_id: String,
    /// OIDC access token（downstream への JWT/token-exchange に使う・サーバ側のみ）。
    pub access_token: String,
    /// OIDC refresh token（サーバ側のみ・access のローテーションに使う）。
    pub refresh_token: Option<String>,
    /// OIDC id token（サーバ側のみ。将来の backchannel logout 等に備えて保持する。
    /// BFF 不変条件によりブラウザには出さない＝logout の id_token_hint には使わない）。
    pub id_token: Option<String>,
    /// access token の満了時刻（unix 秒）。
    pub access_expires_at: i64,
    /// double-submit CSRF トークン（CSRF Cookie と突合する）。
    pub csrf_token: String,
}

/// BFF セッションストア（チョークポイント。Redis 実装の裏に隠す）。
///
/// すべての操作が `tenant_id` を要求し、キーをテナントスコープ化する。あるテナントの
/// コンテキストから他テナントの session id を引けないようにするための継ぎ目。
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// セッションを作成/更新（upsert）する。`ttl` で失効する。
    async fn put(
        &self,
        tenant_id: &str,
        session_id: &str,
        record: &SessionRecord,
        ttl: Duration,
    ) -> Result<(), SessionError>;

    /// **既存セッションがある時のみ**更新する（無ければ作らない）。更新したら `true`。
    ///
    /// refresh ローテーションの保存に使う。logout がセッションを削除した直後に refresh の
    /// 書き戻しでセッションを**復活させない**ため（即時失効の保証を守る）。
    async fn update_if_present(
        &self,
        tenant_id: &str,
        session_id: &str,
        record: &SessionRecord,
        ttl: Duration,
    ) -> Result<bool, SessionError>;

    /// セッションを取得する（無ければ `None`）。
    async fn get(
        &self,
        tenant_id: &str,
        session_id: &str,
    ) -> Result<Option<SessionRecord>, SessionError>;

    /// セッションを削除する（ログアウト・失効）。
    async fn delete(&self, tenant_id: &str, session_id: &str) -> Result<(), SessionError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record() -> SessionRecord {
        SessionRecord {
            principal: Principal {
                id: "user-1".into(),
                email: Some("u@example.com".into()),
                groups: vec!["/acme".into()],
                dept: Some("eng".into()),
                tenant_id: Some("acme".into()),
            },
            tenant_id: "acme".into(),
            access_token: "access".into(),
            refresh_token: Some("refresh".into()),
            id_token: Some("id".into()),
            access_expires_at: 1_700_000_000,
            csrf_token: "csrf".into(),
        }
    }

    #[test]
    fn session_record_round_trip() {
        // ストアに JSON で保持されるため、シリアライズ→デシリアライズで等価であること。
        let record = sample_record();
        let json = serde_json::to_string(&record).unwrap();
        let restored: SessionRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.principal, record.principal);
        assert_eq!(restored.tenant_id, "acme");
        assert_eq!(restored.access_token, "access");
        assert_eq!(restored.refresh_token.as_deref(), Some("refresh"));
        assert_eq!(restored.id_token.as_deref(), Some("id"));
        assert_eq!(restored.access_expires_at, 1_700_000_000);
        assert_eq!(restored.csrf_token, "csrf");
    }

    #[test]
    fn session_record_optional_tokens_can_be_absent() {
        // refresh_token / id_token は欠落（None）でも復元できること。
        let mut record = sample_record();
        record.refresh_token = None;
        record.id_token = None;
        let json = serde_json::to_string(&record).unwrap();
        let restored: SessionRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.refresh_token, None);
        assert_eq!(restored.id_token, None);
    }

    #[test]
    fn session_record_serializes_expected_keys() {
        // 永続化フォーマットのキー名を固定する（互換性のため）。
        let value = serde_json::to_value(sample_record()).unwrap();
        for key in [
            "principal",
            "tenant_id",
            "access_token",
            "refresh_token",
            "id_token",
            "access_expires_at",
            "csrf_token",
        ] {
            assert!(value.get(key).is_some(), "キー {key} が欠落");
        }
    }

    #[test]
    fn session_error_display() {
        // backend / serde エラーの表示文言。
        assert!(SessionError::Backend("redis timeout".into())
            .to_string()
            .contains("redis timeout"));
        assert!(SessionError::Serde("bad json".into())
            .to_string()
            .contains("bad json"));
    }
}
