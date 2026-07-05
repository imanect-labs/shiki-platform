//! StorageService: メタ更新・移動/リネーム（closure 張り替え）。
//!
//! `service.rs`（親）が持つ struct/フィールド/自由関数を `use super::*` で参照する。

// 分割した impl ブロック。親 `service.rs` の struct/フィールド/自由関数/型 import を総取りする。
#[allow(clippy::wildcard_imports)]
use super::*;

impl StorageService {
    /// ノード（ファイル/フォルダ）のリネーム・移動を **1 トランザクションで原子的に**適用する。
    ///
    /// `expect` でファイル/フォルダ種別を強制し（種別違いは存在秘匿の `NotFound`）、
    /// `new_name`: 指定でリネーム。`new_parent`: `Some(Some(p))` で `p` 配下へ、
    /// `Some(None)` でルートへ移動、`None` で移動しない。move と rename を一度に指定しても
    /// 部分適用にならない。
    ///
    /// 移動はサブツリー全体の closure を張り替え、**循環（自身の配下への移動）を拒否**する。
    /// PIT-16: 移動サブツリー ∪ 移動先の祖先列を id 昇順ロックした単一 txn で更新する。
    // `new_parent` は「移動しない / ルートへ / 指定親へ」の三状態を表す意図的な
    // Option<Option<_>>。単一 txn で closure 張り替え＋各種ゲートを行うため長くなるが、
    // 分割すると txn 境界と循環検出の不変条件が読みづらくなるため一体に保つ。
    #[allow(clippy::too_many_lines, clippy::option_option)]
    async fn update_node(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        expect: NodeKind,
        new_name: Option<&str>,
        new_parent: Option<Option<Uuid>>,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        if new_name.is_none() && new_parent.is_none() {
            return Err(StorageError::Invalid("変更内容がありません".into()));
        }
        if let Some(name) = new_name {
            validate_name(name)?;
        }
        // 早期の存在＋種別＋tenant ゲート（最新状態はロック後に再読込する・Lb76B）。
        let existing = self.load_node(ctx, node_id, false).await?;
        if existing.kind != expect {
            return Err(StorageError::NotFound);
        }
        let self_obj = node_fga_object(&ctx.ns(), expect, node_id);
        let action = update_action(expect);
        let id_str = node_id.to_string();
        // 対象ノードの editor 権限。
        self.require(
            ctx,
            Relation::Editor,
            &self_obj,
            action,
            expect.as_str(),
            &id_str,
            trace_id,
        )
        .await?;
        // 移動する場合は移動先の権限＋実在を確認。
        if let Some(target) = new_parent {
            match target {
                Some(p) => {
                    if p == node_id {
                        return Err(StorageError::Invalid("自分自身へは移動できません".into()));
                    }
                    self.require(
                        ctx,
                        Relation::Editor,
                        &ctx.ns().folder(&p.to_string()),
                        action,
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
                        action,
                        "organization",
                        &ctx.org,
                        trace_id,
                    )
                    .await?;
                }
            }
        }

        let mut tx = self.db.begin().await?;
        // PIT-16: 移動時は「移動サブツリー ∪ 移動先の祖先列」を id 昇順ロック（祖先ロック下の単一 txn）。
        // rename だけなら対象 1 行で足りる。
        let lock_ids: Vec<Uuid> = if new_parent.is_some() {
            let mut ids: Vec<Uuid> = sqlx::query_scalar(
                "SELECT descendant FROM node_closure WHERE tenant_id = $1 AND org = $2 AND ancestor = $3",
            )
            .bind(&ctx.tenant_id)
            .bind(&ctx.org)
            .bind(node_id)
            .fetch_all(&mut *tx)
            .await?;
            if let Some(Some(p)) = new_parent {
                let anc: Vec<Uuid> = sqlx::query_scalar(
                    "SELECT ancestor FROM node_closure WHERE tenant_id = $1 AND org = $2 AND descendant = $3",
                )
                .bind(&ctx.tenant_id)
                .bind(&ctx.org)
                .bind(p)
                .fetch_all(&mut *tx)
                .await?;
                ids.extend(anc);
            }
            ids
        } else {
            vec![node_id]
        };
        sqlx::query(
            "SELECT id FROM node \
             WHERE id = ANY($1) AND org = $2 AND tenant_id = $3 ORDER BY id FOR UPDATE",
        )
        .bind(&lock_ids)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .fetch_all(&mut *tx)
        .await?;

        // Lb76B: ロック取得後に**最新状態を再読込**する。並行 move とオーバーラップした際、
        // ロック前に読んだ stale な親/名前を使うと、FGA の parent タプルが DB とずれる
        // （旧フォルダの継承アクセスが残る）ため、ロック下の最新行から計算する。
        let fresh_sql = format!(
            "SELECT {NODE_COLS} FROM node \
             WHERE id = $1 AND org = $2 AND tenant_id = $3 AND deleted_at IS NULL"
        );
        let fresh: NodeRow = sqlx::query_as(&fresh_sql)
            .bind(node_id)
            .bind(&ctx.org)
            .bind(&ctx.tenant_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StorageError::NotFound)?;
        let old_parent = fresh.parent_id;
        let final_parent = match new_parent {
            Some(target) => target,
            None => fresh.parent_id,
        };
        let final_name = new_name.unwrap_or(fresh.name.as_str());
        let parent_changed = new_parent.is_some() && final_parent != old_parent;

        // 移動先の生存を**ロック下で再確認**する（pre-txn の ensure_folder 後に移動先が
        // soft-delete される race を防ぐ。削除済みフォルダ配下へ生存ノードを移さない）。
        if parent_changed {
            if let Some(p) = final_parent {
                let target_live: Option<String> = sqlx::query_scalar(
                    "SELECT kind FROM node \
                     WHERE id = $1 AND org = $2 AND tenant_id = $3 AND deleted_at IS NULL",
                )
                .bind(p)
                .bind(&ctx.org)
                .bind(&ctx.tenant_id)
                .fetch_optional(&mut *tx)
                .await?;
                match target_live.as_deref() {
                    Some("folder") => {}
                    Some(_) => {
                        return Err(StorageError::Invalid(
                            "移動先がフォルダではありません".into(),
                        ))
                    }
                    None => return Err(StorageError::NotFound),
                }
            }
        }

        // 循環拒否: 移動先が自身の配下（closure で ancestor=self に含まれる）なら拒否する。
        // ロック下で判定し、並行移動でサブツリーが入れ替わっても閉路を作らせない。
        if parent_changed {
            if let Some(p) = final_parent {
                let is_descendant: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM node_closure \
                     WHERE tenant_id = $1 AND ancestor = $2 AND descendant = $3)",
                )
                .bind(&ctx.tenant_id)
                .bind(node_id)
                .bind(p)
                .fetch_one(&mut *tx)
                .await?;
                if is_descendant {
                    return Err(StorageError::Invalid(
                        "フォルダを自身の配下へは移動できません".into(),
                    ));
                }
            }
        }

        // version をインクリメント（メタ変更を後段の write-event/索引が検知できるように）。
        let sql = format!(
            "UPDATE node SET name = $1, parent_id = $2, version = version + 1, updated_at = now() \
             WHERE id = $3 AND org = $4 AND tenant_id = $5 AND deleted_at IS NULL RETURNING {NODE_COLS}"
        );
        let row: NodeRow = sqlx::query_as(&sql)
            .bind(final_name)
            .bind(final_parent)
            .bind(node_id)
            .bind(&ctx.org)
            .bind(&ctx.tenant_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StorageError::NotFound)?;

        // closure 書換（親が変わった時のみ・サブツリー全体）:
        //   1. 移動サブツリーの各ノードから、移動ノードの旧祖先へのリンクを切る（サブツリー内部は保つ）。
        //   2. 新親（とその祖先）から移動サブツリー各ノードへ depth を足して張り直す（cross join）。
        // 葉（ファイル）はサブツリー＝自身のみなので既存挙動と一致する。
        if parent_changed {
            sqlx::query(
                "DELETE FROM node_closure \
                 WHERE tenant_id = $2 \
                   AND descendant IN (SELECT descendant FROM node_closure WHERE tenant_id = $2 AND ancestor = $1) \
                   AND ancestor IN (SELECT ancestor FROM node_closure WHERE tenant_id = $2 AND descendant = $1 AND ancestor <> $1)",
            )
            .bind(node_id)
            .bind(&ctx.tenant_id)
            .execute(&mut *tx)
            .await?;
            if let Some(p) = final_parent {
                sqlx::query(
                    "INSERT INTO node_closure (tenant_id, org, ancestor, descendant, depth) \
                     SELECT sup.tenant_id, sup.org, sup.ancestor, sub.descendant, sup.depth + sub.depth + 1 \
                     FROM node_closure sup CROSS JOIN node_closure sub \
                     WHERE sup.tenant_id = $3 AND sub.tenant_id = $3 \
                       AND sup.descendant = $1 AND sub.ancestor = $2",
                )
                .bind(p)
                .bind(node_id)
                .bind(&ctx.tenant_id)
                .execute(&mut *tx)
                .await?;
            }
        }
        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action,
                object_type: expect.as_str(),
                object_id: &id_str,
                decision: Decision::Allow,
                trace_id,
                metadata: json!({
                    "renamed": new_name.is_some(),
                    "moved": parent_changed,
                    "old_parent": old_parent.map(|p| p.to_string()),
                    "new_parent": final_parent.map(|p| p.to_string()),
                }),
            },
            Chain::Yes,
        )
        .await?;
        // 書込イベント（Task 1.8）。move は authz_tags 再評価、rename はメタ更新を後段に伝える。
        event::emit_on(
            &mut tx,
            ctx,
            WriteEvent {
                node_id,
                version: row.version,
                op: if parent_changed {
                    WriteOp::Move
                } else {
                    WriteOp::Rename
                },
                payload: json!({
                    "kind": expect.as_str(),
                    "renamed": new_name.is_some(),
                    "moved": parent_changed,
                    "old_parent": old_parent.map(|p| p.to_string()),
                    "new_parent": final_parent.map(|p| p.to_string()),
                }),
            },
            trace_id,
        )
        .await?;
        // FGA parent タプルは過剰権限を生まない順序で更新する（DB と FGA は 2PC できないため、
        // どの失敗点でも fail-safe へ倒し、かつ**冪等な再試行で収束**できるようにする）:
        //   1. 旧親の剥奪は **コミット前**（剥奪失敗ならロールバック＝旧親経由の継続アクセスを残さない）。
        //   2. 新親の付与は **コミット後**かつ **final_parent がある限り常に**実行する。
        //      `write_tuple` は冪等（既存は成功扱い）なので、付与失敗時に同じ move を再試行すれば
        //      `parent_changed=false` でも新親タプルを張り直して修復できる（Lb76A）。
        //      コミット前付与は移動未確定時の先行アクセスを生むため避ける（Lb76C/LbiSj）。
        // 移動するのは**自ノードの parent タプルのみ**（子は OpenFGA の `from parent` 継承で追従）。
        // 完全失敗時の残留（新親未付与＝過小権限）は再試行 or 書込イベント/outbox（Task 1.8）で収束。
        if parent_changed {
            if let Some(op) = old_parent {
                self.authz
                    .delete_tuple(
                        &Subject::object(&ctx.ns().folder(&op.to_string())),
                        Relation::Parent,
                        &self_obj,
                    )
                    .await?; // 失敗 → tx は drop でロールバック（移動なし＝整合）
            }
        }
        if let Err(e) = tx.commit().await {
            // 移動が確定しなかったので、剥奪済みの旧親タプルを復元（冪等・best-effort）。
            if parent_changed {
                if let Some(op) = old_parent {
                    let _ = self
                        .authz
                        .write_tuple(
                            &Subject::object(&ctx.ns().folder(&op.to_string())),
                            Relation::Parent,
                            &self_obj,
                        )
                        .await;
                }
            }
            return Err(StorageError::from(e));
        }
        // 現在の親（folder）への parent タプルを冪等に保証する（再試行で修復可能）。
        if let Some(np) = final_parent {
            self.authz
                .write_tuple(
                    &Subject::object(&ctx.ns().folder(&np.to_string())),
                    Relation::Parent,
                    &self_obj,
                )
                .await?;
        }
        row_to_node(row)
    }

    /// ファイルのリネーム・移動（[`update_node`](Self::update_node) の種別固定ラッパ）。
    pub async fn update_file(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        new_name: Option<&str>,
        new_parent: Option<Option<Uuid>>,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        self.update_node(ctx, file_id, NodeKind::File, new_name, new_parent, trace_id)
            .await
    }

    /// フォルダのリネーム・移動（[`update_node`](Self::update_node) の種別固定ラッパ）。
    pub async fn update_folder(
        &self,
        ctx: &AuthContext,
        folder_id: Uuid,
        new_name: Option<&str>,
        new_parent: Option<Option<Uuid>>,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        self.update_node(
            ctx,
            folder_id,
            NodeKind::Folder,
            new_name,
            new_parent,
            trace_id,
        )
        .await
    }

    /// リネーム（[`update_file`](Self::update_file) の薄いラッパ）。
    pub async fn rename_file(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        new_name: &str,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        self.update_file(ctx, file_id, Some(new_name), None, trace_id)
            .await
    }

    /// 移動（[`update_file`](Self::update_file) の薄いラッパ）。
    pub async fn move_file(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        new_parent: Option<Uuid>,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        self.update_file(ctx, file_id, None, Some(new_parent), trace_id)
            .await
    }

    /// フォルダのリネーム（[`update_folder`](Self::update_folder) の薄いラッパ）。
    pub async fn rename_folder(
        &self,
        ctx: &AuthContext,
        folder_id: Uuid,
        new_name: &str,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        self.update_folder(ctx, folder_id, Some(new_name), None, trace_id)
            .await
    }

    /// フォルダの移動（[`update_folder`](Self::update_folder) の薄いラッパ）。
    pub async fn move_folder(
        &self,
        ctx: &AuthContext,
        folder_id: Uuid,
        new_parent: Option<Uuid>,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        self.update_folder(ctx, folder_id, None, Some(new_parent), trace_id)
            .await
    }
}
