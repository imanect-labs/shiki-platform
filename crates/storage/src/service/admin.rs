//! StorageService: テナント admin プレーン（プロビジョニング/撤去）と認可ヘルパ。
//!
//! `service.rs`（親）が持つ struct/フィールド/自由関数を `use super::*` で参照する。

// 分割した impl ブロック。親 `service.rs` の struct/フィールド/自由関数/型 import を総取りする。
#[allow(clippy::wildcard_imports)]
use super::*;

impl StorageService {
    /// テナント初期 admin へ org member タプルを付与する（冪等・監査つき）。
    ///
    /// admin プレーン操作のため呼び出しユーザーの `AuthContext` は無い。実行時と同じ
    /// `AuthContext::ns()` 名前空間経路を通すために合成コンテキストで識別子を組む
    /// （dev_seed と同型）。`actor` には呼び出し主体（provisioner の `azp` 等）を渡し、
    /// 監査ログからどのクライアントの操作か追えるようにする（#91 M-7）。
    pub async fn provision_tenant_admin(
        &self,
        tenant_id: &str,
        org: &str,
        admin_user_id: &str,
        actor: &str,
    ) -> Result<(), StorageError> {
        let ctx = system_ctx(tenant_id, org, actor);
        let subject = ctx.ns().user(admin_user_id);
        self.authz
            .write_tuple(&subject, Relation::Member, &ctx.ns().organization(org))
            .await?;
        self.audit
            .record(
                &ctx,
                AuditEntry {
                    action: "tenant.provision",
                    object_type: "organization",
                    object_id: org,
                    decision: Decision::Allow,
                    trace_id: None,
                    metadata: json!({ "admin_user_id": admin_user_id }),
                },
            )
            .await?;
        Ok(())
    }

    /// テナントの全データを撤去する（SAAS.2 テナント削除・admin プレーン）。
    ///
    /// 撤去順（fail-safe・全段冪等＝途中失敗は再実行で収束）:
    /// 1. **FGA タプル**: DB からオブジェクトを列挙（node / role / org）し、各オブジェクトの
    ///    直接タプルを一括剥奪する。識別子は tenant 名前空間経由なので他テナントに触れない。
    /// 2. **オブジェクトストア**: `{tenant_id}/` prefix 配下をページ列挙しバッチ削除する。
    /// 3. **DB 行**: 1 txn で FK 依存順に物理削除する（closure → version → pending →
    ///    outbox → directory → node → blob）。**audit_log は削除証跡として保持**する。
    ///
    /// 返り値は `(剥奪タプル数, 削除オブジェクト数)`（ログ/レスポンス用の概数）。
    pub async fn purge_tenant(
        &self,
        tenant_id: &str,
        org: &str,
        actor: &str,
    ) -> Result<(u64, u64), StorageError> {
        let ctx = system_ctx(tenant_id, org, actor);
        let ns = ctx.ns();
        let mut tuples_deleted: u64 = 0;

        // 1a. node（file/folder）のタプル。keyset ページングで全行（ゴミ箱含む）を走査。
        let mut last_id: Option<Uuid> = None;
        loop {
            let rows: Vec<(Uuid, String)> = sqlx::query_as(
                "SELECT id, kind FROM node WHERE tenant_id = $1 AND ($2::uuid IS NULL OR id > $2) \
                 ORDER BY id LIMIT 500",
            )
            .bind(tenant_id)
            .bind(last_id)
            .fetch_all(&self.db)
            .await?;
            if rows.is_empty() {
                break;
            }
            last_id = rows.last().map(|(id, _)| *id);
            for (id, kind) in &rows {
                let obj = match kind.as_str() {
                    "folder" => ns.folder(&id.to_string()),
                    _ => ns.file(&id.to_string()),
                };
                tuples_deleted += u64::from(self.authz.delete_object_tuples(&obj).await?);
            }
        }
        // 1b. role のタプル（directory_role が当該テナントの role 台帳）。
        let role_ids: Vec<String> =
            sqlx::query_scalar("SELECT role_id FROM directory_role WHERE tenant_id = $1")
                .bind(tenant_id)
                .fetch_all(&self.db)
                .await?;
        for role_id in &role_ids {
            tuples_deleted += u64::from(self.authz.delete_object_tuples(&ns.role(role_id)).await?);
        }
        // 1c. org のタプル（member）。
        tuples_deleted += u64::from(
            self.authz
                .delete_object_tuples(&ns.organization(org))
                .await?,
        );
        // 1d. artifact のタプル（owner/editor/viewer・workflow プリンシパル委譲含む）。
        //     artifact:<tenant>|<id> を台帳（artifact テーブル）から列挙して撤去する（SAAS.2 完全削除）。
        {
            let mut last: Option<Uuid> = None;
            loop {
                let ids: Vec<Uuid> = sqlx::query_scalar(
                    "SELECT id FROM artifact WHERE tenant_id = $1 \
                     AND ($2::uuid IS NULL OR id > $2) ORDER BY id LIMIT 500",
                )
                .bind(tenant_id)
                .bind(last)
                .fetch_all(&self.db)
                .await?;
                if ids.is_empty() {
                    break;
                }
                last = ids.last().copied();
                for id in &ids {
                    tuples_deleted += u64::from(
                        self.authz
                            .delete_object_tuples(&ns.artifact(&id.to_string()))
                            .await?,
                    );
                }
            }
        }

        // 2. オブジェクトストア: `{tenant_id}/` prefix 配下を全削除。
        let mut objects_deleted: u64 = 0;
        let prefix = format!("{tenant_id}/");
        let mut continuation: Option<String> = None;
        loop {
            let (keys, next) = self
                .store
                .list_prefix(&prefix, continuation.as_deref())
                .await?;
            if !keys.is_empty() {
                objects_deleted += keys.len() as u64;
                self.store.delete_batch(&keys).await?;
            }
            match next {
                Some(token) => continuation = Some(token),
                None => break,
            }
        }

        // 3. DB 行を 1 txn・FK 依存順で物理削除（audit_log は保持）。
        let mut tx = self.db.begin().await?;
        // node_closure は tenant_id を直接持つ（#91 L-1）ため node JOIN 不要。
        sqlx::query("DELETE FROM node_closure WHERE tenant_id = $1")
            .bind(tenant_id)
            .execute(&mut *tx)
            .await?;
        for table in [
            "node_version",
            "pending_upload",
            "storage_event_outbox",
            "directory_user",
            "directory_role",
            "node",
            "blob",
            // artifact 本文（artifact_version は FK cascade で連鎖削除・SAAS.2 完全削除）。
            "artifact",
        ] {
            sqlx::query(&format!("DELETE FROM {table} WHERE tenant_id = $1"))
                .bind(tenant_id)
                .execute(&mut *tx)
                .await?;
        }
        // 撤去の監査（ハッシュチェーン連結・削除証跡）。
        audit::record_on(
            &mut tx,
            &ctx,
            AuditEntry {
                action: "tenant.purge",
                object_type: "organization",
                object_id: org,
                decision: Decision::Allow,
                trace_id: None,
                metadata: json!({
                    "tuples_deleted": tuples_deleted,
                    "objects_deleted": objects_deleted,
                    "roles": role_ids.len(),
                }),
            },
            Chain::Yes,
        )
        .await?;
        tx.commit().await?;
        Ok((tuples_deleted, objects_deleted))
    }

