//! パーソナルエージェントの名前とアイコン（design §4.14・Task 14.20）。
//!
//! 名前とアイコンは**見た目だけ**で、権限・承認・使える道具に影響しない。呼べるのは持ち主本人だけ
//! （[`crate::agent`]）。発言には「〇〇 のエージェント」と持ち主を必ず並べて出し、実在の利用者名や
//! 「シキ」「管理者」と紛らわしい名前は付けさせない。

use authz::AuthContext;
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use std::collections::HashMap;
use storage::audit::AuditAction;
use storage::db_tenant::begin_tenant_tx;
use utoipa::ToSchema;

use crate::store;
use crate::{Error, MessagingService, DEFAULT_AGENT_NAME};

/// アイコンの最大バイト数（256 KiB）。
pub const MAX_ICON_BYTES: usize = 256 * 1024;

/// パーソナルエージェントの設定（本人から見た状態）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AgentProfileView {
    pub name: String,
    pub has_icon: bool,
    /// 名前が未設定で既定（シキ）か。
    pub is_default: bool,
}

/// 名前・アイコンの更新。
#[derive(Debug, Clone, Default)]
pub struct AgentProfileUpdate {
    /// `Some("")` で既定に戻す。
    pub name: Option<String>,
    /// `Some(None)` でアイコンを外す。
    pub icon: Option<Option<(Vec<u8>, String)>>,
}

pub(crate) struct ProfileRow {
    pub name: String,
    pub has_icon: bool,
}

/// 書き手（持ち主）ごとのエージェント名とアイコンの有無。未設定の人は含まない（既定 = シキ）。
pub(crate) async fn profiles(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    users: &[String],
) -> Result<HashMap<String, ProfileRow>, Error> {
    if users.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(String, String, bool)> = sqlx::query_as(
        "SELECT user_id, name, icon IS NOT NULL FROM messaging_agent_profile \
         WHERE tenant_id = $1 AND org = $2 AND user_id = ANY($3)",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(users)
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(u, name, has_icon)| (u, ProfileRow { name, has_icon }))
        .collect())
}

pub(crate) async fn agent_name(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    user: &str,
) -> Result<String, Error> {
    Ok(profiles(conn, ctx, &[user.to_string()])
        .await?
        .remove(user)
        .map_or_else(|| DEFAULT_AGENT_NAME.to_string(), |p| p.name))
}

/// なりすましの判定に使う正規化（空白を除き小文字・全角英数を半角・ひらがなをカタカナに）。
#[must_use]
pub fn normalize_name(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '・' | '_' | '-' | '.' | '　'))
        .map(|c| {
            let u = c as u32;
            let c = match u {
                0xFF01..=0xFF5E => char::from_u32(u - 0xFEE0).unwrap_or(c),
                0x3041..=0x3096 => char::from_u32(u + 0x60).unwrap_or(c),
                _ => c,
            };
            // 半角カタカナのシ・キ（ｼｷ）も同一視する。
            match c {
                'ｼ' => 'シ',
                'ｷ' => 'キ',
                _ => c,
            }
        })
        .flat_map(char::to_lowercase)
        .collect()
}

/// 公式・管理者・既定のエージェントと紛らわしい語（正規化後に含んでいたら拒否）。
const RESERVED: &[&str] = &[
    "シキ",
    "shiki",
    "式",
    "管理者",
    "管理人",
    "admin",
    "administrator",
    "システム",
    "system",
    "運営",
    "公式",
    "official",
    "サポート",
    "support",
    "監査",
    "audit",
];

/// エージェント名を検証する（1〜32 文字・制御文字なし・紛らわしい語を含まない）。
///
/// # Errors
///
/// 不正・紛らわしい名前のとき [`Error::Invalid`]。
pub fn validate_agent_name(name: &str) -> Result<String, Error> {
    let name = name.trim().to_string();
    let n = name.chars().count();
    if n == 0 || n > 32 {
        return Err(Error::Invalid("名前は 1〜32 文字です".into()));
    }
    if name.chars().any(|c| c.is_control() || c == '@') {
        return Err(Error::Invalid("名前に使えない文字が含まれています".into()));
    }
    let norm = normalize_name(&name);
    if norm.is_empty() || RESERVED.iter().any(|r| norm.contains(&normalize_name(r))) {
        return Err(Error::Invalid(
            "「シキ」「管理者」などと紛らわしい名前は付けられません".into(),
        ));
    }
    Ok(name)
}

/// アイコンの形式を中身（マジックバイト）で判定する（PNG / JPEG / WebP だけ）。
#[must_use]
pub fn icon_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

impl MessagingService {
    // ---- パーソナルエージェントの名前とアイコン（Task 14.20） ----

    pub async fn agent_profile(&self, ctx: &AuthContext) -> Result<AgentProfileView, Error> {
        self.require_member(ctx).await?;
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let me = ctx.principal().id.clone();
        let p = profiles(&mut tx, ctx, std::slice::from_ref(&me))
            .await?
            .remove(&me);
        tx.commit().await?;
        Ok(match p {
            Some(p) => AgentProfileView {
                name: p.name,
                has_icon: p.has_icon,
                is_default: false,
            },
            None => AgentProfileView {
                name: DEFAULT_AGENT_NAME.into(),
                has_icon: false,
                is_default: true,
            },
        })
    }

