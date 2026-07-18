//! WOPI access_token（HMAC-SHA256 署名の自己完結トークン・Task 11.6）。
//!
//! 形式は `base64url(claims JSON) + "." + base64url(HMAC-SHA256)`。クレームに
//! （実行主体×org×tenant×file_id×exp）を焼き込み、**他ファイル・他テナントへ
//! 流用できない**（file_id 固定・tenant/org 固定）。
//!
//! **トークンは UX 用であり権限の根拠ではない**。検証はあくまで「クレームの真正性」
//! の確認であり、実際のアクセス可否は毎 WOPI 呼び出しの OpenFGA check
//! （`HigherConsistency`・`routes` 側）が決める。共有解除はトークンの残存寿命に
//! かかわらず次の呼び出しで即時反映される（PIT-11）。

use std::time::Duration;

use authz::{AuthContext, Principal, PrincipalKind};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use uuid::Uuid;

use crate::error::OfficeError;

type HmacSha256 = Hmac<Sha256>;

/// access_token の有効期間（60 分）。Collabora のセッション継続はトークン更新でなく
/// 毎呼び出し ReBAC が守るため、失効＝再入室（/office/sessions の再発行）で足りる。
pub const TOKEN_TTL: Duration = Duration::from_hours(1);

/// トークン署名鍵（HMAC-SHA256）。
///
/// `SHIKI__OFFICE__TOKEN_SECRET` 未設定ならプロセス起動時に乱数生成する
/// （再起動で全セッション失効＝許容。複数レプリカ構成では設定注入が必須）。
#[derive(Clone)]
pub struct OfficeTokenKey(Vec<u8>);

/// 設定注入する秘密鍵の最小長（バイト）。HMAC-SHA256 の鍵として弱すぎる値を弾く。
const MIN_SECRET_LEN: usize = 32;

impl OfficeTokenKey {
    /// プロセス起動時の乱数鍵（32 バイト・CSPRNG）。
    pub fn random() -> Self {
        use rand::RngCore;
        let mut key = vec![0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut key);
        OfficeTokenKey(key)
    }

    /// 設定注入の秘密鍵から構築する（32 バイト未満は拒否＝弱鍵の排除）。
    pub fn from_secret(secret: &str) -> Result<Self, OfficeError> {
        if secret.len() < MIN_SECRET_LEN {
            return Err(OfficeError::Invalid(format!(
                "office.token_secret は {MIN_SECRET_LEN} バイト以上が必要です"
            )));
        }
        Ok(OfficeTokenKey(secret.as_bytes().to_vec()))
    }

    fn mac(&self) -> Result<HmacSha256, OfficeError> {
        // HMAC は任意長鍵を受けるため実質失敗しないが、fail-closed で写像する。
        HmacSha256::new_from_slice(&self.0).map_err(|_| OfficeError::Unauthorized)
    }
}

/// トークンのクレーム（実行主体×org×tenant×file×期限）。
///
/// 全フィールド必須（`#[serde(default)]` を付けない）。欠落したクレームは
/// デシリアライズ失敗＝検証拒否になる（fail-closed）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WopiClaims {
    pub principal_id: String,
    pub principal_kind: PrincipalKind,
    pub org: String,
    pub tenant_id: String,
    pub file_id: Uuid,
    /// 失効時刻（unix 秒）。
    pub exp: i64,
}

