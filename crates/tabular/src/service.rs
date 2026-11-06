//! CSV クエリ/パッチの単一チョークポイント（Task 11P.7・design §4.8.2）。
//!
//! UI・エージェントツール・ワークフローステップは**すべてここを通る**（AuthContext 必須）。
//! 認可は操作別に StorageService の内部 API が強制する（**実行主体交差則**・昇格しない）:
//! - query/rows/schema … `read_file_internal`（viewer@file）。複数 CSV クエリは対象の**全ファイル**で
//!   viewer を個別に要求する（1 つでも欠ければ全体を拒否・#1037）
//! - patch … `update_file_content_internal`（editor@file）
//! - save（新規） … `write_file_internal`（member@org / editor@folder＝作成権限）
//!
//! DuckDB 実行は [`crate::runner`] の非特権別プロセスに隔離し、api には CSV を食わせない。

use std::collections::HashMap;
use std::io::Write as _;
use std::sync::Arc;

use authz::AuthContext;
use futures::future::try_join_all;
use storage::{NodeKind, StorageService};
use uuid::Uuid;

use crate::error::TabularError;
use crate::patch::{apply_patches, PatchOp};
use crate::protocol::{RunnerOp, RunnerRequest, RunnerResponse, RunnerTable};
use crate::runner::{run_isolated, RunnerConfig};
use crate::sql_guard::validate_read_only;
use crate::tables::{validate_tables, TableRef};

/// クォータ既定値（design §4.8.2「メモリ/時間/結果サイズのクォータ強制」）。
#[derive(Debug, Clone)]
pub struct Quotas {
    pub memory_limit_mb: u32,
    pub max_rows: u32,
    pub page_size: u32,
    /// 1 クエリで読み込む CSV の合計バイト数の上限（主ファイル＋追加ファイル）。api プロセスは
    /// 取得した CSV を一時ファイルへ書くまでメモリに持つため、複数 CSV の同時取得で使う量を抑える。
    pub max_query_input_bytes: u64,
}

impl Default for Quotas {
    fn default() -> Self {
        Quotas {
            memory_limit_mb: 512,
            max_rows: 10_000,
            page_size: 1_000,
            max_query_input_bytes: 1024 * 1024 * 1024,
        }
    }
}

