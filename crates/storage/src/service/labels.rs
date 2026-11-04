//! StorageService: 見えるファイル・フォルダーの名前をまとめて引く（GAP-37・アプリとワークフローを使う人の権限の確かめ）。
//!
//! 共有の画面・公開の確認・アプリの画面が、部品の読み先・書き先のファイルとフォルダーを名前で言うために使う。名前を
//! 返すのは呼んだ人が閲覧できる（viewer）ものだけで、見えない・無い・消したものは返さない（存在を区別させない）。
//!
//! 二段で絞る: DB の候補（組織・テナント・生きているもの・`id` と種類で選んだもの）を先に引き（pre-filter）、OpenFGA の
//! BatchCheck（[`authz::visible::holds_any`]）で絞る（post-filter）。中身もメタデータの読み出しもしない名前だけの確かめ
//! なので監査には残さない（子の一覧と同じ扱い）。共有リンクの遅延失効（`get_metadata` の前段）は掛けないので、期限を
//! 過ぎたばかりのリンクで開けていた人には、失効のタイマーが撤去するまで名前が見える（中身は読めない）。

#[allow(clippy::wildcard_imports)]
use super::*;
use std::collections::HashMap;

/// 1 回に名前を引けるファイル・フォルダーの数の上限（呼び出し側は上限までに切る）。
pub const MAX_NODE_LABELS: usize = 200;

/// ファイル・フォルダーの名前（開くためのリンクを作れるよう、種類と内容の種類も添える）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeLabel {
    pub name: String,
    pub kind: NodeKind,
    pub content_type: Option<String>,
}

impl StorageService {
    /// `ids`（id と種類）のうち、呼んだ人が閲覧できるものの名前。見えない・無い・消した・種類の違うものは入れない。
    ///
    /// `ids` が [`MAX_NODE_LABELS`] を超えたら誤り（呼び出し側の上限の誤り・問う数を抑える）。
    pub async fn node_labels(
        &self,
        ctx: &AuthContext,
        ids: &[(Uuid, NodeKind)],
    ) -> Result<HashMap<Uuid, NodeLabel>, StorageError> {
        if ids.len() > MAX_NODE_LABELS {
            return Err(StorageError::Invalid(format!(
                "名前は 1 回に {MAX_NODE_LABELS} 件までしか引けません"
            )));
        }
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let want: Vec<Uuid> = ids.iter().map(|(id, _)| *id).collect();
        let rows: Vec<(Uuid, String, String, Option<String>)> = sqlx::query_as(
            "SELECT id, kind, name, content_type FROM node \
             WHERE id = ANY($1) AND org = $2 AND tenant_id = $3 AND deleted_at IS NULL",
        )
        .bind(&want)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .fetch_all(&self.db)
        .await?;
        // 求めた種類と違うもの（フォルダーの欄に入ったファイルの id など）は見えないものと同じに扱う。
        let rows: Vec<(Uuid, NodeKind, String, Option<String>)> = rows
            .into_iter()
            .filter_map(|(id, kind, name, content_type)| {
                let kind = NodeKind::parse(&kind)?;
                ids.contains(&(id, kind))
                    .then_some((id, kind, name, content_type))
            })
            .collect();
        if rows.is_empty() {
            return Ok(HashMap::new());
        }
        let ns = ctx.ns();
        let objects: Vec<FgaObject> = rows
            .iter()
            .map(|(id, kind, _, _)| match kind {
                NodeKind::Folder => ns.folder(&id.to_string()),
                NodeKind::File => ns.file(&id.to_string()),
            })
            .collect();
        let held = authz::visible::holds_any(
            self.authz.as_ref(),
            &ctx.subject(),
            &[Relation::Viewer],
            &objects,
            Consistency::HigherConsistency,
        )
        .await?;
        Ok(rows
            .into_iter()
            .zip(held)
            .filter(|(_, h)| *h)
            .map(|((id, kind, name, content_type), _)| {
                (
                    id,
                    NodeLabel {
                        name,
                        kind,
                        content_type,
                    },
                )
            })
            .collect())
    }
}
