//! StorageService: 共有リンク（#342）— redeem 台帳（grant）の管理。
//!
//! - [`StorageService::revoke_share_link_grant`] — owner による per-user 個別取消（#375・**durable**）。
//! - [`StorageService::reconcile_user_grants_for_link`] — リンク失効/期限失効時の参照カウント剥奪。
//! - [`StorageService::list_share_link_grants`] — redeem 済み user の owner 向け可視化（#369 C-3）。
//!
//! **中核の不変条件（#375）**:
//! ```text
//! node_share_link_grant.revoked_at IS NULL  ⇔  その (link,user) の付与が live
//! via_link タプルが存在する                 ⇒  それを参照する live な台帳行が存在する
//! ```
//! per-user 取消もリンクごとの失効も、行を **DELETE せずソフト失効**させる。だから台帳を読む
//! クエリはすべて `revoked_at IS NULL` で絞る必要がある（漏らすと剥奪漏れ＝fail-open）。
//!
//! **deny のスコープは per-(link,user)**（per-(node,user) ではない）。owner がゴミ箱を押した意図は
//! 「このリンクはこの人向けではない」であって「この人をこの文書からブロックせよ」ではない。後者は
//! 否定 ACL であり、OpenFGA に否定が無い以上 redeem 経路でしか効かないため「明示共有では入れるのに
//! ブロックしたつもり」という危険な誤解を生む。#366 B-2（明示共有はリンク機構に触られない）にも反する。
//! 帰結として、あるリンクで取り消されたユーザーは**そのリンクでは二度と解錠できない**（un-revoke は
//! 設けない）。再度渡したければ owner は新しいリンクを発行する。

#[allow(clippy::wildcard_imports)]
use super::*;

use super::share_link_util::kind_of;
use crate::model::ShareLinkGrant;

/// grant 1 行（user_id・表示名・付与時刻）。owner 向け可視化に使う（#369 C-3）。
#[derive(sqlx::FromRow)]
struct GrantRow {
    user_id: String,
    display_name: Option<String>,
    granted_at: DateTime<Utc>,
}

/// 一覧の上限。無期限リンクで redeem 済みユーザーが増え続けても DB/メモリ/転送を抑える
/// （真の人数は `ShareLink.redeem_count` が持つ・上限超過分のページングは #369 follow-up）。
const SHARE_LINK_GRANTS_LIMIT: i64 = 500;

