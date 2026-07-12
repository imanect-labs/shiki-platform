//! `can_use` 授権（Task 9.12）。store の子モジュール＝親の private フィールド/メソッド
//! （authz / require / record_audit）に到達するためここに置く。

use authz::{AuthContext, Relation};
use serde_json::json;
use uuid::Uuid;

use crate::SecretError;

impl super::SecretStore {
    /// 別プリンシパルへ `can_use` を付与する（**owner のみ**・冪等）。
    ///
    /// アプリ所有の資格情報（例: B2 confidential client secret）を、実行時に**アプリの
    /// サービス identity（miniapp subject）**へ使わせるための授権。平文は依然として
    /// `resolve` からしか出ず、宛先束縛・監査もそのまま効く。
    pub async fn grant_can_use(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        subject: &authz::Subject,
        trace_id: Option<&str>,
    ) -> Result<(), SecretError> {
        // owner だけが授権できる（require が owner を確認・不足は Deny 監査）。
        let obj = self
            .require(ctx, id, Relation::Owner, "secret.grant_can_use", trace_id)
            .await?;
        self.authz
            .write_tuple(subject, Relation::CanUse, &obj)
            .await
            .map_err(|e| SecretError::Internal(format!("can_use tuple: {e}")))?;
        self.record_audit(
            ctx,
            "secret.grant_can_use",
            &id.to_string(),
            trace_id,
            json!({ "subject": subject.to_string() }),
        )
        .await?;
        Ok(())
    }
}
