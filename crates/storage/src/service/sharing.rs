//! StorageService: 共有（ReBAC タプル付与/剥奪・共有一覧）。
//!
//! `service.rs`（親）が持つ struct/フィールド/自由関数を `use super::*` で参照する。

// 分割した impl ブロック。親 `service.rs` の struct/フィールド/自由関数/型 import を総取りする。
#[allow(clippy::wildcard_imports)]
use super::*;

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
            .authorize_share_admin(ctx, node_id, "node.share", trace_id)
            .await?;
        // 付与は冪等。granted=true なら実際に新規付与した。
        let granted = self
            .authz
            .write_tuple(&target.subject(&ctx.ns()), role.relation(), &obj)
            .await?;
        if let Err(e) = self
            .record_share_audit(ctx, node_id, &obj, "node.share", target, role, trace_id)
            .await
        {
            // 実際に付与した時だけ巻き戻す（no-op を剥奪して既存共有を壊さない）。
            if granted {
                let _ = self
                    .authz
                    .delete_tuple(&target.subject(&ctx.ns()), role.relation(), &obj)
                    .await;
            }
            return Err(e);
        }
        Ok(())
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
            .authorize_share_admin(ctx, node_id, "node.unshare", trace_id)
            .await?;
        // 剥奪は冪等。revoked=true なら実際に剥奪した。
        let revoked = self
            .authz
            .delete_tuple(&target.subject(&ctx.ns()), role.relation(), &obj)
            .await?;
        if let Err(e) = self
            .record_share_audit(ctx, node_id, &obj, "node.unshare", target, role, trace_id)
            .await
        {
            // 実際に剥奪した時だけ巻き戻す（no-op を付与して権限昇格を起こさない）。
            if revoked {
                let _ = self
                    .authz
                    .write_tuple(&target.subject(&ctx.ns()), role.relation(), &obj)
                    .await;
            }
            return Err(e);
        }
        Ok(())
    }

    /// このノードの共有相手一覧を返す（owner 権限）。
    ///
    /// オブジェクトに**直接**書かれた viewer/editor タプルのみを返す（owner/parent や
    /// 親フォルダからの継承は含めない＝「このノードで誰に共有したか」の管理ビュー）。
    pub async fn list_shares(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Vec<ShareEntry>, StorageError> {
        let obj = self
            .authorize_share_admin(ctx, node_id, "node.shares.list", trace_id)
            .await?;
        let tuples = self.authz.read_tuples(&obj, None).await?;
        let mut entries = Vec::new();
        for t in tuples {
            // viewer/editor のみ共有として扱う（owner/parent は管理対象外）。
            let Some(role) = Relation::parse(&t.relation).and_then(ShareRole::from_relation) else {
                continue;
            };
            let Some(target) = ShareTarget::parse_subject(&ctx.ns(), &t.user) else {
                continue;
            };
            entries.push(ShareEntry { target, role });
        }
        Ok(entries)
    }

    /// 自分に共有されたノード一覧（自分が作成したものを除く・org+tenant スコープ）。
    ///
    /// OpenFGA の `list-objects`（viewer 実効集合・継承込み）で id を引き、DB で生存ノードの
    /// メタへ keyset `(updated_at, id)` 降順で 1 ページ解決する。作成者本人のノード（≒owner）は
    /// 「共有された」一覧から除く。全件取得はせず `next_cursor` で無限スクロールする。
    pub async fn list_shared_with_me(
        &self,
        ctx: &AuthContext,
        cursor: Option<&str>,
        limit: usize,
        trace_id: Option<&str>,
    ) -> Result<ChildPage, StorageError> {
        let limit = limit.clamp(1, 100);
        let subject = ctx.subject();
        let mut ids: Vec<Uuid> = Vec::new();
        for object_type in [ObjectType::File, ObjectType::Folder] {
            let objs = self
                .authz
                .list_objects(&subject, Relation::Viewer, object_type)
                .await?;
            for o in objs {
                // "file:<tenant>|<uuid>" / "folder:<tenant>|<uuid>" から自テナントの id 部を取り出す。
                // strip_object_id が tenant 不一致を弾くため、共用ストアでも越境オブジェクトは混入しない
                // （FGA 側の名前空間化に加え、DB 側 org+tenant フィルタと二重防御）。
                let Some((_, id_part)) = o.split_once(':') else {
                    continue;
                };
                if let Some(local) = ctx.ns().strip_object_id(id_part) {
                    if let Ok(uuid) = Uuid::parse_str(local) {
                        ids.push(uuid);
                    }
                }
            }
        }
        if ids.is_empty() {
            return Ok(ChildPage {
                items: Vec::new(),
                next_cursor: None,
            });
        }
        let (after_ts, after_id) = match cursor {
            Some(c) => {
                let (ts, id) = decode_ts_cursor(c)?;
                (Some(ts), Some(id))
            }
            None => (None, None),
        };
        // FGA の viewer 集合（id）を DB メタへ keyset ページングで解決する。
        let sql = format!(
            "SELECT {NODE_COLS} FROM node \
             WHERE id = ANY($1) AND org = $2 AND tenant_id = $3 \
               AND deleted_at IS NULL AND created_by <> $4 \
               AND ($5::text IS NULL OR (updated_at, id) < ($5::timestamptz, $6)) \
             ORDER BY updated_at DESC, id DESC LIMIT $7"
        );
        let rows: Vec<NodeRow> = sqlx::query_as(&sql)
            .bind(&ids)
            .bind(&ctx.org)
            .bind(&ctx.tenant_id)
            .bind(&ctx.principal.id)
            .bind(after_ts.as_deref())
            .bind(after_id)
            .bind(limit as i64)
            .fetch_all(&self.db)
            .await?;
        let next_cursor = if rows.len() == limit {
            rows.last()
                .map(|r| encode_ts_cursor(&r.updated_at.to_rfc3339(), r.id))
        } else {
            None
        };
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: "node.shared_with_me.list",
                    object_type: "organization",
                    object_id: &ctx.org,
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({ "count": rows.len() }),
                },
            )
            .await?;
        let items = rows
            .into_iter()
            .map(row_to_node)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ChildPage { items, next_cursor })
    }

    /// 共有管理（share/unshare/list）の前段: ノードの存在確認＋owner 認可。FGA object を返す。
    async fn authorize_share_admin(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        action: &str,
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

    /// 共有/解除の監査を**ハッシュチェーンに連結**して記録する（権限変更は改竄検知対象）。
    #[allow(clippy::too_many_arguments)] // 監査記録に必要なフィールド一式。
    async fn record_share_audit(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        obj: &FgaObject,
        action: &str,
        target: &ShareTarget,
        role: ShareRole,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let object_type = if obj.as_str().starts_with("folder:") {
            "folder"
        } else {
            "file"
        };
        let mut tx = self.db.begin().await?;
        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action,
                object_type,
                object_id: &node_id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata: json!({
                    "target": target,
                    "role": role,
                }),
            },
            Chain::Yes,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    // --- テナント・プロビジョニング/撤去（SAAS.2 / #87・admin プレーン） ---
}
