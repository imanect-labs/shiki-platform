//! CSV クエリ/パッチの単一チョークポイント（Task 11P.7・design §4.8.2）。
//!
//! UI・エージェントツール・ワークフローステップは**すべてここを通る**（AuthContext 必須）。
//! 認可は操作別に StorageService の内部 API が強制する（**実行主体交差則**・昇格しない）:
//! - query/rows/schema … `read_file_internal`（viewer@file）
//! - patch … `update_file_content_internal`（editor@file）
//! - save（新規） … `write_file_internal`（member@org / editor@folder＝作成権限）
//!
//! DuckDB 実行は [`crate::runner`] の非特権別プロセスに隔離し、api には CSV を食わせない。

use std::io::Write as _;
use std::sync::Arc;

use authz::AuthContext;
use storage::{NodeKind, StorageService};
use uuid::Uuid;

use crate::error::TabularError;
use crate::patch::{apply_patches, PatchOp};
use crate::protocol::{RunnerOp, RunnerRequest, RunnerResponse};
use crate::runner::{run_isolated, RunnerConfig};
use crate::sql_guard::validate_read_only;

/// クォータ既定値（design §4.8.2「メモリ/時間/結果サイズのクォータ強制」）。
#[derive(Debug, Clone)]
pub struct Quotas {
    pub memory_limit_mb: u32,
    pub max_rows: u32,
    pub page_size: u32,
}

impl Default for Quotas {
    fn default() -> Self {
        Quotas {
            memory_limit_mb: 512,
            max_rows: 10_000,
            page_size: 1_000,
        }
    }
}

/// CSV クエリ/パッチサービス。
pub struct TabularService {
    storage: Arc<StorageService>,
    runner: RunnerConfig,
    quotas: Quotas,
}

/// 保存（新規 CSV）結果。
#[derive(Debug)]
pub struct SavedCsv {
    pub node_id: Uuid,
    pub version: i64,
    pub name: String,
}

/// パッチ適用結果。
#[derive(Debug)]
pub struct PatchApplied {
    pub node_id: Uuid,
    /// 適用後の新しい版（次回 base_rev に使う）。
    pub version: i64,
    pub rows: usize,
    pub cols: usize,
}

impl TabularService {
    pub fn new(storage: Arc<StorageService>, runner: RunnerConfig, quotas: Quotas) -> Self {
        TabularService {
            storage,
            runner,
            quotas,
        }
    }

    /// CSV のスキーマ（列名・型）と総行数を返す（viewer 認可）。
    pub async fn schema(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<RunnerResponse, TabularError> {
        let path = self.fetch_csv(ctx, file_id, trace_id).await?;
        self.run(RunnerOp::Schema, path.path(), 0).await
    }

    /// CSV の 1 ページ（安定行番号順・offset から page_size）を返す（viewer 認可）。
    pub async fn rows(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        offset: u64,
        trace_id: Option<&str>,
    ) -> Result<RunnerResponse, TabularError> {
        let path = self.fetch_csv(ctx, file_id, trace_id).await?;
        self.run(
            RunnerOp::Rows { offset },
            path.path(),
            self.quotas.page_size,
        )
        .await
    }

    /// 読み取り専用 SQL を実行して結果ページを返す（viewer 認可・SQL は検証必須）。
    pub async fn query(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        sql: &str,
        trace_id: Option<&str>,
    ) -> Result<RunnerResponse, TabularError> {
        // まず SQL を検証（api 側で拒否＝敵対的 SQL を隔離プロセスへ渡す前に弾く多層防御）。
        validate_read_only(sql)?;
        let path = self.fetch_csv(ctx, file_id, trace_id).await?;
        self.run(
            RunnerOp::Query {
                sql: sql.to_string(),
            },
            path.path(),
            self.quotas.max_rows,
        )
        .await
    }

    /// パッチ列を適用して新バージョンを保存する（editor 認可・rev 楽観ロック）。
    pub async fn patch(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        base_rev: i64,
        ops: &[PatchOp],
        trace_id: Option<&str>,
    ) -> Result<PatchApplied, TabularError> {
        // 現在の版を確認（editor でなくとも get_metadata は viewer で通るが、
        // 実際の書込は update_file_content_internal が editor を強制する）。
        let node = self.storage.get_metadata(ctx, file_id, trace_id).await?;
        if node.kind != NodeKind::File {
            return Err(TabularError::NotFound(format!("file {file_id}")));
        }
        // 楽観ロック: base_rev が現在の版と一致しなければ競合（黙って上書きしない）。
        if node.version != base_rev {
            return Err(TabularError::RevConflict {
                base: base_rev,
                current: node.version,
            });
        }
        let (_node, bytes) = self
            .storage
            .read_file_internal(ctx, file_id, trace_id)
            .await?;
        let result = apply_patches(&bytes, ops)?;
        let updated = self
            .storage
            .update_file_content_internal(ctx, file_id, &result.csv, "text/csv", trace_id)
            .await?;
        Ok(PatchApplied {
            node_id: updated.id,
            version: updated.version,
            rows: result.rows,
            cols: result.cols,
        })
    }

    /// 新規 CSV を保存する（作成権限＝write_file_internal が member@org/editor@folder を強制）。
    pub async fn save_new(
        &self,
        ctx: &AuthContext,
        parent_id: Option<Uuid>,
        name: &str,
        csv_bytes: &[u8],
        trace_id: Option<&str>,
    ) -> Result<SavedCsv, TabularError> {
        let file_name = if name.to_lowercase().ends_with(".csv") {
            name.to_string()
        } else {
            format!("{name}.csv")
        };
        let node = self
            .storage
            .write_file_internal(ctx, parent_id, &file_name, csv_bytes, "text/csv", trace_id)
            .await?;
        Ok(SavedCsv {
            node_id: node.id,
            version: node.version,
            name: node.name,
        })
    }

    /// viewer 認可で CSV を取得し、隔離ランナー用の私有一時ファイルへ書く。
    async fn fetch_csv(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<tempfile::NamedTempFile, TabularError> {
        let (node, bytes) = self
            .storage
            .read_file_internal(ctx, file_id, trace_id)
            .await?;
        if node.kind != NodeKind::File {
            return Err(TabularError::NotFound(format!("file {file_id}")));
        }
        let mut tmp = tempfile::Builder::new()
            .prefix("shiki-tabular-")
            .suffix(".csv")
            .tempfile()
            .map_err(|e| TabularError::Internal(format!("一時ファイル作成に失敗: {e}")))?;
        tmp.write_all(&bytes)
            .map_err(|e| TabularError::Internal(format!("一時ファイル書込に失敗: {e}")))?;
        tmp.flush()
            .map_err(|e| TabularError::Internal(format!("一時ファイル flush に失敗: {e}")))?;
        Ok(tmp)
    }

    /// 隔離ランナーを起動して結果を受け取る（クォータ付き）。
    async fn run(
        &self,
        op: RunnerOp,
        csv_path: &std::path::Path,
        max_rows: u32,
    ) -> Result<RunnerResponse, TabularError> {
        let request = RunnerRequest {
            op,
            csv_path: csv_path.to_string_lossy().into_owned(),
            memory_limit_mb: self.quotas.memory_limit_mb,
            max_rows,
        };
        let response = run_isolated(&self.runner, &request).await?;
        if !response.ok {
            return Err(TabularError::Runner(
                response.error.unwrap_or_else(|| "unknown".into()),
            ));
        }
        Ok(response)
    }

    pub fn page_size(&self) -> u32 {
        self.quotas.page_size
    }
}
