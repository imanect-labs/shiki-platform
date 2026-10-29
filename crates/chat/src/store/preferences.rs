//! `ChatStore`: 本人のユーザー設定の読み書き（設定 → チャット／表示・#952）。
//!
//! 行は (tenant_id, org, user_id) で 1 人 1 行。範囲はすべて `AuthContext` から決め、本人以外の
//! 行は読み書きしない（他人を指す引数そのものが無い）。ReBAC のオブジェクトは持たない。

use authz::AuthContext;
use storage::audit::{record_on, AuditAction, AuditEntry, Chain, Decision};

use crate::autonomous::AutonomousMode;
use crate::error::ChatError;
use crate::preferences::UserPreferences;

use super::threads::{map_db, map_storage};
use super::ChatStore;

/// org ポリシで全自動を禁止しているときの、既定の承認モードの保存エラー。
const BYPASS_FORBIDDEN: &str = "全自動（bypass）モードは組織ポリシで禁止されています";

impl ChatStore {
    /// 本人のユーザー設定（未登録なら既定値）。
    pub async fn get_preferences(&self, ctx: &AuthContext) -> Result<UserPreferences, ChatError> {
        let row: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT prefs FROM user_preference \
             WHERE tenant_id = $1 AND org = $2 AND user_id = $3",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(&ctx.principal.id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        Ok(row.map(UserPreferences::from_stored).unwrap_or_default())
    }

    /// 本人のユーザー設定を置き換える（正規化・検査のうえ保存し、保存した値を返す）。
    ///
    /// 既定の承認モードを全自動**へ変える**のは org が許可している場合だけ（禁止なら明示エラー・
    /// スレッドの承認モード変更と同じ扱い）。保存済みの全自動を持ったままほかの項目を変えるのは
    /// 拒否しない（画面は設定全体を送るので、拒否すると org が後から禁止しただけで何も保存できなく
    /// なる）。既定の承認モードはホームの承認モードの選択欄の初期値にすぎず、スレッドはサーバが勝手に
    /// 緩めない（作成者がエージェントのモードで明示して送ったときだけ変わる）。
    /// 変更と監査は同じトランザクションで書く。設定値は個人の記述を含まないので、監査には保存した値を
    /// そのまま残す（承認モードの緩和を追える）。
    pub async fn put_preferences(
        &self,
        ctx: &AuthContext,
        input: UserPreferences,
        trace_id: Option<&str>,
    ) -> Result<UserPreferences, ChatError> {
        let value = input.normalized()?;
        let stored = serde_json::to_value(&value)
            .map_err(|e| ChatError::Internal(format!("preferences: {e}")))?;
        let mut tx = self.db.begin().await.map_err(map_db)?;
        if value.default_autonomous_mode == AutonomousMode::Bypass {
            // 今の保存値を行ロック付きで読む（同時の保存と競って検査をすり抜けさせない）。
            let current: Option<serde_json::Value> = sqlx::query_scalar(
                "SELECT prefs FROM user_preference \
                 WHERE tenant_id = $1 AND org = $2 AND user_id = $3 FOR UPDATE",
            )
            .bind(&ctx.tenant_id)
            .bind(&ctx.org)
            .bind(&ctx.principal.id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_db)?;
            let already_bypass = current
                .map(UserPreferences::from_stored)
                .is_some_and(|p| p.default_autonomous_mode == AutonomousMode::Bypass);
            // org キャップも同じトランザクションで読む（接続を握ったままプールから 2 本目を取りに
            // 行かない。同時の保存が多いとプールを使い切って互いに待ち合う）。未登録のテナントは許可
            // （`autonomous_bypass_allowed` と同じ扱い）。
            let allowed: Option<bool> = sqlx::query_scalar(
                "SELECT allow_autonomous_bypass FROM tenant WHERE tenant_id = $1",
            )
            .bind(&ctx.tenant_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_db)?;
            if !already_bypass && !allowed.unwrap_or(true) {
                return Err(ChatError::Invalid(BYPASS_FORBIDDEN.into()));
            }
        }
        sqlx::query(
            "INSERT INTO user_preference (tenant_id, org, user_id, prefs) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (tenant_id, org, user_id) DO UPDATE SET \
               prefs = EXCLUDED.prefs, updated_at = now()",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(&ctx.principal.id)
        .bind(&stored)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: AuditAction::MePreferencesUpdate,
                object_type: "user_preferences",
                object_id: &ctx.principal.id,
                decision: Decision::Allow,
                trace_id,
                metadata: stored,
            },
            Chain::Yes,
        )
        .await
        .map_err(map_storage)?;
        tx.commit().await.map_err(map_db)?;
        Ok(value)
    }
}