impl StorageService {
    /// 特定 user の redeem を個別に取り消す（owner 権限・#375）。当該 `(link,user)` の台帳行を
    /// **ソフト失効**させ、同 `(node,user,role)` を保持する他の live な grant が無ければ via_link
    /// タプルを剥奪する（参照カウント）。
    ///
    /// ソフト失効なので取消は **durable**: 対象ユーザーが同じ URL＋パスワードで再 redeem しても、
    /// redeem 側の条件付き upsert が revoked 行を復活させないため一律 `Forbidden` になる（#375）。
    /// 明示共有（viewer/editor）は別 relation なので決して剥奪されない（#366 B-2）。
    ///
    /// 存在しない／既に取消済みの grant は冪等成功。リンクが無い／別テナントは存在秘匿の `Forbidden`。
    pub async fn revoke_share_link_grant(
        &self,
        ctx: &AuthContext,
        link_id: Uuid,
        user_id: &str,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let Some((node_id, obj)) = self
            .authorize_link_owner(ctx, link_id, "node.share_link.grant.revoke", trace_id)
            .await?
        else {
            return Err(StorageError::Forbidden);
        };
        let ns = ctx.ns();
        let now = Utc::now();

        let mut tx = self.db.begin().await?;
        self.lock_node(&mut tx, node_id).await?;

        // live な行だけをソフト失効し、剥奪対象の role を同時に取り出す（1 文・冪等）。
        // 2 度目以降は 0 行 → `None` → そのまま冪等成功。
        let grole: Option<String> = sqlx::query_scalar(
            "UPDATE node_share_link_grant SET revoked_at = now() \
             WHERE link_id = $1 AND user_id = $2 AND tenant_id = $3 AND revoked_at IS NULL \
             RETURNING role",
        )
        .bind(link_id)
        .bind(user_id)
        .bind(&ctx.tenant_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(grole) = grole else {
            tx.rollback().await?;
            return Ok(());
        };
        let Some(role) = ShareRole::parse(&grole) else {
            return Err(StorageError::Integrity(format!(
                "共有リンク grant の role が不正: {grole}"
            )));
        };

        // 同 (node,user,role) を保持する **live な grant × active リンク** が残っていればタプルは
        // 消さない（参照カウント）。⚠️ 上の UPDATE を**同一 tx で先に**実行しているので、自分の行は
        // g.revoked_at IS NULL に引っ掛からず自然に除外される（だから link_id <> は要らない）。
        // 順序を入れ替えると自分を数えて remaining = 1 になり、タプルが永久に残る。
        let remaining: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM node_share_link_grant g \
             JOIN node_share_link l ON l.link_id = g.link_id \
             WHERE g.node_id = $1 AND g.user_id = $2 AND g.role = $3 AND g.tenant_id = $4 \
               AND g.revoked_at IS NULL \
               AND l.revoked_at IS NULL AND (l.expires_at IS NULL OR l.expires_at > $5)",
        )
        .bind(node_id)
        .bind(user_id)
        .bind(&grole)
        .bind(&ctx.tenant_id)
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;
        if remaining == 0 {
            // 最後の live grant → via_link を剥奪（失敗は ? 伝播で tx 巻き戻し＝fail-closed）。
            // 剥奪は台帳 UPDATE の後・commit の前・ロック保持下で行う。commit 失敗時に補償が
            // 要らないのは付与と非対称な点: 剥奪の失敗は「アクセスを失ったが台帳は live」＝
            // fail-closed 方向で、本人が再 redeem すればタプルが張り直されて自己修復する。
            self.authz
                .delete_tuple(&ns.user(user_id), role.relation_via_link(), &obj)
                .await?;
        }
        self.finalize_share_link_tx(
            tx,
            ctx,
            node_id,
            kind_of(&obj),
            "node.share_link.grant.revoke",
            json!({ "link_id": link_id, "user_id": user_id, "role": role,
                    "tuple_revoked": remaining == 0 }),
            trace_id,
        )
        .await
    }

