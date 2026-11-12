//! インストールのライフサイクル操作（Task 9.13b・`install.rs` から分割・行数規約）。
//!
//! アンインストール・オフライン import・補償/監査/outbox ヘルパ。本体（同意インストール）は
//! [`crate::install`]。

use storage::audit::AuditAction;
use storage::audit::{AuditEntry, Decision};
use storage::event::{emit_on, WriteEvent, WriteOp};
use uuid::Uuid;

use authz::{AuthContext, Relation};

use crate::install::map_gateway;
use crate::manifest::ServerSpec;
use crate::sign::verify_manifest_signature;
use crate::{AppPlatformError, InstallService, MiniAppManifest};

/// B2 secret の参照名（SecretStore 内・テナント毎に一意）。
pub(crate) fn b2_secret_name(app_id: Uuid) -> String {
    format!("miniapp-b2-{app_id}")
}

/// cron 式（5 つの欄: 分 時 日 月 曜日）から、`after` より後の次の実行の時刻を求める（UTC で読む）。
///
/// 曜日は標準の読み方（0 と 7 が日曜、1 が月曜〜6 が土曜。名前 SUN〜SAT も書ける）で、ワークフローの
/// スケジュールと同じ写像（`shiki-cron5`）を通る。cron クレートへそのまま渡すと 1 を日曜と読み、`1-5`（平日）が
/// 日曜〜木曜に動いていた（BUG-129・BUG-95 と同じずれ）。マニフェストの式は時間帯を持たないので UTC で読む。
/// 読めない式は Invalid（文言は利用者向け。下位の原文は埋め込まない）。
pub fn next_cron_run_after(
    expr: &str,
    after: chrono::DateTime<chrono::Utc>,
) -> Result<chrono::DateTime<chrono::Utc>, AppPlatformError> {
    match cron5::next_after(expr, "UTC", after) {
        Ok(Some(next)) => Ok(next),
        Ok(None) => Err(AppPlatformError::Invalid(format!(
            "cron の式（{expr}）には次の実行がありません。"
        ))),
        Err(e) => {
            tracing::debug!(error = %e, expr, "cron の式を読めない");
            Err(AppPlatformError::Invalid(format!(
                "cron の式（{expr}）を読めません。{}",
                e.user_message()
            )))
        }
    }
}

impl InstallService {
    /// B2 confidential secret を保管する（宛先束縛=token endpoint・best-effort＝
    /// secrets 未配線ではスキップして warn。トリガ/ユーザー起点実行は secret 必須のため
    /// その環境では 502 になる）。
    pub(crate) async fn store_b2_secret(&self, ctx: &AuthContext, app_id: Uuid, secret: &str) {
        let Some(store) = &self.secrets else {
            tracing::warn!(%app_id, "secrets 未配線: B2 client secret を保管できません（B2 実行は無効）");
            return;
        };
        let Some(id) = self.upsert_b2_secret(store, ctx, app_id, secret).await else {
            return;
        };
        // 実行時の解決主体はアプリの service identity（miniapp）。ユーザー起動/トリガ双方が
        // この principal で resolve するため、can_use を miniapp へ付与する（installer 本人
        // 以外・サービス起動でも解決できるようにする）。
        let app_ctx = AuthContext::for_miniapp(
            ctx.tenant().clone(),
            ctx.org().to_owned(),
            &app_id.to_string(),
        );
        if let Err(e) = store.grant_can_use(ctx, id, &app_ctx.subject(), None).await {
            tracing::error!(error = %e, %app_id, "B2 secret の can_use@miniapp 付与に失敗");
        }
    }

    /// B2 secret を作成（既存なら rotate）し、その id を返す（失敗は None＋error ログ）。
    async fn upsert_b2_secret(
        &self,
        store: &secrets::SecretStore,
        ctx: &AuthContext,
        app_id: Uuid,
        secret: &str,
    ) -> Option<Uuid> {
        let name = b2_secret_name(app_id);
        let bytes = secret.as_bytes().to_vec();
        let create_err = match store
            .create(
                ctx,
                secrets::NewSecret {
                    name: name.clone(),
                    plaintext: bytes.clone(),
                    allowed_hosts: self.token_host.iter().cloned().collect(),
                },
                None,
            )
            .await
        {
            Ok(meta) => return Some(meta.id),
            Err(e) => e,
        };
        // 作成失敗＝既存（再インストール）想定。id を引いて rotate で上書きする。
        let metas = match store.list_mine(ctx).await {
            Ok(m) => m,
            Err(e) => {
                tracing::error!(error = %create_err, list_error = %e, %app_id, "B2 secret 一覧取得に失敗");
                return None;
            }
        };
        let Some(meta) = metas.into_iter().find(|m| m.name == name) else {
            tracing::error!(error = %create_err, %app_id, "B2 secret の再解決に失敗");
            return None;
        };
        if let Err(e) = store.rotate(ctx, meta.id, bytes, None).await {
            tracing::error!(error = %create_err, rotate_error = %e, %app_id, "B2 secret の保管に失敗");
        }
        Some(meta.id)
    }