impl WopiClaims {
    /// クレームから AuthContext を再構成する。
    ///
    /// email/groups/roles は持ち越さない（WOPI 経路の認可は OpenFGA タプルのみで
    /// 決まり、IdP 由来のメタデータを必要としない）。
    pub fn to_auth_context(&self) -> AuthContext {
        AuthContext::new(
            Principal {
                kind: self.principal_kind,
                id: self.principal_id.clone(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some(self.tenant_id.clone()),
            },
            self.org.clone(),
            self.tenant_id.clone(),
        )
    }
}

/// access_token を発行する（実行主体×ファイル×TTL 60 分）。
pub fn issue(
    key: &OfficeTokenKey,
    ctx: &AuthContext,
    file_id: Uuid,
) -> Result<String, OfficeError> {
    let exp = chrono::Utc::now().timestamp() + i64::try_from(TOKEN_TTL.as_secs()).unwrap_or(3600);
    issue_at(key, ctx, file_id, exp)
}

/// 失効時刻を指定して発行する（テストで期限切れを作るための内部口）。
fn issue_at(
    key: &OfficeTokenKey,
    ctx: &AuthContext,
    file_id: Uuid,
    exp: i64,
) -> Result<String, OfficeError> {
    let claims = WopiClaims {
        principal_id: ctx.principal.id.clone(),
        principal_kind: ctx.principal.kind,
        org: ctx.org.clone(),
        tenant_id: ctx.tenant_id.clone(),
        file_id,
        exp,
    };
    let payload = serde_json::to_vec(&claims).map_err(|_| OfficeError::Unauthorized)?;
    let payload_b64 = URL_SAFE_NO_PAD.encode(&payload);
    let mut mac = key.mac()?;
    mac.update(payload_b64.as_bytes());
    let sig_b64 = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    Ok(format!("{payload_b64}.{sig_b64}"))
}

/// access_token を検証し、クレームを返す。
///
/// 検証内容: 署名（定数時間比較）・期限・**URL パスの file_id との一致**
/// （他ファイルへの流用を構造的に不能化）。失敗理由は区別せず
/// [`OfficeError::Unauthorized`] に潰す（オラクル防止・fail-closed）。
pub fn verify(
    key: &OfficeTokenKey,
    token: &str,
    expected_file_id: Uuid,
) -> Result<WopiClaims, OfficeError> {
    verify_at(key, token, expected_file_id, chrono::Utc::now().timestamp())
}

fn verify_at(
    key: &OfficeTokenKey,
    token: &str,
    expected_file_id: Uuid,
    now: i64,
) -> Result<WopiClaims, OfficeError> {
    let (payload_b64, sig_b64) = token.split_once('.').ok_or(OfficeError::Unauthorized)?;
    let sig = URL_SAFE_NO_PAD
        .decode(sig_b64)
        .map_err(|_| OfficeError::Unauthorized)?;
    let mut mac = key.mac()?;
    mac.update(payload_b64.as_bytes());
    // verify_slice は定数時間比較（タイミング側チャネル防止）。
    mac.verify_slice(&sig)
        .map_err(|_| OfficeError::Unauthorized)?;
    let payload = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|_| OfficeError::Unauthorized)?;
    let claims: WopiClaims =
        serde_json::from_slice(&payload).map_err(|_| OfficeError::Unauthorized)?;
    if claims.exp <= now {
        return Err(OfficeError::Unauthorized);
    }
    if claims.file_id != expected_file_id {
        return Err(OfficeError::Unauthorized);
    }
    Ok(claims)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn ctx() -> AuthContext {
        AuthContext::new(
            Principal {
                kind: PrincipalKind::User,
                id: "alice".into(),
                email: Some("alice@example.com".into()),
                groups: vec!["/acme".into()],
                roles: vec![],
                tenant_id: Some("default".into()),
            },
            "acme".into(),
            "default".into(),
        )
    }

    #[test]
    fn roundtrip_and_claims() {
        let key = OfficeTokenKey::random();
        let file_id = Uuid::new_v4();
        let token = issue(&key, &ctx(), file_id).unwrap();
        let claims = verify(&key, &token, file_id).unwrap();
        assert_eq!(claims.principal_id, "alice");
        assert_eq!(claims.tenant_id, "default");
        assert_eq!(claims.org, "acme");
        assert_eq!(claims.file_id, file_id);
        // AuthContext 再構成（tenant/org 焼き込み・IdP メタは持ち越さない）。
        let rebuilt = claims.to_auth_context();
        assert_eq!(rebuilt.tenant_id, "default");
        assert_eq!(rebuilt.org, "acme");
        assert_eq!(rebuilt.principal.email, None);
    }