    /// あるリンクの redeem 済み per-user タプルを参照カウントして剥奪する（失効/期限失効時）。
    ///
    /// (node,user,role) について**他に live な grant × active リンクが残っていれば FGA タプルは
    /// 消さず**、当該リンクの grant 行だけをソフト失効させる。最後の 1 本なら FGA タプルを剥奪する。
    /// FGA 剥奪に失敗したら `?` 伝播で tx を巻き戻す（fail-closed・失効を確定しない）。tx 内で呼ぶこと。
    ///
    /// 行を **DELETE せずソフト失効**させるのは、**期限切れリンクが active に戻り得る**ため
    /// （`extend_share_link` は `expires_at` を見ないので、未 sweep の期限切れリンクを未来へ延長すると
    /// 復活する）。行を消すと deny 台帳がこの経路で蒸発し #375 の durability が壊れる。
    /// 振る舞いは従来と互換: 一覧も `redeem_count` も `revoked_at IS NULL` で絞るので失効後は空 / 0。
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn reconcile_user_grants_for_link(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        ns: &Namespace<'_>,
        obj: &FgaObject,
        link_id: Uuid,
        node_id: Uuid,
        tenant_id: &str,
        now: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        // live な行だけが対象（既に取消済みの行を再処理すると無駄な FGA 呼び出しと冗長 UPDATE を生む）。
        let grants: Vec<(String, String)> = sqlx::query_as(
            "SELECT user_id, role FROM node_share_link_grant \
             WHERE link_id = $1 AND revoked_at IS NULL",
        )
        .bind(link_id)
        .fetch_all(&mut **tx)
        .await?;
        for (user_id, grole) in &grants {
            let Some(role) = ShareRole::parse(grole) else {
                continue; // 破損行は残す（黙って消さない）。
            };
            // 同一 (node,user,role) を保持する他 active リンク由来の **live な** grant 数。
            // ⚠️ g.revoked_at IS NULL を落とすと、他リンクで個別取消済みの行を数えて remaining = 1 に
            // なり、全リンク失効後も via_link タプルが残る（fail-open・#375 で最も危険な取り違え）。
            let remaining: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM node_share_link_grant g \
                 JOIN node_share_link l ON l.link_id = g.link_id \
                 WHERE g.node_id = $1 AND g.user_id = $2 AND g.role = $3 \
                   AND g.link_id <> $4 AND g.tenant_id = $5 AND g.revoked_at IS NULL \
                   AND l.revoked_at IS NULL AND (l.expires_at IS NULL OR l.expires_at > $6)",
            )
            .bind(node_id)
            .bind(user_id)
            .bind(grole)
            .bind(link_id)
            .bind(tenant_id)
            .bind(now)
            .fetch_one(&mut **tx)
            .await?;
            if remaining == 0 {
                // 最後の live grant → via_link タプルを剥奪（#366・失敗は ? 伝播で tx 巻き戻し＝
                // fail-closed）。明示共有の viewer/editor は別 relation なので決して触れない。
                self.authz
                    .delete_tuple(&ns.user(user_id), role.relation_via_link(), obj)
                    .await?;
            }
            // どちらの場合も当該リンクの grant 行はソフト失効させる（タプルは他 active リンクが保持）。
            sqlx::query(
                "UPDATE node_share_link_grant SET revoked_at = now() \
                 WHERE link_id = $1 AND user_id = $2 AND revoked_at IS NULL",
            )
            .bind(link_id)
            .bind(user_id)
            .execute(&mut **tx)
            .await?;
        }
        Ok(())
    }

    /// リンクを redeem した user 一覧を返す（owner 権限・#369 C-3）。表示名は **同一 org** の
    /// `directory_user` から解決する（無ければ `None`）。broad リンクは台帳を持たないため常に空。
    /// 個別取消済み（`revoked_at IS NOT NULL`）は live でないので含めない —— 含めると owner が
    /// 取消に失敗したと誤認する。
    pub async fn list_share_link_grants(
        &self,
        ctx: &AuthContext,
        link_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Vec<ShareLinkGrant>, StorageError> {
        if self
            .authorize_link_owner(ctx, link_id, "node.share_link.grants.list", trace_id)
            .await?
            .is_none()
        {
            return Err(StorageError::Forbidden);
        }
        // 表示名解決は同一 org の directory_user に限定する（テナント内 org 越境で別 org の表示名を
        // 返さない・org は隔離境界・#371）。LEFT JOIN なので該当なしは user_id 表示へフォールバック。
        let rows: Vec<GrantRow> = sqlx::query_as(
            "SELECT g.user_id, d.display_name, g.granted_at \
             FROM node_share_link_grant g \
             LEFT JOIN directory_user d \
               ON d.user_id = g.user_id AND d.tenant_id = g.tenant_id AND d.org = $3 \
             WHERE g.link_id = $1 AND g.tenant_id = $2 AND g.revoked_at IS NULL \
             ORDER BY g.granted_at DESC \
             LIMIT $4",
        )
        .bind(link_id)
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(SHARE_LINK_GRANTS_LIMIT)
        .fetch_all(&self.db)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| ShareLinkGrant {
                user_id: r.user_id,
                display_name: r.display_name,
                granted_at: r.granted_at,
            })
            .collect())
    }
}
