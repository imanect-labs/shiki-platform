//! `ChatStore`: 本人のカスタム指示の読み書き（設定 → パーソナライズ・#900）。
//!
//! 行は (tenant_id, org, user_id) で 1 人 1 行。範囲はすべて `AuthContext` から決め、本人以外の
//! 行は読み書きしない（他人を指す引数そのものが無い）。ReBAC のオブジェクトは持たない。

use authz::AuthContext;
use serde_json::json;
use storage::audit::{record_on, AuditAction, AuditEntry, Chain, Decision};

use crate::custom_instructions::{CustomInstructionTone, CustomInstructions};
use crate::error::ChatError;

use super::threads::{map_db, map_storage};
use super::ChatStore;

impl ChatStore {
    /// 本人のカスタム指示（未登録なら空の既定値）。
    pub async fn get_custom_instructions(
        &self,
        ctx: &AuthContext,
    ) -> Result<CustomInstructions, ChatError> {
        let row: Option<(String, String, String)> = sqlx::query_as(
            "SELECT about, style, tone FROM user_custom_instruction \
             WHERE tenant_id = $1 AND org = $2 AND user_id = $3",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(&ctx.principal.id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        Ok(row
            .map(|(about, style, tone)| CustomInstructions {
                about,
                style,
                tone: CustomInstructionTone::parse(&tone),
            })
            .unwrap_or_default())
    }

    /// 本人のカスタム指示を置き換える（正規化・上限検査のうえ保存し、保存した値を返す）。
    ///
    /// すべて空にしたら行を消す（「設定なし」と同じ状態に戻す）。変更と監査は同じトランザクションで
    /// 書く（監査の無い変更を残さない）。監査には文字数と口調だけを残し、本文は残さない（個人の記述。
    /// 管理者に会話本文を読ませない design §4.12・FR-9 と同じ扱い）。
    pub async fn put_custom_instructions(
        &self,
        ctx: &AuthContext,
        input: CustomInstructions,
        trace_id: Option<&str>,
    ) -> Result<CustomInstructions, ChatError> {
        let value = input.normalized()?;
        let mut tx = self.db.begin().await.map_err(map_db)?;
        if value.is_empty() {
            sqlx::query(
                "DELETE FROM user_custom_instruction \
                 WHERE tenant_id = $1 AND org = $2 AND user_id = $3",
            )
            .bind(&ctx.tenant_id)
            .bind(&ctx.org)
            .bind(&ctx.principal.id)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        } else {
            sqlx::query(
                "INSERT INTO user_custom_instruction (tenant_id, org, user_id, about, style, tone) \
                 VALUES ($1, $2, $3, $4, $5, $6) \
                 ON CONFLICT (tenant_id, org, user_id) DO UPDATE SET \
                   about = EXCLUDED.about, style = EXCLUDED.style, tone = EXCLUDED.tone, \
                   updated_at = now()",
            )
            .bind(&ctx.tenant_id)
            .bind(&ctx.org)
            .bind(&ctx.principal.id)
            .bind(&value.about)
            .bind(&value.style)
            .bind(value.tone.as_str())
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        }
        record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: AuditAction::MeCustomInstructionsUpdate,
                object_type: "custom_instructions",
                object_id: &ctx.principal.id,
                decision: Decision::Allow,
                trace_id,
                metadata: json!({
                    "about_chars": value.about.chars().count(),
                    "style_chars": value.style.chars().count(),
                    "tone": value.tone.as_str(),
                }),
            },
            Chain::Yes,
        )
        .await
        .map_err(map_storage)?;
        tx.commit().await.map_err(map_db)?;
        Ok(value)
    }
}
