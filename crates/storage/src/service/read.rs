//! StorageService: メタ取得・子一覧・breadcrumb（読み取り経路）。
//!
//! `service.rs`（親）が持つ struct/フィールド/自由関数を `use super::*` で参照する。

// 分割した impl ブロック。親 `service.rs` の struct/フィールド/自由関数/型 import を総取りする。
use super::node_scan::ScanWhere;
#[allow(clippy::wildcard_imports)]
use super::*;
use crate::audit::AuditAction;
use std::collections::HashSet;

impl StorageService {
    /// ファイルメタを取得する（viewer 権限が要る）。
    pub async fn get_metadata(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        self.get_metadata_annotated(ctx, file_id, trace_id, &serde_json::Map::new())
            .await
    }

    /// [`Self::get_metadata`] に、許可・拒否の監査 metadata へ書き足す値を添える
    /// （チャットの回答のための読み出しなら会話・回答の ID・#829）。
    pub async fn get_metadata_annotated(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        trace_id: Option<&str>,
        annotations: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Node, StorageError> {
        let node = self.load_node(ctx, file_id, false).await?;
        // 共有リンクの遅延失効（#342・defense-in-depth）: 期限切れなら FGA タプルを先行剥奪して
        // から viewer 判定する（新規 open が期限後のタプルで通らないようにする）。
        self.enforce_share_link_expiry(ctx, file_id, node.kind)
            .await?;
        self.require_read_annotated(
            ctx,
            &ctx.ns().file(&file_id.to_string()),
            AuditAction::FileMetadataRead,
            "file",
            &file_id.to_string(),
            trace_id,
            annotations,
        )
        .await?;
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: AuditAction::FileMetadataRead,
                    object_type: "file",
                    object_id: &file_id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: serde_json::Value::Object(annotations.clone()),
                },
            )
            .await?;
        Ok(node)
    }

    // --- 変更系 ---

    /// フォルダの子を**権限フィルタ済み**で 1 ページ返す（PIT-13）。
    ///
    /// `parent_id` が `None` なら org ルート直下。`limit` は 1..=100 にクランプ。
    /// `sort`（name/updated/size×方向）を **keyset カーソルに織り込んで**サーバ側で並べる。
    /// クライアント側の全件ソートは採らない（全件取得の禁止）。
    ///
    /// 権限フィルタは **子の候補を BatchCheck でまとめて OpenFGA viewer の post-filter に通す**（読めない子は読み飛ばす・
    /// [`super::node_scan`]）。継承を pre-filter にした「親が読めれば全子可視」の最適化は採らない:
    /// move 直後は DB の `parent_id` が先に見え、新親の FGA `parent` タプルが遅延し得るため、
    /// DB 親子関係を認可の近道にすると未認可の子を露出し得る（FGA を真実とする）。
    ///
    /// 1 回の頼みで OpenFGA に問う数は [`super::node_scan::NODE_SCAN_MAX_CHECKS`] 問まで（BUG-187）。届いたら、
    /// `limit` より少ない（空のこともある）ページに続きの位置を付けて返す。`next_cursor` が `Some` なら続きがある
    /// （末尾ちょうどで空ページが 1 回返ることはあるが、欠落や重複は起きない）。続きの位置は見えない子の名前を
    /// 含みうる内部の値なので、外へ出す API は封をする（`data::DataStore::seal_list_cursor`）。
    #[allow(clippy::too_many_arguments)] // 認可 ctx ＋ 親 ＋ ソート ＋ ページング ＋ 可視性 ＋ trace。
    pub async fn list_children(
        &self,
        ctx: &AuthContext,
        parent_id: Option<Uuid>,
        sort: ChildSort,
        cursor: Option<&str>,
        limit: usize,
        include_system: bool,
        trace_id: Option<&str>,
    ) -> Result<ChildPage, StorageError> {
        // 親の閲覧可否を先に確認（ルートは org メンバー）。読めない親は存在秘匿で空扱い。
        match parent_id {
            Some(p) => {
                self.ensure_folder(ctx, p).await?;
                // 共有リンクの遅延失効（#368/A-3・defense-in-depth）: 親フォルダの期限切れ broad
                // タプルを先行剥奪してから viewer 判定する（get_metadata と同じ経路を列挙にも通す）。
                // 子ごとの遅延失効は列挙のホットパスに reconcile を撒くコストが高いため通さない
                // （子はイベント駆動タイマ＋各 open 時の get_metadata で eventual に失効する）。
                self.enforce_share_link_expiry(ctx, p, NodeKind::Folder)
                    .await?;
                self.require_read(
                    ctx,
                    &ctx.ns().folder(&p.to_string()),
                    AuditAction::FolderChildrenList,
                    "folder",
                    &p.to_string(),
                    trace_id,
                )
                .await?;
            }
            None => {
                self.require(
                    ctx,
                    Relation::Member,
                    &ctx.ns().organization(&ctx.org),
                    AuditAction::FolderChildrenList,
                    "organization",
                    &ctx.org,
                    trace_id,
                )
                .await?;
            }
        }

        // `include_system=false`（ドライブ UI 等）はシステム領域（#392）を除外する。
        // true はエージェント自身のワークスペース列挙だけが使う（chat::StorageWorkspaceStore）。
        let (items, next_cursor) = self
            .scan_nodes(
                ctx,
                &ScanWhere::Children {
                    parent_id,
                    include_system,
                },
                sort,
                cursor,
                limit,
                &HashSet::new(),
            )
            .await?;
        // 成功した一覧（ディレクトリ列挙）も監査に残す（NFR-6・読取系なので未チェーン）。
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: AuditAction::FolderChildrenList,
                    object_type: "folder",
                    object_id: &parent_id.map_or_else(|| "root".into(), |p| p.to_string()),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({ "returned": items.len() }),
                },
            )
            .await?;
        Ok(ChildPage { items, next_cursor })
    }

    /// フォルダ横断の**名前検索**（権限フィルタ済み・keyset ページング）。
    ///
    /// 空白区切りの各語を AND の部分一致（ILIKE・ワイルドカード無害化）で名前へ適用する。
    /// `ILIKE ALL(array)` は ESCAPE 句を取れないため、既定のエスケープ文字 `\` を前提に
    /// `escape_like` が `\` で無害化した語をそのまま使う。
    /// 認可は org メンバー確認 → 自分が viewer のファイルとフォルダー（ListObjects）の中だけを名前に当てる。どちらかが
    /// 上限に届いたら、候補を語で選ばずに並びの順に引いて BatchCheck で絞る（OpenFGA に問う数は語によらず、
    /// [`super::node_scan::NODE_SCAN_MAX_CHECKS`] 問まで・BUG-187・[`super::node_scan`]）。上限に届いたら、`limit`
    /// より少ない（空のこともある）ページに続きの位置を付けて返す。内容検索（RAG `/search`）とフロントで統合される
    /// 想定で、こちらは名前一致のみを担う。
    pub async fn search_nodes_by_name(
        &self,
        ctx: &AuthContext,
        query: &str,
        sort: ChildSort,
        cursor: Option<&str>,
        limit: usize,
        trace_id: Option<&str>,
    ) -> Result<ChildPage, StorageError> {
        self.require(
            ctx,
            Relation::Member,
            &ctx.ns().organization(&ctx.org),
            AuditAction::NodeSearch,
            "organization",
            &ctx.org,
            trace_id,
        )
        .await?;

        let terms: Vec<String> = query
            .split_whitespace()
            .map(|t| format!("%{}%", crate::directory::escape_like(t)))
            .collect();
        if terms.is_empty() {
            return Ok(ChildPage {
                items: vec![],
                next_cursor: None,
            });
        }

        // 名前検索はユーザー向けの導線のみ（システム領域は常に対象外・#392）。
        // エージェントのワークスペースは名前解決（`resolve_child_file`）で引く。
        let (known, cut) = self.visible_nodes(ctx).await?;
        let (items, next_cursor) = if cut {
            self.scan_nodes(
                ctx,
                &ScanWhere::Named { terms: &terms },
                sort,
                cursor,
                limit,
                &known,
            )
            .await?
        } else {
            let ids: Vec<Uuid> = known.into_iter().collect();
            self.search_in_visible(ctx, &terms, sort, cursor, limit, &ids)
                .await?
        };
        // 検索も列挙操作として監査へ（クエリ本文は含めない: 低エントロピー語の露出回避）。
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: AuditAction::NodeSearch,
                    object_type: "organization",
                    object_id: &ctx.org,
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({ "returned": items.len() }),
                },
            )
            .await?;
        Ok(ChildPage { items, next_cursor })
    }

    /// ノードのパンくず（祖先列）を root→自身の順で返す（**読める接尾のみ**）。
    ///
    /// 自身の viewer を確認後、closure の祖先を**自身→上**（depth 昇順）に辿り、読めない祖先に
    /// 当たった時点で打ち切る。これにより返すのは「自身から上方向に連続して読める範囲」＝
    /// 読める接尾（contiguous suffix ending at self）であり、読めない祖先名は一切漏れない。
    /// 直接共有でルート祖先が読めない場合は、読める範囲（最小で自身のみ）だけを返す。
    pub async fn breadcrumb(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Vec<Crumb>, StorageError> {
        let node = self.load_node(ctx, node_id, false).await?;
        self.require_read(
            ctx,
            &node_fga_object(&ctx.ns(), node.kind, node_id),
            AuditAction::NodeBreadcrumbRead,
            node.kind.as_str(),
            &node_id.to_string(),
            trace_id,
        )
        .await?;
        // 祖先（自身含む）を 自身→root の順（depth 昇順）で取得する。
        let rows: Vec<(Uuid, String, String, i32)> = sqlx::query_as(
            "SELECT n.id, n.name, n.kind, c.depth \
             FROM node_closure c JOIN node n ON n.id = c.ancestor AND n.tenant_id = c.tenant_id \
             WHERE c.tenant_id = $3 AND c.descendant = $1 AND n.org = $2 AND n.deleted_at IS NULL \
             ORDER BY c.depth ASC",
        )
        .bind(node_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .fetch_all(&self.db)
        .await?;

        // 自身（depth 0）から上へ。読めない祖先に当たったら打ち切る（読める接尾のみ）。
        let mut crumbs = Vec::with_capacity(rows.len());
        for (id, name, kind, _depth) in rows {
            let kind = NodeKind::parse(&kind)
                .ok_or_else(|| StorageError::Integrity(format!("未知のノード種別: {kind}")))?;
            if id != node_id {
                let allowed = self
                    .authz
                    .check(
                        &ctx.subject(),
                        Relation::Viewer,
                        &node_fga_object(&ctx.ns(), kind, id),
                        Consistency::HigherConsistency,
                    )
                    .await?;
                if !allowed {
                    break;
                }
            }
            crumbs.push(Crumb { id, name, kind });
        }
        // 自身→root で積んだので、表示順（root→自身）へ反転する。
        crumbs.reverse();
        // 成功読取を監査に残す（NFR-6・読取系なので未チェーン）。
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: AuditAction::NodeBreadcrumbRead,
                    object_type: node.kind.as_str(),
                    object_id: &node_id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({ "depth": crumbs.len() }),
                },
            )
            .await?;
        Ok(crumbs)
    }

    /// org + tenant スコープでノードを 1 件読む。
    pub(crate) async fn load_node(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        include_deleted: bool,
    ) -> Result<Node, StorageError> {
        let sql = if include_deleted {
            format!("SELECT {NODE_COLS} FROM node WHERE id = $1 AND org = $2 AND tenant_id = $3")
        } else {
            format!(
                "SELECT {NODE_COLS} FROM node \
                 WHERE id = $1 AND org = $2 AND tenant_id = $3 AND deleted_at IS NULL"
            )
        };
        let row: Option<NodeRow> = sqlx::query_as(&sql)
            .bind(id)
            .bind(&ctx.org)
            .bind(&ctx.tenant_id)
            .fetch_optional(&self.db)
            .await?;
        row.map(row_to_node)
            .transpose()?
            .ok_or(StorageError::NotFound)
    }
}
