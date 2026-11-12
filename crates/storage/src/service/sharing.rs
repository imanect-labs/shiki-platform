//! StorageService: 共有（ReBAC タプル付与/剥奪・共有一覧）。
//!
//! `service.rs`（親）が持つ struct/フィールド/自由関数を `use super::*` で参照する。

// 分割した impl ブロック。親 `service.rs` の struct/フィールド/自由関数/型 import を総取りする。
#[allow(clippy::wildcard_imports)]
use super::*;
use crate::audit::AuditAction;
use crate::tuple_commit::run_to_end;

/// 共有先 subject の検証。subject 識別子（`user:<tenant>|<id>` / `role:<tenant>|<id>#member`）を
/// 壊す/曖昧化する id を弾く。文字ポリシーの正本は [`authz::validate_local_id`]
/// （`:`/`#`＝型/userset 区切り・`|`＝tenant 名前空間区切り・制御文字・空を拒否。単一定義・#91 M-3）。
/// 前後空白の拒否（trim で往復が変わる混乱の防止）だけは共有 API 固有のルールとしてここで足す。
///
/// role の**存在**（当該テナントに実在するロールか）はここでは強制しない: 全メンバーのログイン前でも
/// 部署（AD group 由来 role）へ共有できるようにするため（dangling grant 回避は共有ダイアログの
/// `directory_role` オートコンプリートで担保し、厳格な存在ゲートは IdP フル同期後に足す）。
fn validate_share_target(target: &ShareTarget) -> Result<(), StorageError> {
    let id = match target {
        ShareTarget::User { id } | ShareTarget::Role { id } => id,
    };
    if id != id.trim() {
        return Err(StorageError::Invalid(
            "共有先 id の前後に空白は使えません".into(),
        ));
    }
    authz::validate_local_id(id)
        .map_err(|violation| StorageError::Invalid(format!("共有先 id が不正です: {violation}")))?;
    Ok(())
}

/// 共有の付け外しのどちらか。
#[derive(Debug, Clone, Copy)]
enum ShareChange {
    Grant,
    Revoke,
}

impl ShareChange {
    fn action(self) -> AuditAction {
        match self {
            ShareChange::Grant => AuditAction::NodeShare,
            ShareChange::Revoke => AuditAction::NodeUnshare,
        }
    }
}

/// 共有の付け外しの監査に残す中身（別のタスクへ渡すので借りずに持つ）。
struct ShareAudit {
    node_id: Uuid,
    obj: FgaObject,
    target: ShareTarget,
    role: ShareRole,
    trace_id: Option<String>,
}

impl ShareAudit {
    fn new(
        node_id: Uuid,
        obj: FgaObject,
        target: &ShareTarget,
        role: ShareRole,
        trace_id: Option<&str>,
    ) -> Self {
        ShareAudit {
            node_id,
            obj,
            target: target.clone(),
            role,
            trace_id: trace_id.map(str::to_owned),
        }
    }

    /// 共有/解除の監査を**ハッシュチェーンに連結**して記録する（権限変更は改竄検知対象）。
    async fn record(
        &self,
        recorder: &AuditRecorder,
        ctx: &AuthContext,
        action: AuditAction,
    ) -> Result<(), StorageError> {
        let object_type = if self.obj.as_str().starts_with("folder:") {
            "folder"
        } else {
            "file"
        };
        recorder
            .record_chained(
                ctx,
                AuditEntry {
                    action,
                    object_type,
                    object_id: &self.node_id.to_string(),
                    decision: Decision::Allow,
                    trace_id: self.trace_id.as_deref(),
                    metadata: json!({
                        "target": self.target,
                        "role": self.role,
                    }),
                },
            )
            .await
    }
}

impl StorageService {
    /// ファイル/フォルダを **user** へ viewer/editor で共有する（role 共有は #76 で defer）。
    ///
    /// 共有の管理（ACL 付与）は **owner 権限**を要求する（editor が再共有して権限を
    /// 横展開する confused-deputy を防ぐ）。OpenFGA の tuple 付与として実装する。
    ///
    /// FGA と監査 DB は別 durability 境界のため、**監査失敗時は付与した tuple を補償剥奪**して
    /// 「ACL は変わったが監査が無い」状態を残さない。ただし補償は **実際に付与したとき
    /// （`write_tuple` が `true`）のみ**行う。冪等 no-op（既共有の再共有）を巻き戻すと
    /// 既存共有を誤って剥奪してしまうため（idempotent 補償の逆破壊を防ぐ）。
    pub async fn share_node(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        target: &ShareTarget,
        role: ShareRole,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        validate_share_target(target)?;
        let obj = self
            .authorize_share_admin(ctx, node_id, AuditAction::NodeShare, trace_id)
            .await?;
        self.change_share(
            ctx,
            ShareChange::Grant,
            ShareAudit::new(node_id, obj, target, role, trace_id),
        )
        .await
    }

    /// 共有を解除する（owner 権限・冪等）。
    ///
    /// PIT-11: read 認可は HIGHER_CONSISTENCY で問い合わせるため、剥奪は次リクエストから即時に効く。
    /// 監査失敗時は剥奪を補償付与するが、**実際に剥奪したとき（`delete_tuple` が `true`）のみ**。
    /// 冪等 no-op（未共有の unshare）を巻き戻すと存在しなかった権限を新規付与してしまうため。
    pub async fn unshare_node(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        target: &ShareTarget,
        role: ShareRole,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        validate_share_target(target)?;
        let obj = self
            .authorize_share_admin(ctx, node_id, AuditAction::NodeUnshare, trace_id)
            .await?;
        self.change_share(
            ctx,
            ShareChange::Revoke,
            ShareAudit::new(node_id, obj, target, role, trace_id),
        )
        .await
    }