    #[test]
    fn rejects_tampered_signature() {
        let key = OfficeTokenKey::random();
        let file_id = Uuid::new_v4();
        let token = issue(&key, &ctx(), file_id).unwrap();
        let mut tampered = token.clone();
        // 署名末尾 1 文字を反転する。
        let last = tampered.pop().unwrap();
        tampered.push(if last == 'A' { 'B' } else { 'A' });
        assert!(matches!(
            verify(&key, &tampered, file_id),
            Err(OfficeError::Unauthorized)
        ));
        // 別鍵で署名されたトークンも拒否。
        let other_key = OfficeTokenKey::random();
        let forged = issue(&other_key, &ctx(), file_id).unwrap();
        assert!(matches!(
            verify(&key, &forged, file_id),
            Err(OfficeError::Unauthorized)
        ));
    }

    #[test]
    fn rejects_tampered_payload() {
        let key = OfficeTokenKey::random();
        let file_id = Uuid::new_v4();
        let token = issue(&key, &ctx(), file_id).unwrap();
        let (_, sig) = token.split_once('.').unwrap();
        // 別テナントを主張する payload に元の署名を付け替える。
        let evil = serde_json::json!({
            "principal_id": "alice", "principal_kind": "user", "org": "acme",
            "tenant_id": "other-tenant", "file_id": file_id, "exp": i64::MAX,
        });
        let evil_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&evil).unwrap());
        assert!(matches!(
            verify(&key, &format!("{evil_b64}.{sig}"), file_id),
            Err(OfficeError::Unauthorized)
        ));
    }

    #[test]
    fn rejects_expired() {
        let key = OfficeTokenKey::random();
        let file_id = Uuid::new_v4();
        let now = chrono::Utc::now().timestamp();
        let token = issue_at(&key, &ctx(), file_id, now - 1).unwrap();
        assert!(matches!(
            verify_at(&key, &token, file_id, now),
            Err(OfficeError::Unauthorized)
        ));
        // ちょうど exp == now も失効扱い（fail-closed）。
        let token = issue_at(&key, &ctx(), file_id, now).unwrap();
        assert!(matches!(
            verify_at(&key, &token, file_id, now),
            Err(OfficeError::Unauthorized)
        ));
    }

    #[test]
    fn rejects_other_file_id() {
        let key = OfficeTokenKey::random();
        let token = issue(&key, &ctx(), Uuid::new_v4()).unwrap();
        assert!(matches!(
            verify(&key, &token, Uuid::new_v4()),
            Err(OfficeError::Unauthorized)
        ));
    }

    #[test]
    fn rejects_missing_claims() {
        let key = OfficeTokenKey::random();
        let file_id = Uuid::new_v4();
        // tenant_id を欠いた（正しく署名された）トークンはデシリアライズで拒否。
        let payload = serde_json::json!({
            "principal_id": "alice", "principal_kind": "user", "org": "acme",
            "file_id": file_id, "exp": i64::MAX,
        });
        let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
        let mut mac = key.mac().unwrap();
        mac.update(payload_b64.as_bytes());
        let sig_b64 = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        assert!(matches!(
            verify(&key, &format!("{payload_b64}.{sig_b64}"), file_id),
            Err(OfficeError::Unauthorized)
        ));
        // 区切り無し・空文字も拒否。
        assert!(verify(&key, "", file_id).is_err());
        assert!(verify(&key, "abc", file_id).is_err());
    }

    #[test]
    fn weak_secret_is_rejected() {
        assert!(OfficeTokenKey::from_secret("short").is_err());
        assert!(OfficeTokenKey::from_secret(&"x".repeat(32)).is_ok());
    }
}