/// CSV クエリ/パッチサービス。
pub struct TabularService {
    storage: Arc<StorageService>,
    runner: RunnerConfig,
    quotas: Quotas,
    /// SQL の予約語（ランナーから 1 回だけ引いて使い回す・#1061）。失敗は記録しない（次で再試行）。
    reserved_words: tokio::sync::OnceCell<Arc<[String]>>,
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
    /// 適用後のファイル名（チャットの document_ref カード表示に使う・#381）。
    pub name: String,
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
            reserved_words: tokio::sync::OnceCell::new(),
        }
    }

    /// SQL の予約語の一覧（DuckDB の `duckdb_keywords()` の `reserved`・#1061）。
    ///
    /// 追加テーブル名の予約語判定はランナーが同じ語彙で行う。これは画面側の補完・事前検証用で、
    /// 語彙を手書きで複製しないために DuckDB 自身から引く。ファイルを読まないので認可は不要
    /// （公開情報）。初回だけランナーを起動し、以降は使い回す。
    pub async fn reserved_words(&self) -> Result<Arc<[String]>, TabularError> {
        self.reserved_words
            .get_or_try_init(|| async {
                let request = RunnerRequest {
                    op: RunnerOp::ReservedWords,
                    csv_path: String::new(),
                    memory_limit_mb: self.quotas.memory_limit_mb,
                    max_rows: 0,
                    extra_tables: Vec::new(),
                };
                let response = run_isolated(&self.runner, &request).await?;
                if !response.ok {
                    return Err(TabularError::Runner(
                        response.error.unwrap_or_else(|| "unknown".into()),
                    ));
                }
                let words: Vec<String> = response
                    .rows
                    .into_iter()
                    .filter_map(|row| row.into_iter().next().flatten())
                    .collect();
                if words.is_empty() {
                    // 空の一覧は「予約語なし」と区別できない。壊れた応答として失敗にする（記録しない）。
                    return Err(TabularError::Runner("予約語の一覧が空です".into()));
                }
                Ok(Arc::from(words))
            })
            .await
            .cloned()
    }

    /// CSV のスキーマ（列名・型）と総行数を返す（viewer 認可）。
    pub async fn schema(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<RunnerResponse, TabularError> {
        let path = self.fetch_csv(ctx, file_id, None, trace_id).await?;
        self.run(RunnerOp::Schema, path.path(), Vec::new(), 0).await
    }

    /// CSV の 1 ページ（安定行番号順・offset から page_size）を返す（viewer 認可）。
    pub async fn rows(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        offset: u64,
        trace_id: Option<&str>,
    ) -> Result<RunnerResponse, TabularError> {
        let path = self.fetch_csv(ctx, file_id, None, trace_id).await?;
        self.run(
            RunnerOp::Rows { offset },
            path.path(),
            Vec::new(),
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
        self.query_tables(ctx, file_id, &[], sql, trace_id).await
    }

    /// 複数 CSV に対する読み取り専用 SQL（#1037）。主ファイルは `data`、`tables` の各ファイルは
    /// 指定名のテーブルとして同じ隔離ランナーにロードされ、JOIN できる。
    ///
    /// **全ファイルを個別に viewer 認可**する（実行主体交差則・昇格しない）。1 つでも読めなければ
    /// 全体を失敗させ、部分的な結果は返さない（fail-closed）。
    pub async fn query_tables(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        tables: &[TableRef],
        sql: &str,
        trace_id: Option<&str>,
    ) -> Result<RunnerResponse, TabularError> {
        // まず SQL とテーブル指定を検証（敵対的入力を隔離プロセスへ渡す前に弾く多層防御）。
        // ストレージ読取より前に置き、不正な指定でファイル取得を発生させない。
        validate_read_only(sql)?;
        validate_tables(tables)?;
        // 取得対象（主ファイル＋追加ファイル）を重複なしで並べる。同じファイルを複数名で参照しても
        // 取得/一時ファイルは 1 回に畳む（認可結果は同一）。
        let mut ids = vec![file_id];
        for t in tables {
            if !ids.contains(&t.file_id) {
                ids.push(t.file_id);
            }
        }
        // 1) ダウンロードの前に、全ファイルの viewer をメタデータで確認し（読めないファイルが 1 つでも
        //    あれば何も取得しない）、入力の合計サイズを予算内に収める。件数は validate_tables で上限済み
        //    （主ファイル込み 1 + MAX_EXTRA_TABLES）＝並列度は有界。
        let metas = try_join_all(
            ids.iter()
                .map(|id| self.storage.get_metadata(ctx, *id, trace_id)),
        )
        .await?;
        let mut total: u64 = 0;
        let mut sizes = Vec::with_capacity(metas.len());
        for (id, node) in ids.iter().zip(&metas) {
            if node.kind != NodeKind::File {
                return Err(TabularError::NotFound(format!("file {id}")));
            }
            let size = node.size_bytes.unwrap_or(0).max(0);
            sizes.push(size);
            total = total.saturating_add(u64::try_from(size).unwrap_or(u64::MAX));
        }
        self.check_input_budget(total)?;
        // 2) 並行に取得する（各取得も viewer を要求する＝メタデータ確認後の剥奪も拾う）。各ファイルは
        //    確認したサイズを上限に取得する＝確認後に大きい版へ差し替わっても、全体をメモリへ載せる
        //    前に拒否される（合計は予算内に収まる）。try_join_all は最初の失敗で残りを破棄する。
        let fetched = try_join_all(
            ids.iter()
                .zip(&sizes)
                .map(|(id, size)| self.fetch_csv(ctx, *id, Some(*size), trace_id)),
        )
        .await?;
        let path_of: HashMap<Uuid, &std::path::Path> = ids
            .iter()
            .copied()
            .zip(fetched.iter().map(tempfile::NamedTempFile::path))
            .collect();
        let path = |id: &Uuid| {
            path_of
                .get(id)
                .copied()
                .ok_or_else(|| TabularError::Internal("一時ファイルが見つかりません".into()))
        };
        let primary = path(&file_id)?;
        let extra = tables
            .iter()
            .map(|t| {
                Ok(RunnerTable {
                    name: t.name.clone(),
                    csv_path: path(&t.file_id)?.to_string_lossy().into_owned(),
                })
            })
            .collect::<Result<Vec<_>, TabularError>>()?;
        // 一時ファイル（fetched）はランナー終了まで生かす（drop で削除される）。
        let resp = self
            .run(
                RunnerOp::Query {
                    sql: sql.to_string(),
                },
                primary,
                extra,
                self.quotas.max_rows,
            )
            .await;
        drop(fetched);
        resp
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
            name: updated.name.clone(),
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
    ///
    /// `max_bytes` を渡すと、取得する版がそれより大きければダウンロード前に拒否する
    /// （事前に確認したサイズに取得を固定する・複数 CSV クエリの入力予算）。
    async fn fetch_csv(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        max_bytes: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<tempfile::NamedTempFile, TabularError> {
        let (node, bytes) = match max_bytes {
            Some(max) => {
                self.storage
                    .read_file_internal_bounded(ctx, file_id, max, trace_id)
                    .await?
            }
            None => {
                self.storage
                    .read_file_internal(ctx, file_id, trace_id)
                    .await?
            }
        };
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
        extra_tables: Vec<RunnerTable>,
        max_rows: u32,
    ) -> Result<RunnerResponse, TabularError> {
        // ユーザー SQL（Query）の実行失敗は利用者起因（400）、それ以外の op はこちらの
        // 生成 SQL なので失敗＝内部異常（500）として区別する。
        let is_user_query = matches!(op, RunnerOp::Query { .. });
        let request = RunnerRequest {
            op,
            csv_path: csv_path.to_string_lossy().into_owned(),
            memory_limit_mb: self.quotas.memory_limit_mb,
            max_rows,
            extra_tables,
        };
        let response = run_isolated(&self.runner, &request).await?;
        if !response.ok {
            let msg = response.error.unwrap_or_else(|| "unknown".into());
            return Err(if is_user_query {
                TabularError::QueryFailed(msg)
            } else {
                TabularError::Runner(msg)
            });
        }
        Ok(response)
    }

    /// 1 クエリの入力合計（バイト）が予算内か。超過は [`TabularError::QuotaExceeded`]。
    fn check_input_budget(&self, total: u64) -> Result<(), TabularError> {
        if total > self.quotas.max_query_input_bytes {
            return Err(TabularError::QuotaExceeded(format!(
                "読み込む CSV の合計サイズが上限を超えています（{} MiB / 上限 {} MiB）",
                total / (1024 * 1024),
                self.quotas.max_query_input_bytes / (1024 * 1024)
            )));
        }
        Ok(())
    }

    pub fn page_size(&self) -> u32 {
        self.quotas.page_size
    }
}
