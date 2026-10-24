//! 保存ビュー（Task 9.4・型付き表示設定は Task 15.5）。
//!
//! 宣言的クエリ＋表示設定（[`ViewSpec`]）を `artifact(kind=data_view)` として保存する（6.1 共通枠・
//! ReBAC 共有・不変バージョン）。**実行は必ずクエリチョークポイント（[`crate::query`]）
//! 経由**で、行述語・フィールドマスク・集計抑制を**閲覧者本人の権限で毎回再評価**する
//! （作成者の権限を引き継がない・PIT-19/21）。表示設定は保存時にサーバが検証する（[`crate::view_spec`]）。

use std::sync::Arc;

use artifact::{ArtifactError, ArtifactKind, ArtifactStore, NewArtifact};
use authz::AuthContext;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::query::ir::{DataQuery, QueryResult, SortKey, MAX_SORT_KEYS};
use crate::store::DataStore;
use crate::view_spec::{ViewCheck, ViewSpec};
use crate::DataError;

/// 保存ビュー本文（artifact body）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DataViewBody {
    /// 対象テーブル。
    pub table_id: Uuid,
    /// 宣言的クエリ（実行時に閲覧者の述語/マスクと合成）。
    pub query: DataQuery,
    /// 表示設定（型付き・保存時にサーバが検証する）。省略時は既定の表。
    #[serde(default)]
    pub display: ViewSpec,
}

impl DataViewBody {
    /// 保存済みの本文を読む（上位互換）。
    ///
    /// Task 15.5 より前の `display` はサーバ非解釈の JSON だったので、`display` だけが読めない本文は
    /// 既定の表として読む（クエリ・対象テーブルはそのまま）。それ以外の破損は誤りにする。
    /// 書込（作成・更新）の入力は常に厳密に型検査する（[`ViewSpec`]）。
    pub fn from_stored(raw: serde_json::Value) -> Result<Self, serde_json::Error> {
        match serde_json::from_value::<DataViewBody>(raw.clone()) {
            Ok(b) => Ok(b),
            Err(e) => {
                let mut raw = raw;
                let removed = raw
                    .as_object_mut()
                    .and_then(|o| o.remove("display"))
                    .is_some();
                if removed {
                    serde_json::from_value(raw)
                } else {
                    Err(e)
                }
            }
        }
    }

    /// 実行に使うクエリ（表示設定のグループ化を並べ替えの先頭に入れ、投影が無ければ表示する列だけを取る）。
    ///
    /// 権限には関与しない（行述語・マスクは `run_query` が閲覧者本人の権限で掛ける）。`schema` は投影を
    /// 決めるために要るときだけ渡す（`None` なら投影はクエリのまま）。
    pub fn effective_query(&self, schema: Option<&crate::model::TableSchema>) -> DataQuery {
        let mut q = self.query.clone();
        if !q.aggregate.is_empty() {
            return q;
        }
        q.sort = effective_sort(&self.display, &q.sort);
        if q.select.is_none() {
            if let Some(schema) = schema {
                q.select = Some(self.display.projection(schema));
            }
        }
        q
    }

    /// 投影を決めるのにスキーマが要るか（表で全列を見せるなら投影は不要）。
    fn needs_projection(&self) -> bool {
        if self.query.select.is_some() || !self.query.aggregate.is_empty() {
            return false;
        }
        match &self.display {
            ViewSpec::Grid(g) => g.columns.iter().any(|c| c.hidden),
            _ => true,
        }
    }
}

/// 表示設定のグループ化の列を並べ替えの先頭キーにする（重複は除き、上限 4 キーで切る）。
pub fn effective_sort(display: &ViewSpec, sort: &[SortKey]) -> Vec<SortKey> {
    let ViewSpec::Grid(g) = display else {
        return sort.to_vec();
    };
    if g.group_by.is_empty() {
        return sort.to_vec();
    }
    let mut out: Vec<SortKey> = g
        .group_by
        .iter()
        .map(|gc| SortKey {
            field: gc.field.clone(),
            descending: gc.descending,
        })
        .collect();
    for k in sort {
        if !out.iter().any(|x| x.field == k.field) {
            out.push(k.clone());
        }
    }
    out.truncate(MAX_SORT_KEYS);
    out
}