    /// event 購読と cron スケジュールをインストール時ピンから実体化する。
    pub(crate) async fn provision_triggers(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
        spec: &ServerSpec,
    ) -> Result<(), AppPlatformError> {
        // 冪等: 再インストールは作り直し（同意内容の入れ替え）。
        self.remove_triggers(ctx, app_id).await;
        for event_type in &spec.events {
            for function in &spec.functions {
                sqlx::query(
                    "INSERT INTO app_event_subscription (tenant_id, org, app_id, event_type, function) \
                     VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
                )
                .bind(ctx.tenant_id())
                .bind(ctx.org())
                .bind(app_id)
                .bind(event_type)
                .bind(function)
                .execute(&self.db)
                .await
                .map_err(crate::map_db)?;
            }
        }
        for entry in &spec.cron {
            if !spec.functions.contains(&entry.function) {
                return Err(AppPlatformError::Invalid(format!(
                    "cron の関数 {} は server.functions に宣言されていません",
                    entry.function
                )));
            }
            let next = next_cron_run_after(&entry.expr, chrono::Utc::now())?;
            sqlx::query(
                "INSERT INTO app_function_schedule \
                     (tenant_id, org, app_id, function, expr, next_run_at) \
                 VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
            )
            .bind(ctx.tenant_id())
            .bind(ctx.org())
            .bind(app_id)
            .bind(&entry.function)
            .bind(&entry.expr)
            .bind(next)
            .execute(&self.db)
            .await
            .map_err(crate::map_db)?;
        }
        Ok(())
    }

    /// 購読/スケジュールと、自動実行の直近の失敗（GAP-60）を撤去する（アンインストール・再インストール前の掃除）。
    pub(crate) async fn remove_triggers(&self, ctx: &AuthContext, app_id: Uuid) {
        for table in [
            "app_event_subscription",
            "app_function_schedule",
            "app_function_failure",
        ] {
            if let Err(e) = sqlx::query(&format!(
                "DELETE FROM {table} WHERE tenant_id = $1 AND app_id = $2"
            ))
            .bind(ctx.tenant_id())
            .bind(app_id)
            .execute(&self.db)
            .await
            {
                tracing::warn!(error = %e, table, "トリガ撤去に失敗");
            }
        }
    }

