//! StorageService: メタ取得・子一覧・breadcrumb（読み取り経路）。
//!
//! `service.rs`（親）が持つ struct/フィールド/自由関数を `use super::*` で参照する。

// 分割した impl ブロック。親 `service.rs` の struct/フィールド/自由関数/型 import を総取りする。
#[allow(clippy::wildcard_imports)]
use super::*;

impl StorageService {
    /// ファイルメタを取得する（viewer 権限が要る）。
    pub async fn get_metadata(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        let node = self.load_node(ctx, file_id, false).await?;
        self.require_read(
            ctx,
            &ctx.ns().file(&file_id.to_string()),
            "file.metadata.read",
            "file",
            &file_id.to_string(),
            trace_id,
        )
        .await?;
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: "file.metadata.read",
                    object_type: "file",
                    object_id: &file_id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({}),
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
    /// `next_cursor` が `Some` なら続きがある（末尾ちょうどで空ページが 1 回返ることはあるが、
    /// 欠落や重複は起きない）。クライアント側の全件ソートは採らない（全件取得の禁止）。
    ///
    /// 権限フィルタは **子ごとに OpenFGA viewer を post-filter**する（読めない子はオーバーフェッチで
    /// 読み飛ばす）。継承を pre-filter にした「親が読めれば全子可視」の最適化は採らない:
    /// move 直後は DB の `parent_id` が先に見え、新親の FGA `parent` タプルが遅延し得るため、
    /// DB 親子関係を認可の近道にすると未認可の子を露出し得る（FGA を真実とする）。
    pub async fn list_children(
        &self,
        ctx: &AuthContext,
        parent_id: Option<Uuid>,
        sort: ChildSort,
        cursor: Option<&str>,
        limit: usize,
        trace_id: Option<&str>,
    ) -> Result<ChildPage, StorageError> {
        // 親の閲覧可否を先に確認（ルートは org メンバー）。読めない親は存在秘匿で空扱い。
        match parent_id {
            Some(p) => {
                self.ensure_folder(ctx, p).await?;
                self.require_read(
                    ctx,
                    &ctx.ns().folder(&p.to_string()),
                    "folder.children.list",
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
                    "folder.children.list",
                    "organization",
                    &ctx.org,
                    trace_id,
                )
                .await?;
            }
        }

        let limit = limit.clamp(1, 100);
        // 1 ラウンドのフェッチ歩幅（フィルタ落ちを見越して多めに引く）。
        let batch: i64 = (limit as i64 * 2).clamp(16, 200);
        // ソートキーごとの列式・型キャスト・方向。keyset 比較とカーソルをこれに合わせる。
        let (sort_col, sort_cast) = match sort.key {
            ChildSortKey::Name => ("name", "text"),
            ChildSortKey::Updated => ("updated_at", "timestamptz"),
            // フォルダは size_bytes が NULL のため 0 とみなす（NULL を keyset から排除）。
            ChildSortKey::Size => ("coalesce(size_bytes, 0)", "bigint"),
        };
        let (order_dir, keyset_cmp) = if sort.desc {
            ("DESC", "<")
        } else {
            ("ASC", ">")
        };
        let (mut after_val, mut after_id) = match cursor {
            Some(c) => {
                let (val, id) = decode_child_cursor(sort, c)?;
                (Some(val), Some(id))
            }
            None => (None, None),
        };

        let mut items: Vec<Node> = Vec::with_capacity(limit);
        let mut exhausted = false;
        while items.len() < limit && !exhausted {
            // keyset: (sort_col, id) cmp (after_val, after_id)。parent_id は IS NOT DISTINCT FROM で
            // NULL（ルート）も同値比較する。after_val は text で受けて列型へキャストして比較する。
            let sql = format!(
                "SELECT {NODE_COLS} FROM node \
                 WHERE org = $1 AND tenant_id = $2 AND deleted_at IS NULL \
                   AND parent_id IS NOT DISTINCT FROM $3 \
                   AND ($4::text IS NULL OR ({sort_col}, id) {keyset_cmp} ($4::{sort_cast}, $5)) \
                 ORDER BY {sort_col} {order_dir}, id {order_dir} LIMIT $6"
            );
            let rows: Vec<NodeRow> = sqlx::query_as(&sql)
                .bind(&ctx.org)
                .bind(&ctx.tenant_id)
                .bind(parent_id)
                .bind(after_val.as_deref())
                .bind(after_id)
                .bind(batch)
                .fetch_all(&self.db)
                .await?;
            if (rows.len() as i64) < batch {
                exhausted = true;
            }
            if rows.is_empty() {
                break;
            }
            for row in rows {
                after_val = Some(child_sort_value(sort.key, &row));
                after_id = Some(row.id);
                // 子ごとに viewer を確認（FGA を真実とする post-filter）。即時剥奪反映のため強整合。
                let kind = NodeKind::parse(&row.kind).unwrap_or(NodeKind::File);
                let allowed = self
                    .authz
                    .check(
                        &ctx.subject(),
                        Relation::Viewer,
                        &node_fga_object(&ctx.ns(), kind, row.id),
                        Consistency::HigherConsistency,
                    )
                    .await?;
                if !allowed {
                    continue;
                }
                items.push(row_to_node(row)?);
                if items.len() == limit {
                    break;
                }
            }
        }
        // limit 充足で止めたなら続きがあり得る → カーソルを返す。尽きたなら None。
        let next_cursor = if items.len() == limit {
            match (after_val, after_id) {
                (Some(v), Some(i)) => Some(encode_child_cursor(sort, &v, i)),
                _ => None,
            }
        } else {
            None
        };
        // 成功した一覧（ディレクトリ列挙）も監査に残す（NFR-6・読取系なので未チェーン）。
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: "folder.children.list",
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
    /// 認可は `list_children` と同じ方針: org メンバー確認 → **子ごとに OpenFGA viewer を
    /// post-filter**（強整合・読めないものはオーバーフェッチで読み飛ばす）。内容検索
    /// （RAG `/search`）とフロントで統合される想定で、こちらは名前一致のみを担う。
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
            "node.search",
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

        let limit = limit.clamp(1, 100);
        let batch: i64 = (limit as i64 * 2).clamp(16, 200);
        let (sort_col, sort_cast) = match sort.key {
            ChildSortKey::Name => ("name", "text"),
            ChildSortKey::Updated => ("updated_at", "timestamptz"),
            ChildSortKey::Size => ("coalesce(size_bytes, 0)", "bigint"),
        };
        let (order_dir, keyset_cmp) = if sort.desc {
            ("DESC", "<")
        } else {
            ("ASC", ">")
        };
        let (mut after_val, mut after_id) = match cursor {
            Some(c) => {
                let (val, id) = decode_child_cursor(sort, c)?;
                (Some(val), Some(id))
            }
            None => (None, None),
        };

        let mut items: Vec<Node> = Vec::with_capacity(limit);
        let mut exhausted = false;
        while items.len() < limit && !exhausted {
            let sql = format!(
                "SELECT {NODE_COLS} FROM node \
                 WHERE org = $1 AND tenant_id = $2 AND deleted_at IS NULL \
                   AND name ILIKE ALL($3) \
                   AND ($4::text IS NULL OR ({sort_col}, id) {keyset_cmp} ($4::{sort_cast}, $5)) \
                 ORDER BY {sort_col} {order_dir}, id {order_dir} LIMIT $6"
            );
            let rows: Vec<NodeRow> = sqlx::query_as(&sql)
                .bind(&ctx.org)
                .bind(&ctx.tenant_id)
                .bind(&terms)
                .bind(after_val.as_deref())
                .bind(after_id)
                .bind(batch)
                .fetch_all(&self.db)
                .await?;
            if (rows.len() as i64) < batch {
                exhausted = true;
            }
            if rows.is_empty() {
                break;
            }
            for row in rows {
                after_val = Some(child_sort_value(sort.key, &row));
                after_id = Some(row.id);
                let kind = NodeKind::parse(&row.kind).unwrap_or(NodeKind::File);
                let allowed = self
                    .authz
                    .check(
                        &ctx.subject(),
                        Relation::Viewer,
                        &node_fga_object(&ctx.ns(), kind, row.id),
                        Consistency::HigherConsistency,
                    )
                    .await?;
                if !allowed {
                    continue;
                }
                items.push(row_to_node(row)?);
                if items.len() == limit {
                    break;
                }
            }
        }
        let next_cursor = if items.len() == limit {
            match (after_val, after_id) {
                (Some(v), Some(i)) => Some(encode_child_cursor(sort, &v, i)),
                _ => None,
            }
        } else {
            None
        };
        // 検索も列挙操作として監査へ（クエリ本文は含めない: 低エントロピー語の露出回避）。
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: "node.search",
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
            "node.breadcrumb.read",
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
                    action: "node.breadcrumb.read",
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
