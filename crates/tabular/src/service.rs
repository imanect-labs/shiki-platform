//! CSV クエリ/パッチの単一チョークポイント（Task 11P.7・design §4.8.2）。
//!
//! UI・エージェントツール・ワークフローステップは**すべてここを通る**（AuthContext 必須）。
//! 認可は操作別に StorageService の内部 API が強制する（**実行主体交差則**・昇格しない）:
//! - query/rows/schema … `read_file_internal`（viewer@file）。複数 CSV クエリは対象の**全ファイル**で
//!   viewer を個別に要求する（1 つでも欠ければ全体を拒否・#1037）
//! - patch … `update_file_content_internal`（editor@file）
//! - save（新規） … `write_file_named`（member@org / editor@folder＝作成権限・同じ名前の扱いを選ぶ）
//!
//! DuckDB 実行は [`crate::runner`] の非特権別プロセスに隔離し、api には CSV を食わせない。
//!
//! CSV の文字コードは、読むときに `textenc` の判定（BOM → UTF-8 → 日本の文字コードの推定）で決めて UTF-8 にする
//! （ランナーと編集は UTF-8 だけを扱う）。日本語の Windows の Excel が保存する CSV の既定は Shift_JIS。編集は、
//! 元の文字コードと BOM のまま書き戻す（Excel や取り込む先の仕組みが読めなくならないように・GAP-45）。
//!
//! 集計（schema・rows・query）は Excel（.xlsx）も読む（D-17）。形式は先頭のバイトだけで決め（[`crate::input`]）、
//! 中身はそのままランナーへ渡す（展開と読み取りはランナーの中・PIT-39）。古い形式の Excel（.xls）とパスワードを
//! 付けたファイルは読まない。

use std::collections::HashMap;
use std::io::Write as _;
use std::sync::Arc;

use authz::AuthContext;
use futures::future::try_join_all;
use storage::{NodeKind, OnNameTaken, StorageService};
use uuid::Uuid;

use crate::error::TabularError;
use crate::input::{self, Sniffed};
use crate::patch::{apply_patches_with, PatchOp};
use crate::protocol::{
    CsvDialect, InputFormat, RunnerOp, RunnerRequest, RunnerResponse, RunnerTable,
};
use crate::runner::{run_isolated, RunnerConfig};
use crate::sql_guard::validate_read_only;
use crate::tables::{validate_tables, TableRef};

/// クォータ既定値（design §4.8.2「メモリ/時間/結果サイズのクォータ強制」）。
#[derive(Debug, Clone)]
pub struct Quotas {
    pub memory_limit_mb: u32,
    pub max_rows: u32,
    pub page_size: u32,
    /// 集計の結果の大きさの上限（バイト・[`crate::protocol::result_row_bytes`] で数える）。超える行の手前で打ち切り、
    /// `truncated` を立てる（GAP-92）。グリッドのページ（rows）には使わない（ページの大きさは `page_size`）。
    pub max_result_bytes: u64,
    /// 1 クエリで読み込むファイルの合計バイト数の上限（主ファイル＋追加ファイル）。api プロセスは
    /// 取得したファイルを一時ファイルへ書くまでメモリに持つため、複数 CSV の同時取得で使う量を抑える。
    pub max_query_input_bytes: u64,
}