    /// 共有の付け外しの FGA の区間（タプルの付け外し → 監査 → 監査の失敗で戻す）を、別のタスクで最後まで
    /// 走らせる（BUG-134）。
    ///
    /// 区間の途中で呼ぶ側のフューチャが捨てられる（HTTP の切断・チャットの中止）と、監査の無い共有の付与・
    /// 解除が残る（NFR-6）。タスクにしても補償の決まりは変えない: 監査に失敗したら、実際に変えたとき
    /// （付与は `write_tuple`・解除は `delete_tuple` が `true`）だけ戻す。タプルの付け外しの誤りはそのまま返す。
    async fn change_share(
        &self,
        ctx: &AuthContext,
        change: ShareChange,
        audit: ShareAudit,
    ) -> Result<(), StorageError> {
        let authz = self.authz.clone();
        let recorder = self.audit.clone();
        let ctx = ctx.clone();
        let subject = audit.target.subject(&ctx.ns());
        let relation = audit.role.relation();
        run_to_end(async move {
            let obj = &audit.obj;
            // 付け外しは冪等。changed=true なら実際に変えた。
            let changed = match change {
                ShareChange::Grant => authz.write_tuple(&subject, relation, obj).await?,
                ShareChange::Revoke => authz.delete_tuple(&subject, relation, obj).await?,
            };
            if let Err(e) = audit.record(&recorder, &ctx, change.action()).await {
                // 実際に変えた時だけ戻す（no-op を巻き戻して既存共有を壊す・権限昇格を起こすのを防ぐ）。
                if changed {
                    let _ = match change {
                        ShareChange::Grant => authz.delete_tuple(&subject, relation, obj).await,
                        ShareChange::Revoke => authz.write_tuple(&subject, relation, obj).await,
                    };
                }
                return Err(e);
            }
            Ok(())
        })
        .await
    }

    /// このノードの共有相手一覧を返す（owner 権限）。
    ///
    /// オブジェクトに**直接**書かれた viewer/editor タプルのみを返す（owner/parent や
    /// 親フォルダからの継承は含めない＝「このノードで誰に共有したか」の管理ビュー）。
    /// 相手の表示名（user は email も）は呼び出し元テナント（＋ org）のディレクトリから 1 クエリで
    /// 一括解決する（#741）。解決失敗は非致命（名前は表示補助・一覧自体は返す）。
    pub async fn list_shares(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Vec<ShareEntry>, StorageError> {
        let obj = self
            .authorize_share_admin(ctx, node_id, AuditAction::NodeSharesList, trace_id)
            .await?;
        let tuples = self.authz.read_tuples(&obj, None).await?;
        let mut shares = Vec::new();
        for t in tuples {
            // viewer/editor のみ共有として扱う（owner/parent は管理対象外）。
            let Some(role) = Relation::parse(&t.relation).and_then(ShareRole::from_relation) else {
                continue;
            };
            let Some(target) = ShareTarget::parse_subject(&ctx.ns(), &t.user) else {
                continue;
            };
            shares.push((target, role));
        }
        let profiles = crate::directory::resolve_share_profiles(
            &self.db,
            ctx,
            shares.iter().map(|(t, _)| t),
        )
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "共有相手の表示名解決に失敗（ID 表示へフォールバック）");
            crate::directory::ShareProfiles::default()
        });
        Ok(shares
            .into_iter()
            .map(|(target, role)| {
                let p = profiles.get(&target);
                ShareEntry {
                    target,
                    role,
                    display_name: p.display_name,
                    email: p.email,
                }
            })
            .collect())
    }

    /// 共有管理（share/unshare/list）の前段: ノードの存在確認＋owner 認可。FGA object を返す。
    /// 一般アクセス（#338）の set/clear/get も同じ owner ゲートを再利用する（confused-deputy 防御）。
    pub(crate) async fn authorize_share_admin(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        action: AuditAction,
        trace_id: Option<&str>,
    ) -> Result<FgaObject, StorageError> {
        let node = self.load_node(ctx, node_id, false).await?;
        let obj = node_fga_object(&ctx.ns(), node.kind, node_id);
        self.require(
            ctx,
            Relation::Owner,
            &obj,
            action,
            node.kind.as_str(),
            &node_id.to_string(),
            trace_id,
        )
        .await?;
        Ok(obj)
    }

    // --- テナント・プロビジョニング/撤去（SAAS.2 / #87・admin プレーン） ---
}

#[cfg(test)]
mod tests {
    use super::validate_share_target;
    use crate::model::ShareTarget;

    #[test]
    fn validate_share_target_rejects_bad_ids() {
        // user / role とも同一ルール。正常系（role は AD group 由来の `/` を含んでも可）。
        assert!(validate_share_target(&ShareTarget::User { id: "alice".into() }).is_ok());
        assert!(validate_share_target(&ShareTarget::Role { id: "sales".into() }).is_ok());
        assert!(validate_share_target(&ShareTarget::Role {
            id: "sales/team-1".into()
        })
        .is_ok());
        // 異常系（`|`＝tenant 区切りも拒否）。
        for bad in ["", " alice", "alice ", "a:b", "a#member", "a|b", "bad\nid"] {
            assert!(
                validate_share_target(&ShareTarget::User { id: bad.into() }).is_err(),
                "user should reject {bad:?}"
            );
            assert!(
                validate_share_target(&ShareTarget::Role { id: bad.into() }).is_err(),
                "role should reject {bad:?}"
            );
        }
    }
}