    /// アンインストール: 失効（即時 403）→ テーブル archive ＋ FGA tuple 撤去 → client 無効化。
    pub async fn uninstall(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(), AppPlatformError> {
        self.require_artifact_owner(ctx, app_id).await?;
        // 失効を先に（gateway は次のリクエストから 403＝トークン有効期限内でも遮断）。
        let installation = self
            .installations
            .resolve_active_by_app(ctx.tenant_id(), app_id)
            .await
            .map_err(map_gateway)?;
        self.installations
            .revoke(ctx, app_id)
            .await
            .map_err(map_gateway)?;

        // 所有テーブルを archive（soft delete）し tuple を撤去する。
        let tables = self.data.table_ids_owned_by_app(ctx, app_id).await?;
        for id in &tables {
            if let Err(e) = self.data.delete_table(ctx, *id, trace_id).await {
                tracing::warn!(error = %e, table_id = %id, "アンインストール時のテーブル archive に失敗");
            }
            let obj = ctx.ns().data_table(&id.to_string());
            if let Err(e) = self.authz.delete_object_tuples(&obj).await {
                tracing::warn!(error = %e, table_id = %id, "アンインストール時の tuple 撤去に失敗");
            }
            // 撤去した ReBAC を配信へ知らせる（archive 済みでも開いている購読を次のバッチで落とす）。
            if let Err(e) = self.data.notify_authz_changed(ctx, Some(*id)).await {
                tracing::warn!(error = %e, table_id = %id, "アンインストール時の配信の世代更新に失敗");
            }
        }
        if let Some(inst) = &installation {
            self.disable_clients_best_effort(
                inst.client_id_b1.as_deref(),
                inst.client_id_b2.as_deref(),
            )
            .await;
        }
        // 購読・スケジュールと自動実行の直近の失敗を消す（migration 0046 の決まり。これまでは残り、自動実行が
        // 失効したインストールを引いて空振りしていた・GAP-60）。
        self.remove_triggers(ctx, app_id).await;
        self.record_audit(
            ctx,
            app_id,
            AuditAction::AppUninstall,
            Decision::Allow,
            trace_id,
        )
        .await;
        self.emit_app_event(
            ctx,
            app_id,
            "app.uninstalled",
            serde_json::json!({}),
            trace_id,
        )
        .await;
        Ok(())
    }

    /// アプリの実体（mini_app_code のアーティファクト）を消した後の片付け（削除の API が commit の後に呼ぶ・GAP-69a）。
    ///
    /// もう入れられないアプリの Keycloak のクライアント（`app-<app_id>-b1`・`-b2`・[`Self::delete_app_clients`]）と、
    /// B2 の secret の保管（[`b2_secret_name`]）を消す。アンインストールはクライアントを無効にするだけ（入れ直すと同じ
    /// id を使う）なので、アプリを消さないとたまり続けていた。
    ///
    /// - アーティファクトがまだ消えていない・有効なインストールが残っている（入れたアプリは動き続ける）ときは何もしない。
    ///   そのインストールをアンインストールした後は、起動時の照合（[`Self::repair_oauth_clients`]）がクライアントを消す。
    /// - secret は owner（インストールした人）の ctx で消す。インストールはアーティファクトの owner に限るので、削除の
    ///   API を呼んだ owner の ctx で消せる。呼ぶ人の secret に無ければ何もしない（ほかの人の secret は消せない）。
    /// - どの失敗も削除を戻さない（best-effort・warn）。消せなかったクライアントは起動時の照合がやり直す。
    pub async fn after_app_deleted(&self, ctx: &AuthContext, app_id: Uuid, trace_id: Option<&str>) {
        match self.no_longer_installable(ctx, app_id).await {
            Ok(true) => {}
            Ok(false) => return,
            Err(e) => {
                tracing::warn!(error = %e, %app_id, "消したアプリの片付けの前の確かめに失敗（起動時の照合がクライアントを消す）");
                return;
            }
        }
        if let Some(oauth) = &self.oauth {
            let (b1, b2) = (format!("app-{app_id}-b1"), format!("app-{app_id}-b2"));
            if let Err(e) = self
                .delete_app_clients(oauth, ctx.tenant_id(), app_id, Some(&b1), Some(&b2))
                .await
            {
                tracing::warn!(error = %e, %app_id, "消したアプリのクライアントを Keycloak から消せません（起動時の照合がやり直す）");
            }
        }
        self.delete_b2_secret(ctx, app_id, trace_id).await;
    }

    /// アーティファクトを消し（mini_app_code の行が残り、消した印がある）、有効なインストールも無いか。
    async fn no_longer_installable(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
    ) -> Result<bool, AppPlatformError> {
        let deleted: Option<bool> = sqlx::query_scalar(
            "SELECT deleted_at IS NOT NULL FROM artifact \
             WHERE tenant_id = $1 AND id = $2 AND kind = 'mini_app_code'",
        )
        .bind(ctx.tenant_id())
        .bind(app_id)
        .fetch_optional(&self.db)
        .await
        .map_err(crate::map_db)?;
        if deleted != Some(true) {
            return Ok(false);
        }
        let active = self
            .installations
            .resolve_active_by_app(ctx.tenant_id(), app_id)
            .await
            .map_err(map_gateway)?;
        if active.is_some() {
            tracing::info!(%app_id, "消したアプリに有効なインストールが残るので、クライアントと secret を残します");
            return Ok(false);
        }
        Ok(true)
    }

    /// B2 の secret の保管を、呼ぶ人（secret の owner）の ctx で消す（無ければ何もしない・失敗は warn）。
    async fn delete_b2_secret(&self, ctx: &AuthContext, app_id: Uuid, trace_id: Option<&str>) {
        let Some(store) = &self.secrets else { return };
        let name = b2_secret_name(app_id);
        let metas = match store.list_mine(ctx).await {
            Ok(metas) => metas,
            Err(e) => {
                tracing::warn!(error = %e, %app_id, "消したアプリの B2 の secret を探せません");
                return;
            }
        };
        let Some(meta) = metas.into_iter().find(|m| m.name == name) else {
            return;
        };
        if let Err(e) = store.delete(ctx, meta.id, trace_id).await {
            tracing::warn!(error = %e, %app_id, "消したアプリの B2 の secret を消せません");
        }
    }

    /// オフライン（エアギャップ）import: 署名検証 → artifact 作成 → 不変 publish。
    ///
    /// 署名は**常に必須**（ネット非依存の信頼根＝信頼鍵台帳）。検証に成功した場合のみ
    /// 呼出ユーザーを owner として artifact を作る。
    pub async fn import_signed(
        &self,
        ctx: &AuthContext,
        manifest: MiniAppManifest,
        signature: &[u8],
        key_id: &str,
        trace_id: Option<&str>,
    ) -> Result<crate::RegistryEntry, AppPlatformError> {
        let key = self
            .keys
            .find_active(ctx, key_id)
            .await?
            .ok_or(AppPlatformError::Forbidden)?;
        verify_manifest_signature(&manifest, signature, &key)?;
        let id = self.code.create(ctx, &manifest, trace_id).await?;
        let entry = self
            .code
            .publish(ctx, id, None, Some(signature), trace_id)
            .await?;
        self.record_audit(ctx, id, AuditAction::AppImport, Decision::Allow, trace_id)
            .await;
        Ok(entry)
    }

    pub(crate) async fn require_artifact_owner(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
    ) -> Result<(), AppPlatformError> {
        let obj = ctx.ns().artifact(&app_id.to_string());
        let ok = self
            .authz
            .check(
                &ctx.subject(),
                Relation::Owner,
                &obj,
                authz::Consistency::HigherConsistency,
            )
            .await
            .map_err(|e| AppPlatformError::Internal(format!("authz: {e}")))?;
        if !ok {
            return Err(AppPlatformError::Forbidden);
        }
        Ok(())
    }

    /// 補償: 作成済みテーブルの削除＋tuple 撤去（best-effort・欠落は tracing）。
    pub(crate) async fn compensate_tables(
        &self,
        ctx: &AuthContext,
        created: &[Uuid],
        trace_id: Option<&str>,
    ) {
        for id in created {
            if let Err(e) = self.data.delete_table(ctx, *id, trace_id).await {
                tracing::error!(error = %e, table_id = %id, "インストール補償のテーブル削除に失敗");
            }
            let obj = ctx.ns().data_table(&id.to_string());
            if let Err(e) = self.authz.delete_object_tuples(&obj).await {
                tracing::error!(error = %e, table_id = %id, "インストール補償の tuple 撤去に失敗");
            }
        }
    }

    pub(crate) async fn disable_clients_best_effort(&self, b1: Option<&str>, b2: Option<&str>) {
        let Some(oauth) = &self.oauth else { return };
        for id in [b1, b2].into_iter().flatten() {
            if let Err(e) = oauth.set_enabled(id, false).await {
                tracing::warn!(error = %e, client_id = id, "client 無効化に失敗");
            }
        }
    }

    pub(crate) async fn record_installed_event(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
        manifest: &MiniAppManifest,
        tables: &[Uuid],
        trace_id: Option<&str>,
    ) {
        self.record_audit(
            ctx,
            app_id,
            AuditAction::AppInstall,
            Decision::Allow,
            trace_id,
        )
        .await;
        self.emit_app_event(
            ctx,
            app_id,
            "app.installed",
            serde_json::json!({ "name": manifest.name, "version": manifest.version, "tables": tables }),
            trace_id,
        )
        .await;
    }

    pub(crate) async fn record_audit(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
        action: AuditAction,
        decision: Decision,
        trace_id: Option<&str>,
    ) {
        if let Err(e) = self
            .audit
            .record(
                ctx,
                AuditEntry {
                    action,
                    object_type: "miniapp",
                    object_id: &app_id.to_string(),
                    decision,
                    trace_id,
                    metadata: serde_json::json!({ "security": decision == Decision::Deny }),
                },
            )
            .await
        {
            tracing::warn!(error = %e, "インストール監査の記録に失敗");
        }
    }

    pub(crate) async fn audit_deny(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
        action: AuditAction,
        trace_id: Option<&str>,
    ) {
        self.record_audit(ctx, app_id, action, Decision::Deny, trace_id)
            .await;
    }

    /// outbox へアプリライフサイクルイベントを発行する（best-effort・単発 Tx）。
    pub(crate) async fn emit_app_event(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
        event_type: &str,
        mut payload: serde_json::Value,
        trace_id: Option<&str>,
    ) {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("event_type".into(), serde_json::json!(event_type));
            obj.insert("app_id".into(), serde_json::json!(app_id));
        }
        let result = async {
            let mut tx = self.db.begin().await?;
            emit_on(
                &mut tx,
                ctx,
                WriteEvent {
                    node_id: app_id,
                    version: 1,
                    op: WriteOp::Update,
                    payload,
                },
                trace_id,
            )
            .await
            .map_err(|e| sqlx::Error::Protocol(e.to_string()))?;
            tx.commit().await
        }
        .await;
        if let Err(e) = result {
            tracing::warn!(error = %e, event_type, "outbox 発行に失敗");
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{b2_secret_name, next_cron_run_after};
    use crate::AppPlatformError;

    #[test]
    fn b2_secret_name_is_app_scoped() {
        let a = uuid::Uuid::nil();
        assert_eq!(b2_secret_name(a), format!("miniapp-b2-{a}"));
        // 別アプリは別名（テナント内衝突なし）。
        assert_ne!(b2_secret_name(uuid::Uuid::new_v4()), b2_secret_name(a));
    }

    #[test]
    fn next_cron_run_after_normalizes_five_fields() {
        // 毎日 09:00（5 フィールド）＝秒 0 補完で解釈される。
        let after = chrono::DateTime::parse_from_rfc3339("2026-07-09T08:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let next = next_cron_run_after("0 9 * * *", after).expect("valid cron");
        assert_eq!(next.to_rfc3339(), "2026-07-09T09:00:00+00:00");
        // 同日 09:00 を過ぎていれば翌日へ送る。
        let after2 = chrono::DateTime::parse_from_rfc3339("2026-07-09T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let next2 = next_cron_run_after("0 9 * * *", after2).expect("valid cron");
        assert_eq!(next2.to_rfc3339(), "2026-07-10T09:00:00+00:00");
    }

    /// 曜日は標準の読み方（1 が月曜・0 と 7 が日曜）。cron クレートの番号（1 が日曜）で読まない（BUG-129）。
    #[test]
    fn next_cron_run_after_reads_weekdays_like_workflows() {
        use chrono::Datelike;
        // 2026-10-25 は日曜。そこから 1 週間に実行する曜日（0＝日曜〜6＝土曜）。
        let weekdays = |expr: &str| {
            let mut at = chrono::DateTime::parse_from_rfc3339("2026-10-24T23:59:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc);
            let end = at + chrono::Duration::days(7);
            let mut days = Vec::new();
            loop {
                at = next_cron_run_after(expr, at).expect("valid cron");
                if at > end {
                    break days;
                }
                days.push(at.weekday().num_days_from_sunday());
            }
        };
        assert_eq!(weekdays("0 9 * * 1-5"), [1, 2, 3, 4, 5], "平日");
        assert_eq!(weekdays("0 9 * * MON-FRI"), [1, 2, 3, 4, 5]);
        assert_eq!(weekdays("0 9 * * 1"), [1], "月曜");
        assert_eq!(weekdays("0 9 * * 0"), [0], "日曜（0）も書ける");
        assert_eq!(weekdays("0 9 * * 7"), [0], "7 も日曜");
        assert_eq!(weekdays("0 9 * * SAT,SUN"), [0, 6]);
        // ワークフローのスケジュールと同じ時刻を返す（UTC で読む）。
        let after = chrono::DateTime::parse_from_rfc3339("2026-10-25T03:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(
            Some(next_cron_run_after("30 8 * * 1-5", after).unwrap()),
            cron5::next_after("30 8 * * 1-5", "UTC", after).unwrap()
        );
    }

    #[test]
    fn next_cron_run_after_rejects_malformed_expr() {
        let after = chrono::Utc::now();
        assert!(matches!(
            next_cron_run_after("not a cron", after),
            Err(AppPlatformError::Invalid(_))
        ));
        // フィールド不足も不正。
        assert!(matches!(
            next_cron_run_after("0 9", after),
            Err(AppPlatformError::Invalid(_))
        ));
        // 標準の読み方で写せない曜日（範囲の逆転）も不正。文言に下位の原文を入れない。
        match next_cron_run_after("0 9 * * 5-1", after) {
            Err(AppPlatformError::Invalid(m)) => {
                assert!(m.contains("SUN〜SAT"), "{m}");
                assert!(!m.contains("BadDayOfWeek"), "{m}");
            }
            other => panic!("{other:?}"),
        }
    }
}