impl Default for Quotas {
    fn default() -> Self {
        Quotas {
            memory_limit_mb: 512,
            max_rows: 10_000,
            page_size: 1_000,
            max_result_bytes: crate::protocol::DEFAULT_MAX_RESULT_BYTES,
            max_query_input_bytes: 1024 * 1024 * 1024,
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
        let input = self.fetch_input(ctx, file_id, None, trace_id).await?;
        self.run(RunnerOp::Schema, &input, Vec::new(), 0).await
    }

    /// CSV の 1 ページ（安定行番号順・offset から page_size）を返す（viewer 認可）。
    pub async fn rows(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        offset: u64,
        trace_id: Option<&str>,
    ) -> Result<RunnerResponse, TabularError> {
        let input = self.fetch_input(ctx, file_id, None, trace_id).await?;
        self.run(
            RunnerOp::Rows { offset },
            &input,
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
        Ok(self
            .query_tables_with_version(ctx, file_id, tables, sql, trace_id)
            .await?
            .0)
    }

    /// [`Self::query_tables`] と同じく SQL を実行し、主ファイルのバージョン（読んだ中身の `node.version`）も返す。
    ///
    /// 追加のファイルは CSV だけ（文字コードは主ファイルと同じく決めて UTF-8 にする・GAP-45）。Excel の主ファイルには
    /// 追加のファイルを足せない（シートの名前と表の名前がぶつかるため・D-17）。
    pub async fn query_tables_with_version(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        tables: &[TableRef],
        sql: &str,
        trace_id: Option<&str>,
    ) -> Result<(RunnerResponse, i64), TabularError> {
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
        //    前に拒否される（取得する元のバイト列の合計は予算内に収まる）。try_join_all は最初の失敗で
        //    残りを破棄する。
        let fetched = try_join_all(
            ids.iter()
                .zip(&sizes)
                .map(|(id, size)| self.fetch_input(ctx, *id, Some(*size), trace_id)),
        )
        .await?;
        // 3) 1) は元のバイト数で数えた。CSV は UTF-8 に直して一時ファイルに書く（GAP-45）ので、大きくなることがある
        //    （Shift_JIS の半角カナは 1 バイトが 3 バイト、全角は 2 バイトが 3 バイト）。ランナーに渡す一時ファイルの
        //    合計も予算に収める。
        let written = fetched
            .iter()
            .try_fold(0u64, |sum, input| {
                let len = input.file.as_file().metadata()?.len();
                Ok::<_, std::io::Error>(sum.saturating_add(len))
            })
            .map_err(|e| {
                TabularError::Internal(format!("一時ファイルの大きさを読めません: {e}"))
            })?;
        self.check_input_budget(written)?;
        let input_of: HashMap<Uuid, &RunnerInput> = ids.iter().copied().zip(&fetched).collect();
        let input = |id: &Uuid| {
            input_of
                .get(id)
                .copied()
                .ok_or_else(|| TabularError::Internal("一時ファイルが見つかりません".into()))
        };
        let primary = input(&file_id)?;
        if !tables.is_empty() && primary.format != InputFormat::Csv {
            return Err(TabularError::InvalidTables(
                "Excel のファイルには、ほかの CSV を表として足せません（CSV のファイルで使ってください）".into(),
            ));
        }
        let extra = tables
            .iter()
            .map(|t| {
                let table = input(&t.file_id)?;
                if table.format != InputFormat::Csv {
                    return Err(TabularError::InvalidTables(format!(
                        "テーブル `{}` のファイルは CSV ではありません（足せるのは CSV だけです）",
                        t.name
                    )));
                }
                Ok(RunnerTable {
                    name: t.name.clone(),
                    csv_path: table.file.path().to_string_lossy().into_owned(),
                })
            })
            .collect::<Result<Vec<_>, TabularError>>()?;
        // 一時ファイル（fetched）はランナー終了まで生かす（drop で削除される）。
        let response = self
            .run(
                RunnerOp::Query {
                    sql: sql.to_string(),
                },
                primary,
                extra,
                self.quotas.max_rows,
            )
            .await?;
        let version = primary.version;
        drop(fetched);
        Ok((response, version))
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
        self.query_tables_with_version(ctx, file_id, &[], sql, trace_id)
            .await
    }

    /// パッチ列を適用して新バージョンを保存する（editor 認可・rev 楽観ロック）。CSV でないファイル（Excel など）は
    /// 編集しない（[`TabularError::Unsupported`]）。
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
        // 編集は CSV だけ（Excel などは読めても書き戻せない・D-17）。
        if input::sniff(&bytes) != Sniffed::Text {
            return Err(TabularError::Unsupported(input::NOT_EDITABLE.into()));
        }
        let original = decode_csv(&bytes)?;
        // 区切りと改行は、集計と同じ推定（隔離ランナー）で決め、書き戻しも同じ形にする（BUG-144）。
        let dialect = self.dialect(&original.text).await;
        let result = apply_patches_with(original.text.as_bytes(), ops, &dialect)?;
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

    /// 新規 CSV を保存する（作成権限＝write_file_named が member@org/editor@folder を強制）。
    ///
    /// 同じ名前のファイルがあるときは `on_taken` に従う（番号は StorageService が付ける・D-112）。名前が Excel の
    /// 拡張子（.xlsx・.xlsm・.xls）で終わるときは保存しない（[`TabularError::Unsupported`]）。
    pub async fn save_new(
        &self,
        ctx: &AuthContext,
        parent_id: Option<Uuid>,
        name: &str,
        csv_bytes: &[u8],
        on_taken: OnNameTaken,
        trace_id: Option<&str>,
    ) -> Result<SavedCsv, TabularError> {
        // Excel の名前で CSV を作らない（「売上.xlsx.csv」を黙って作らない・D-17）。
        if let Some(message) = input::not_csv_name(name) {
            return Err(TabularError::Unsupported(message));
        }
        let file_name = if name.to_lowercase().ends_with(".csv") {
            name.to_string()
        } else {
            format!("{name}.csv")
        };
        let node = self
            .storage
            .write_file_named(
                ctx, parent_id, &file_name, csv_bytes, "text/csv", on_taken, trace_id,
            )
            .await?;
        Ok(SavedCsv {
            node_id: node.id,
            version: node.version,
            name: node.name,
        })
    }

    /// viewer 認可でファイルを取得し、隔離ランナー用の私有一時ファイルへ書く（読んだ中身のバージョンも返す）。
    ///
    /// CSV は文字コードを決めて UTF-8 にしてから書く（BOM は含めない・ランナーは UTF-8 だけを読む）。Excel（zip）は
    /// 大きさだけを確かめて、そのままのバイト列を書く（中身はランナーが読む）。
    ///
    /// `max_bytes` を渡すと、取得する版がそれより大きければダウンロード前に拒否する
    /// （事前に確認したサイズに取得を固定する・複数 CSV クエリの入力予算）。
    async fn fetch_input(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        max_bytes: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<RunnerInput, TabularError> {
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
        let (file, format) = match input::sniff(&bytes) {
            Sniffed::Zip => {
                if bytes.len() as u64 > input::MAX_FILE_BYTES {
                    return Err(TabularError::NotText(input::file_too_large()));
                }
                (runner_file(&bytes, ".xlsx")?, InputFormat::Xlsx)
            }
            Sniffed::OldOffice => return Err(TabularError::NotText(input::OLD_OFFICE.into())),
            Sniffed::Text => (
                runner_file(decode_csv(&bytes)?.text.as_bytes(), ".csv")?,
                InputFormat::Csv,
            ),
        };
        Ok(RunnerInput {
            file,
            format,
            version: node.version,
        })
    }

    /// CSV の区切りと改行を、集計の読み込みと同じ推定（隔離ランナーの [`RunnerOp::Dialect`]）で決める（BUG-144）。
    ///
    /// 中身が空のときと推定できないとき（ランナーが動かない・推定器が読めない CSV）は、以前の決め打ち（カンマ区切り・
    /// LF）で読む。推定できない CSV は集計でも読めないが、編集はこれまでどおりカンマ区切りとして通す。
    async fn dialect(&self, text: &str) -> CsvDialect {
        if text.trim().is_empty() {
            return CsvDialect::default();
        }
        let sniffed = match runner_file(text.as_bytes(), ".csv") {
            Ok(file) => {
                let input = RunnerInput {
                    file,
                    format: InputFormat::Csv,
                    version: 0,
                };
                self.run(RunnerOp::Dialect, &input, Vec::new(), 0).await
            }
            Err(e) => Err(e),
        };
        match sniffed {
            Ok(RunnerResponse {
                dialect: Some(dialect),
                ..
            }) => dialect,
            Ok(_) => CsvDialect::default(),
            Err(e) => {
                tracing::warn!(error = %e, "CSV の区切りを推定できませんでした（カンマ区切りとして編集します）");
                CsvDialect::default()
            }
        }
    }

    /// 隔離ランナーを起動して結果を受け取る（クォータ付き）。
    async fn run(
        &self,
        op: RunnerOp,
        input: &RunnerInput,
        extra_tables: Vec<RunnerTable>,
        max_rows: u32,
    ) -> Result<RunnerResponse, TabularError> {
        // ユーザー SQL（Query）の実行失敗は利用者起因（400）、それ以外の op はこちらの
        // 生成 SQL なので失敗＝内部異常（500）として区別する。
        let is_user_query = matches!(op, RunnerOp::Query { .. });
        let request = RunnerRequest {
            op,
            csv_path: input.file.path().to_string_lossy().into_owned(),
            format: input.format,
            memory_limit_mb: self.quotas.memory_limit_mb,
            max_rows,
            max_result_bytes: is_user_query.then_some(self.quotas.max_result_bytes),
            extra_tables,
        };
        let response = run_isolated(&self.runner, &request).await?;
        if !response.ok {
            let msg = response.error.unwrap_or_else(|| "unknown".into());
            // 渡したファイルを表として読めなかった（壊れた・上限を超える Excel など）。文言は利用者向け（D-17）。
            if response.input_error {
                return Err(TabularError::NotText(msg));
            }
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

/// 隔離ランナーへ渡す入力（私有の一時ファイル・形式・読んだ中身のバージョン）。
struct RunnerInput {
    file: tempfile::NamedTempFile,
    format: InputFormat,
    version: i64,
}

/// UTF-8 にした CSV か Excel のバイト列を、隔離ランナー用の私有一時ファイルへ書く。
fn runner_file(bytes: &[u8], suffix: &str) -> Result<tempfile::NamedTempFile, TabularError> {
    let mut tmp = tempfile::Builder::new()
        .prefix("shiki-tabular-")
        .suffix(suffix)
        .tempfile()
        .map_err(|e| TabularError::Internal(format!("一時ファイル作成に失敗: {e}")))?;
    tmp.write_all(bytes)
        .map_err(|e| TabularError::Internal(format!("一時ファイル書込に失敗: {e}")))?;
    tmp.flush()
        .map_err(|e| TabularError::Internal(format!("一時ファイル flush に失敗: {e}")))?;
    Ok(tmp)
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
    use crate::patch::apply_patches;
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