    /// 名前・アイコンを変える（見た目だけ・権限・承認・道具に影響しない）。
    pub async fn set_agent_profile(
        &self,
        ctx: &AuthContext,
        update: AgentProfileUpdate,
        trace: Option<&str>,
    ) -> Result<AgentProfileView, Error> {
        self.require_member(ctx).await?;
        let me = ctx.principal().id.clone();
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let current = profiles(&mut tx, ctx, std::slice::from_ref(&me))
            .await?
            .remove(&me);
        let name = match update.name.as_deref() {
            Some("") => None,
            Some(n) => {
                let n = validate_agent_name(n)?;
                if store::display_name_taken(&mut tx, ctx, &normalize_name(&n)).await? {
                    return Err(Error::Invalid(
                        "組織の利用者と同じ名前は付けられません".into(),
                    ));
                }
                Some(n)
            }
            None => current.as_ref().map(|p| p.name.clone()),
        };
        if let Some(Some((bytes, mime))) = &update.icon {
            if bytes.len() > MAX_ICON_BYTES {
                return Err(Error::Invalid("アイコンは 256 KB までです".into()));
            }
            if icon_mime(bytes) != Some(mime.as_str()) {
                return Err(Error::Invalid(
                    "アイコンは PNG・JPEG・WebP の画像だけです".into(),
                ));
            }
        }
        let keep_icon = update.icon.is_none();
        let has_icon = match &update.icon {
            None => current.as_ref().is_some_and(|p| p.has_icon),
            Some(icon) => icon.is_some(),
        };
        // 実際に名前を付けたか（既定の「シキ」のままなら行の name は既定値で持つ）。
        let named = match update.name.as_deref() {
            Some(_) => name.is_some(),
            None => current
                .as_ref()
                .is_some_and(|p| p.name != DEFAULT_AGENT_NAME),
        };
        if !named && !has_icon {
            // 名前もアイコンも既定 = 行を消す。
            sqlx::query(
                "DELETE FROM messaging_agent_profile WHERE tenant_id = $1 AND org = $2 AND user_id = $3",
            )
            .bind(ctx.tenant_id())
            .bind(ctx.org())
            .bind(&me)
            .execute(&mut *tx)
            .await?;
        } else {
            let name = name
                .clone()
                .unwrap_or_else(|| DEFAULT_AGENT_NAME.to_string());
            let (icon, mime) = match &update.icon {
                Some(Some((b, m))) => (Some(b.clone()), Some(m.clone())),
                _ => (None, None),
            };
            sqlx::query(
                "INSERT INTO messaging_agent_profile (tenant_id, org, user_id, name, icon, icon_mime) \
                 VALUES ($1, $2, $3, $4, $5, $6) \
                 ON CONFLICT (tenant_id, org, user_id) DO UPDATE SET name = $4, \
                   icon = CASE WHEN $7 THEN messaging_agent_profile.icon ELSE $5 END, \
                   icon_mime = CASE WHEN $7 THEN messaging_agent_profile.icon_mime ELSE $6 END, \
                   updated_at = now()",
            )
            .bind(ctx.tenant_id())
            .bind(ctx.org())
            .bind(&me)
            .bind(&name)
            .bind(icon)
            .bind(mime)
            .bind(keep_icon)
            .execute(&mut *tx)
            .await?;
        }
        storage::audit::record_on(
            &mut tx,
            ctx,
            storage::audit::AuditEntry {
                action: AuditAction::MessagingAgentProfileUpdate,
                object_type: "messaging_agent_profile",
                object_id: &me,
                decision: storage::audit::Decision::Allow,
                trace_id: trace,
                metadata: serde_json::json!({
                    "name": update.name.is_some(),
                    "icon": update.icon.is_some(),
                }),
            },
            storage::audit::Chain::Yes,
        )
        .await?;
        tx.commit().await?;
        self.agent_profile(ctx).await
    }

    /// エージェントのアイコン（組織の利用者なら誰でも見られる・プロフィール画像と同じ扱い）。
    pub async fn agent_icon(
        &self,
        ctx: &AuthContext,
        owner: &str,
    ) -> Result<(Vec<u8>, String), Error> {
        self.require_member(ctx).await?;
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let row: Option<(Vec<u8>, String)> = sqlx::query_as(
            "SELECT icon, icon_mime FROM messaging_agent_profile WHERE tenant_id = $1 AND org = $2 \
             AND user_id = $3 AND icon IS NOT NULL",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(owner)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        row.ok_or(Error::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_confusable_agent_names() {
        for ng in [
            "シキ",
            "しき",
            "ｼｷ",
            "ＳＨＩＫＩ",
            "shiki-bot",
            "管理者",
            "Admin",
            "システム",
            "公式アシスタント",
            "",
            "a@b",
            &"x".repeat(33),
        ] {
            assert!(validate_agent_name(ng).is_err(), "{ng}");
        }
        for ok in ["ミケ", "アシスタント花子", "Kotaro"] {
            assert_eq!(validate_agent_name(ok).unwrap(), ok);
        }
    }

    #[test]
    fn detects_icon_formats_by_content() {
        assert_eq!(
            icon_mime(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0]),
            Some("image/png")
        );
        assert_eq!(icon_mime(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(icon_mime(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(icon_mime(b"<svg onload=alert(1)>"), None);
    }
}