/// 保存ビューの一覧の 1 件（テーブルごとのビュー切替用・本文は含めない）。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DataViewSummary {
    pub id: Uuid,
    pub name: String,
    pub owner: String,
    pub version: i64,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 保存ビューの 1 ページの指定（生成 UI の `data_grid` の束縛の params・Task 15.7）。
///
/// 画面（クライアント）が送れるのは**ページ位置と件数だけ**で、条件・並べ替え・投影は
/// ピンした版のビューの [`DataQuery`] がそのまま使われる（面ごとの独自フィルタ表現を作らない）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DataViewPage {
    /// 前ページの `next_cursor`（不透明トークン）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// 1 ページの件数（省略時はビューの `limit`・上限はクエリと同じ 200）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// 保存ビューを 1 ページ実行した結果（`data_grid` の描画材料・Task 15.7）。
///
/// 行・列は**閲覧者本人**の行述語・フィールドマスク・集計抑制で毎回評価したもの。列の型・表示名は
/// `table_id` のスキーマ（`GET /data/tables/{id}`・閲覧者本人の viewer）から引く。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DataViewRun {
    pub view_id: Uuid,
    /// ピンした版。
    pub version: i64,
    pub table_id: Uuid,
    /// 表示設定（型付き・Task 15.5）。
    pub display: ViewSpec,
    pub result: QueryResult,
}

/// 保存ビューの保存/実行（`crates/data` の一部＝クエリ実行と同じチョークポイント側）。
#[derive(Clone)]
pub struct DataViewStore {
    artifacts: Arc<ArtifactStore>,
    data: DataStore,
}

impl DataViewStore {
    pub fn new(artifacts: Arc<ArtifactStore>, data: DataStore) -> Self {
        DataViewStore { artifacts, data }
    }

    /// ビューを保存する（table 存在＋クエリ整合を検証 → artifact version 1）。
    pub async fn create(
        &self,
        ctx: &AuthContext,
        name: &str,
        body: &DataViewBody,
        trace_id: Option<&str>,
    ) -> Result<Uuid, DataError> {
        self.validate(ctx, body, trace_id).await?;
        let raw = serde_json::to_value(body)
            .map_err(|e| DataError::Invalid(format!("view body: {e}")))?;
        let artifact = self
            .artifacts
            .create(
                ctx,
                NewArtifact {
                    kind: ArtifactKind::DataView,
                    name: name.to_string(),
                    body: raw,
                },
                trace_id,
            )
            .await
            .map_err(map_artifact)?;
        Ok(artifact.id)
    }