    // --- 内部ヘルパ ---

    /// 認可 check（deny は監査して Forbidden）。
    #[allow(clippy::too_many_arguments)] // check + 監査記録に必要なフィールド一式。
    pub(crate) async fn require(
        &self,
        ctx: &AuthContext,
        relation: Relation,
        object: &FgaObject,
        action: &str,
        object_type: &str,
        object_id: &str,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        // 書込/管理系の権限判定。即時剥奪反映は不要なので低レイテンシ既定で問い合わせる。
        let allowed = self
            .authz
            .check(
                &ctx.subject(),
                relation,
                object,
                Consistency::MinimizeLatency,
            )
            .await?;
        if !allowed {
            self.audit
                .record(
                    ctx,
                    AuditEntry {
                        action,
                        object_type,
                        object_id,
                        decision: Decision::Deny,
                        trace_id,
                        metadata: json!({ "relation": relation.as_str() }),
                    },
                )
                .await?;
            return Err(StorageError::Forbidden);
        }
        Ok(())
    }

    /// 読取系の viewer 認可。deny は**存在を秘匿**するため `NotFound` を返す（403/404 で
    /// 私有ファイルの存在が漏れないようにする・P2-6）。deny の監査は残す。
    pub(crate) async fn require_read(
        &self,
        ctx: &AuthContext,
        object: &FgaObject,
        action: &str,
        object_type: &str,
        object_id: &str,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        // 読取認可。共有解除の即時反映（PIT-11）が要るため強整合で問い合わせる。
        let allowed = self
            .authz
            .check(
                &ctx.subject(),
                Relation::Viewer,
                object,
                Consistency::HigherConsistency,
            )
            .await?;
        if !allowed {
            self.audit
                .record(
                    ctx,
                    AuditEntry {
                        action,
                        object_type,
                        object_id,
                        decision: Decision::Deny,
                        trace_id,
                        metadata: json!({ "relation": Relation::Viewer.as_str() }),
                    },
                )
                .await?;
            return Err(StorageError::NotFound);
        }
        Ok(())
    }
}
