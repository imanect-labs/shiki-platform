//! StorageService: フォルダ作成。
//!
//! `service.rs`（親）が持つ struct/フィールド/自由関数を `use super::*` で参照する。

// 分割した impl ブロック。親 `service.rs` の struct/フィールド/自由関数/型 import を総取りする。
#[allow(clippy::wildcard_imports)]
use super::*;

impl StorageService {
    /// フォルダを作成する（親フォルダ配下 or org ルート直下）。
    ///
    /// 認可は upload と対称: フォルダ配下は `editor@parent`、ルートは `member@org`。
    /// closure（親継承 ＋ self depth0）を張り、FGA に owner（＋folder 配下なら parent）
    /// タプルを書く。DB と FGA は 2PC できないため、tuple は **commit 前**に書き、
    /// parent 失敗・commit 失敗のどちらでも書けた tuple を revoke して不整合を残さない。
    // DB↔FGA の 2PC 不能を補償する commit 前 tuple 書き込み＋失敗時 revoke の一連を
    // 一体で追えるようにするため長め。分割せずグランドファーザ許容する。
    #[allow(clippy::too_many_lines)]
    pub async fn create_folder(
        &self,
        ctx: &AuthContext,
        parent_id: Option<Uuid>,
        name: &str,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        validate_name(name)?;
        match parent_id {
            Some(p) => {
                self.require(
                    ctx,
                    Relation::Editor,
                    &ctx.ns().folder(&p.to_string()),
                    "folder.create",
                    "folder",
                    &p.to_string(),
                    trace_id,
                )
                .await?;
                self.ensure_folder(ctx, p).await?;
            }
            None => {
                self.require(
                    ctx,
                    Relation::Member,
                    &ctx.ns().organization(&ctx.org),
                    "folder.create",
                    "organization",
                    &ctx.org,
                    trace_id,
                )
                .await?;
            }
        }

        let tx_result: Result<Node, StorageError> = async {
            let mut tx = self.db.begin().await?;
            // 親の生存を **txn 内で行ロックして再確認**する（pre-txn の ensure_folder 後に親が
            // soft-delete される race を防ぐ。削除済み親の下に生存子を作らない）。
            if let Some(p) = parent_id {
                let parent_live: Option<String> = sqlx::query_scalar(
                    "SELECT kind FROM node \
                     WHERE id = $1 AND org = $2 AND tenant_id = $3 AND deleted_at IS NULL \
                     FOR UPDATE",
                )
                .bind(p)
                .bind(&ctx.org)
                .bind(&ctx.tenant_id)
                .fetch_optional(&mut *tx)
                .await?;
                match parent_live.as_deref() {
                    Some("folder") => {}
                    Some(_) => {
                        return Err(StorageError::Invalid("親がフォルダではありません".into()))
                    }
                    None => return Err(StorageError::NotFound),
                }
            }
            let sql = format!(
                "INSERT INTO node (org, tenant_id, kind, name, parent_id, created_by) \
                 VALUES ($1, $2, 'folder', $3, $4, $5) RETURNING {NODE_COLS}"
            );
            let row: NodeRow = sqlx::query_as(&sql)
                .bind(&ctx.org)
                .bind(&ctx.tenant_id)
                .bind(name)
                .bind(parent_id)
                .bind(&ctx.principal.id)
                .fetch_one(&mut *tx)
                .await?;
            let folder_id = row.id;
            // 祖先リンク（親の closure を +1 で引き継ぐ）。
            if let Some(p) = parent_id {
                sqlx::query(
                    "INSERT INTO node_closure (tenant_id, org, ancestor, descendant, depth) \
                     SELECT tenant_id, org, ancestor, $1, depth + 1 FROM node_closure \
                     WHERE tenant_id = $3 AND descendant = $2",
                )
                .bind(folder_id)
                .bind(p)
                .bind(&ctx.tenant_id)
                .execute(&mut *tx)
                .await?;
            }
            // 自分自身（depth 0）。
            sqlx::query(
                "INSERT INTO node_closure (tenant_id, org, ancestor, descendant, depth) VALUES ($1, $2, $3, $3, 0)",
            )
            .bind(&ctx.tenant_id)
            .bind(&ctx.org)
            .bind(folder_id)
            .execute(&mut *tx)
            .await?;

            // 監査・書込イベントは **FGA tuple を書く前**に済ませる（post-tuple の失敗で FGA
            // tuple だけが孤立するのを防ぐ。外部副作用の手前で DB 側を全て確定させる）。
            audit::record_on(
                &mut tx,
                ctx,
                AuditEntry {
                    action: "folder.create",
                    object_type: "folder",
                    object_id: &folder_id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({ "parent_id": parent_id.map(|p| p.to_string()) }),
                },
                Chain::Yes,
            )
            .await?;
            // 書込イベント（Task 1.8）。フォルダ作成は索引対象外だが、move/authz 再評価の
            // 将来配線のため経路を統一する（1 行で安価）。
            event::emit_on(
                &mut tx,
                ctx,
                WriteEvent {
                    node_id: folder_id,
                    version: row.version,
                    op: WriteOp::Create,
                    payload: json!({
                        "kind": "folder",
                        "parent_id": parent_id.map(|p| p.to_string()),
                    }),
                },
                trace_id,
            )
            .await?;
            // DB 側が確定したので FGA tuple を書く（commit 前）。
            let folder_obj = ctx.ns().folder(&folder_id.to_string());
            self.authz
                .write_tuple(&ctx.subject(), Relation::Owner, &folder_obj)
                .await
                .map_err(StorageError::Authz)?;
            if let Some(p) = parent_id {
                if let Err(e) = self
                    .authz
                    .write_tuple(
                        &Subject::object(&ctx.ns().folder(&p.to_string())),
                        Relation::Parent,
                        &folder_obj,
                    )
                    .await
                {
                    let _ = self
                        .authz
                        .delete_tuple(&ctx.subject(), Relation::Owner, &folder_obj)
                        .await;
                    return Err(StorageError::Authz(e));
                }
            }
            if let Err(e) = tx.commit().await {
                let _ = self
                    .authz
                    .delete_tuple(&ctx.subject(), Relation::Owner, &folder_obj)
                    .await;
                if let Some(p) = parent_id {
                    let _ = self
                        .authz
                        .delete_tuple(
                            &Subject::object(&ctx.ns().folder(&p.to_string())),
                            Relation::Parent,
                            &folder_obj,
                        )
                        .await;
                }
                return Err(StorageError::from(e));
            }
            row_to_node(row)
        }
        .await;
        tx_result
    }

    /// 親が存在する生存フォルダであることを確認する（org + tenant スコープ）。
    pub(crate) async fn ensure_folder(
        &self,
        ctx: &AuthContext,
        id: Uuid,
    ) -> Result<(), StorageError> {
        let kind: Option<String> = sqlx::query_scalar(
            "SELECT kind FROM node \
             WHERE id = $1 AND org = $2 AND tenant_id = $3 AND deleted_at IS NULL",
        )
        .bind(id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .fetch_optional(&self.db)
        .await?;
        match kind.as_deref() {
            Some("folder") => Ok(()),
            Some(_) => Err(StorageError::Invalid("親がフォルダではありません".into())),
            None => Err(StorageError::NotFound),
        }
    }
}