    /// ビューに新バージョンを追記する（検証 → 不変追記）。
    pub async fn update(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        body: &DataViewBody,
        expected_version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<i64, DataError> {
        self.ensure_kind(ctx, id, trace_id).await?;
        self.validate(ctx, body, trace_id).await?;
        let raw = serde_json::to_value(body)
            .map_err(|e| DataError::Invalid(format!("view body: {e}")))?;
        let v = self
            .artifacts
            .append_version(ctx, id, raw, expected_version, trace_id)
            .await
            .map_err(map_artifact)?;
        Ok(v.version)
    }

    /// 指定バージョン（省略時は最新）のビューを取得する（viewer）。
    pub async fn get(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<(i64, DataViewBody), DataError> {
        let meta = self
            .artifacts
            .get(ctx, id, trace_id)
            .await
            .map_err(map_artifact)?;
        if meta.kind != ArtifactKind::DataView {
            return Err(DataError::NotFound);
        }
        let ver = version.unwrap_or(meta.current_version);
        let v = self
            .artifacts
            .get_version(ctx, id, ver, trace_id)
            .await
            .map_err(map_artifact)?;
        let body = DataViewBody::from_stored(v.body)
            .map_err(|e| DataError::Internal(format!("view body 破損: {e}")))?;
        Ok((v.version, body))
    }

    /// ビューを実行する（**閲覧者本人**の行述語・フィールドマスク・集計抑制で毎回再評価）。
    pub async fn run(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<QueryResult, DataError> {
        let (_, body) = self.get(ctx, id, version, trace_id).await?;
        let query = self.query_for_run(ctx, &body, trace_id).await?;
        // 実行はクエリチョークポイント経由（ビューの table viewer だけでなく、行述語も効く）。
        self.data
            .run_query(ctx, body.table_id, &query, trace_id)
            .await
    }

    /// ピンした版のビューを 1 ページ実行する（ビューの読取は**閲覧者本人の viewer**・Task 15.7）。
    ///
    /// 行・列は閲覧者本人の権限で毎回評価する（作成者・保存者の権限を引き継がない）。
    pub async fn run_pinned(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: i64,
        page: &DataViewPage,
        trace_id: Option<&str>,
    ) -> Result<DataViewRun, DataError> {
        let (version, body) = self.get(ctx, id, Some(version), trace_id).await?;
        self.run_page(ctx, id, version, body, page, trace_id).await
    }

    /// **バンドル（ミニアプリ）経由**でピンした版のビューを 1 ページ実行する（Task 15.7）。
    ///
    /// ビューの定義（クエリ）はバンドル本体の viewer で読む（部品の個別共有は不要・
    /// [`ArtifactStore::get_version_via_bundle`]）。**データは閲覧者本人の権限**（テーブル viewer・
    /// 行述語・フィールドマスク・集計抑制）で評価するので、バンドルの共有はデータの権限を広げない。
    ///
    /// ⚠️ `id@version` がバンドルにピンされていることの照合は呼び出し側（`gui::MiniAppStore`）の責務。
    pub async fn run_via_bundle(
        &self,
        ctx: &AuthContext,
        bundle_id: Uuid,
        id: Uuid,
        version: i64,
        page: &DataViewPage,
        trace_id: Option<&str>,
    ) -> Result<DataViewRun, DataError> {
        let v = self
            .artifacts
            .get_version_via_bundle(ctx, bundle_id, id, version, trace_id)
            .await
            .map_err(map_artifact)?;
        let body = DataViewBody::from_stored(v.body).map_err(|_| DataError::NotFound)?;
        self.run_page(ctx, id, version, body, page, trace_id).await
    }

    async fn run_page(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: i64,
        body: DataViewBody,
        page: &DataViewPage,
        trace_id: Option<&str>,
    ) -> Result<DataViewRun, DataError> {
        let mut query = self.query_for_run(ctx, &body, trace_id).await?;
        if let Some(cursor) = &page.cursor {
            query.cursor = Some(cursor.clone());
        }
        if let Some(limit) = page.limit {
            query.limit = Some(limit);
        }
        let result = self
            .data
            .run_query(ctx, body.table_id, &query, trace_id)
            .await?;
        Ok(DataViewRun {
            view_id: id,
            version,
            table_id: body.table_id,
            display: body.display,
            result,
        })
    }

    /// 実行に使うクエリ（グループ化の並べ替え・表示する列だけの投影）。スキーマは要るときだけ読む
    /// （閲覧者本人の viewer。行・列の権限は `run_query` が改めて掛ける）。
    async fn query_for_run(
        &self,
        ctx: &AuthContext,
        body: &DataViewBody,
        trace_id: Option<&str>,
    ) -> Result<DataQuery, DataError> {
        if !body.needs_projection() && body.query.select.is_none() {
            return Ok(body.effective_query(None));
        }
        let table = self.data.get_table(ctx, body.table_id, trace_id).await?;
        let mut q = body.effective_query(Some(&table.schema));
        if q.select.is_some() {
            let az = self.data.authz_memo();
            omit_masked(&mut q, &self.data.masked_fields(ctx, &az, &table).await?);
        }
        Ok(q)
    }

    /// テーブルの保存ビューのうち、閲覧者が使えるもの（本人が owner ＋ 共有されたもの・更新の新しい順）。
    ///
    /// テーブルの viewer であることを先に確かめる（見えないテーブルのビューの有無を漏らさない）。
    pub async fn list_for_table(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Vec<DataViewSummary>, DataError> {
        self.data
            .authorize_table(
                ctx,
                table_id,
                authz::Relation::Viewer,
                "data.view.list",
                trace_id,
            )
            .await?;
        let items = self
            .artifacts
            .list_visible_with_body_ref(
                ctx,
                ArtifactKind::DataView,
                "table_id",
                &table_id.to_string(),
                100,
            )
            .await
            .map_err(map_artifact)?;
        Ok(items
            .into_iter()
            .map(|a| DataViewSummary {
                id: a.id,
                name: a.name,
                owner: a.owner,
                version: a.current_version,
                updated_at: a.updated_at,
            })
            .collect())
    }

    /// 保存前の整合検証: table 存在（viewer）＋表示設定の検証＋クエリを dry-run（limit 1）で通す。
    async fn validate(
        &self,
        ctx: &AuthContext,
        body: &DataViewBody,
        trace_id: Option<&str>,
    ) -> Result<(), DataError> {
        // 表示設定は保存者本人のスキーマの見え方（マスク）で検証する。閲覧時は閲覧者本人の権限で
        // 改めて評価されるので、ここで通った設定が他の閲覧者に列や行を見せることはない。
        let table = self.data.get_table(ctx, body.table_id, trace_id).await?;
        let masked = self.data.masked_field_keys(ctx, &table).await?;
        let max_lookup_columns = self.data.view_lookup_cap(ctx).await?;
        let check = ViewCheck {
            schema: &table.schema,
            masked: &masked,
            max_lookup_columns,
        };
        check.check(&body.display)?;
        check.check_sort_keys(body.query.sort.iter().map(|k| k.field.as_str()))?;
        let mut query = body.effective_query(Some(&table.schema));
        if body.query.aggregate.is_empty() {
            if let Some(select) = &query.select {
                check.check_lookup_budget(select)?;
            }
        }
        // 実行時と同じく、保存者が読めない列は投影から外して検査する（グリッドの既定の投影は
        // 全列なので、外さないとマスク列を持つテーブルでは誰もビューを保存できない）。
        omit_masked(&mut query, &masked);
        // 作成者が table viewer であること＋クエリの静的整合（未宣言/マスク列参照など）を
        // limit=1 の実行で検査する（run_query が field/mask/index 検証を一手に担う）。
        // 未索引の列は保存を契機に自動昇格する（data-platform §10）。グループ化の列と並べ替えの代替列も
        // 1 列ずつ並べ替えに使って昇格させる。大きなテーブルで索引が準備中でもビューは保存する
        // （実行時に「準備中」になる）。上限超過の承認待ちは保存を止める。
        let mut probes = vec![query];
        for alt in body.display.sort_alternatives() {
            probes.push(DataQuery {
                sort: vec![SortKey {
                    field: alt.to_string(),
                    descending: false,
                }],
                ..DataQuery::default()
            });
        }
        for mut probe in probes {
            probe.limit = Some(1);
            probe.cursor = None;
            probe.with_count = false;
            match self
                .data
                .run_query_promoting(ctx, body.table_id, &probe, trace_id)
                .await
            {
                Ok(_) | Err(DataError::IndexPreparing { .. }) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    async fn ensure_kind(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(), DataError> {
        let meta = self
            .artifacts
            .get(ctx, id, trace_id)
            .await
            .map_err(map_artifact)?;
        if meta.kind != ArtifactKind::DataView {
            return Err(DataError::Invalid(
                "このアーティファクトは data_view ではありません".into(),
            ));
        }
        Ok(())
    }
}

#[allow(clippy::needless_pass_by_value)]
fn map_artifact(e: ArtifactError) -> DataError {
    match e {
        ArtifactError::NotFound => DataError::NotFound,
        ArtifactError::Forbidden => DataError::Forbidden,
        ArtifactError::Invalid(m) => DataError::Invalid(m),
        ArtifactError::Conflict(m) => DataError::Conflict(m),
        ArtifactError::Internal(m) => DataError::Internal(format!("artifact: {m}")),
    }
}

/// ビューの投影は作った人の列の並び。閲覧者が読めない（マスクされた）列は黙って外す
/// （直接の要求の投影は 403・`query::bound_query`）。
fn omit_masked(q: &mut DataQuery, masked: &std::collections::HashSet<String>) {
    if let Some(sel) = q.select.as_mut() {
        sel.retain(|k| !masked.contains(k));
    }
}
