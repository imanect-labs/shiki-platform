//! CSV クエリ/パッチの単一チョークポイント（Task 11P.7・design §4.8.2）。
//!
//! UI・エージェントツール・ワークフローステップは**すべてここを通る**（AuthContext 必須）。
//! 認可は操作別に StorageService の内部 API が強制する（**実行主体交差則**・昇格しない）:
//! - query/rows/schema … `read_file_internal`（viewer@file）
//! - patch … `update_file_content_internal`（editor@file）
//! - save（新規） … `write_file_internal`（member@org / editor@folder＝作成権限）
//!
//! DuckDB 実行は [`crate::runner`] の非特権別プロセスに隔離し、api には CSV を食わせない。
//!
//! CSV の文字コードは、読むときに `textenc` の判定（BOM → UTF-8 → 日本の文字コードの推定）で決めて UTF-8 にする
//! （ランナーと編集は UTF-8 だけを扱う）。日本語の Windows の Excel が保存する CSV の既定は Shift_JIS。編集は、
//! 元の文字コードと BOM のまま書き戻す（Excel や取り込む先の仕組みが読めなくならないように・GAP-45）。

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
        Ok(self
            .query_with_version(ctx, file_id, sql, trace_id)
            .await?
            .0)
    }

    /// [`Self::query`] と同じく SQL を実行し、集計した CSV のバージョン（読んだ中身の `node.version`）も返す。
    ///
    /// バージョンは中身と同じ読み取りで決まる（別に引き直さない）。ワークフローの「CSV を集計」が結果に載せ、
    /// 後ろの「CSV を編集」の前提（base_rev）に使える（集計の後にほかの保存があれば、編集は競合で止まる・D-96）。
    pub async fn query_with_version(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        sql: &str,
        trace_id: Option<&str>,
    ) -> Result<(RunnerResponse, i64), TabularError> {
        // まず SQL を検証（api 側で拒否＝敵対的 SQL を隔離プロセスへ渡す前に弾く多層防御）。
        validate_read_only(sql)?;
        let (path, version) = self.fetch_csv_versioned(ctx, file_id, trace_id).await?;
        let response = self
            .run(
                RunnerOp::Query {
                    sql: sql.to_string(),
                },
                path.path(),
                self.quotas.max_rows,
            )
            .await?;
        Ok((response, version))
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
        let original = decode_csv(&bytes)?;
        let result = apply_patches(original.text.as_bytes(), ops)?;
        let csv = encode_back(&result.csv, &original)?;
        let updated = self
            .storage
            .update_file_content_internal(ctx, file_id, &csv, "text/csv", trace_id)
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

    /// viewer 認可で CSV を取得し、UTF-8 にして隔離ランナー用の私有一時ファイルへ書く。
    async fn fetch_csv(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<tempfile::NamedTempFile, TabularError> {
        Ok(self.fetch_csv_versioned(ctx, file_id, trace_id).await?.0)
    }

    /// [`Self::fetch_csv`] と同じく一時ファイルへ書き、読んだ中身のバージョンも返す。
    async fn fetch_csv_versioned(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(tempfile::NamedTempFile, i64), TabularError> {
        let (node, bytes) = self
            .storage
            .read_file_internal(ctx, file_id, trace_id)
            .await?;
        if node.kind != NodeKind::File {
            return Err(TabularError::NotFound(format!("file {file_id}")));
        }
        // ランナー（DuckDB）は UTF-8 だけを読むので、文字コードを決めて UTF-8 にしてから渡す（BOM は含めない）。
        let text = decode_csv(&bytes)?.text;
        let mut tmp = tempfile::Builder::new()
            .prefix("shiki-tabular-")
            .suffix(".csv")
            .tempfile()
            .map_err(|e| TabularError::Internal(format!("一時ファイル作成に失敗: {e}")))?;
        tmp.write_all(text.as_bytes())
            .map_err(|e| TabularError::Internal(format!("一時ファイル書込に失敗: {e}")))?;
        tmp.flush()
            .map_err(|e| TabularError::Internal(format!("一時ファイル flush に失敗: {e}")))?;
        Ok((tmp, node.version))
    }

    /// 隔離ランナーを起動して結果を受け取る（クォータ付き）。
    async fn run(
        &self,
        op: RunnerOp,
        csv_path: &std::path::Path,
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

    pub fn page_size(&self) -> u32 {
        self.quotas.page_size
    }
}

/// CSV の中身を読めないときの文言（利用者向け・ワークフローの誤りと CSV の画面にそのまま出す）。
const NOT_TEXT_CSV: &str =
    "この CSV の中身は文章として読めません。UTF-8 か Shift_JIS で保存した CSV を選んでください。";

/// CSV のバイト列の文字コードを決めて読む（読めなければ [`TabularError::NotText`]）。
fn decode_csv(bytes: &[u8]) -> Result<textenc::Decoded, TabularError> {
    textenc::decode_text(bytes).map_err(|_| TabularError::NotText(NOT_TEXT_CSV.into()))
}

/// 編集した CSV（UTF-8）を、元の文字コードと BOM で書き戻す。元の文字コードに無い文字は書かない
/// （`&#…;` に置き換えて中身を黙って変えない）。
fn encode_back(csv: &[u8], original: &textenc::Decoded) -> Result<Vec<u8>, TabularError> {
    let text = std::str::from_utf8(csv).map_err(|e| {
        TabularError::Internal(format!("編集した CSV が UTF-8 ではありません: {e}"))
    })?;
    textenc::encode_like(text, original.encoding, original.bom).map_err(|textenc::Unmappable(c)| {
        TabularError::InvalidPatch(format!(
            "「{c}」は、この CSV の文字コード（{}）では書けません。別の文字に置き換えてください。",
            original.encoding.name()
        ))
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use textenc::encoding_rs::{SHIFT_JIS, UTF_16LE, UTF_8};

    /// Excel が保存する文字コードの CSV を読み、編集は元の文字コードと BOM のまま書き戻す（GAP-45）。
    #[test]
    fn edits_are_written_back_in_the_original_encoding() {
        let csv = "経費,金額\n交通費,1200\n";
        for (encoding, bom) in [
            (SHIFT_JIS, false),
            (UTF_8, true),
            (UTF_8, false),
            (UTF_16LE, true),
        ] {
            let bytes = textenc::encode_like(csv, encoding, bom).unwrap();
            let original = decode_csv(&bytes).unwrap();
            assert_eq!(original.text, csv, "{}", encoding.name());
            let edited = apply_patches(
                original.text.as_bytes(),
                &[PatchOp::CellUpdate {
                    row: 0,
                    col: 1,
                    value: "1500".into(),
                }],
            )
            .unwrap();
            let back = encode_back(&edited.csv, &original).unwrap();
            assert_eq!(
                back,
                textenc::encode_like("経費,金額\n交通費,1500\n", encoding, bom).unwrap(),
                "{}",
                encoding.name()
            );
        }
    }

    #[test]
    fn refuses_csv_it_cannot_read_and_characters_it_cannot_write() {
        assert!(matches!(
            decode_csv(b"%PDF-1.7\n\x00\x01"),
            Err(TabularError::NotText(m)) if m == NOT_TEXT_CSV
        ));
        let (sjis, _, _) = SHIFT_JIS.encode("経費\n");
        let original = decode_csv(&sjis).unwrap();
        assert!(matches!(
            encode_back("経費😀\n".as_bytes(), &original),
            Err(TabularError::InvalidPatch(m)) if m.contains("「😀」") && m.contains("Shift_JIS")
        ));
    }
}
