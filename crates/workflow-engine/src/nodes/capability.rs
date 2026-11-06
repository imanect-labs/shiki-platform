//! 能力呼び出しの本体（storage / rag / workflow.start）。node 経路と script の `Shiki.*` 経路で共用。
//!
//! effect_journal（storage.write は in-TX・workflow.start は cross-TX）・監査はここで一点合流する。
//! 個別ノードに認可検査を書かせない（INV-2）。scope ceiling は [`exec`](super::exec) が事前に担保する。

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::capability::{op_digest, EffectJournal, JournalDecision, EFFECT_IN_PROGRESS};
use crate::ir::params::{
    content_type_from_name, CsvPatchParams, CsvQueryParams, CsvWriteParams, NameTakenPolicy,
    RagSearchParams, StorageListParams, StorageReadParams, StorageWriteParams, WorkflowStartParams,
};
use crate::run::NodeContext;
use crate::vocab::STORAGE_READ_MAX_DOCUMENT_CHARS;

use super::csv_encoding::{csv_bytes as encode_csv, is_csv_type};
use super::exec::CapabilityNodeExecutor;
use super::ports::{
    name_taken_error, CsvPatchReq, CsvWriteReq, ExecCtx, NodePorts, PortError, StorageListReq,
    StorageWriteReq, NAME_TAKEN,
};
use super::resolver::{
    as_bytes, as_string, as_u32, as_uuid, opt_param, param, typed, ParamResolver,
};

/// params を typed struct として取り出す（保存済み IR は V1 済み・失敗は permanent 扱い）。
pub(super) fn parse_params<T: serde::de::DeserializeOwned>(raw: &Value) -> Result<T, PortError> {
    crate::ir::params::parse(raw).map_err(|e| PortError::invalid(format!("params が不正: {e}")))
}

/// バイト列の sha256（16 進）。storage.write の op_digest 素材。
fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// 結果にファイルの ID を `file_id` の名前でも載せる（D-96）。
///
/// ファイルの ID の項目名をそろえる（社内文書の検索・きっかけの内容と同じ `file_id`）。古い名前（`id`・
/// `node_id`）も残す（保存済みの定義の `$from` を壊さない）。
pub(super) fn with_file_id(mut out: Value, key: &str) -> Value {
    if let Value::Object(map) = &mut out {
        if !map.contains_key("file_id") {
            if let Some(id) = map.get(key).cloned() {
                map.insert("file_id".to_string(), id);
            }
        }
    }
    out
}

/// 名前の衝突（[`NAME_TAKEN`]）を、頼まれた名前で言い直す（BUG-97）。ほかの失敗はそのまま返す。
///
/// 番号を付けない扱いは、ブロックの欄（「同じ名前があるとき」）で直せることを言う。番号を付ける扱いで上限まで
/// 埋まっていたときは、ポートが最後に試した名前で組んだ文（[`name_taken_error`]）のまま返す（番号の規則は
/// StorageService の 1 か所にあり、エンジンは番号を付けた名前を作らない・D-112・D-124）。
fn explain_name_taken(e: PortError, name: &str, policy: NameTakenPolicy) -> PortError {
    if e.code == NAME_TAKEN && policy == NameTakenPolicy::Fail {
        name_taken_error(name, None)
    } else {
        e
    }
}

/// 副作用の無い誤りか（予約を外してよいか・BUG-188）。
///
/// 内部能力のポート（data・csv・workflow.start）の書込は 1 つの TX で、`unavailable` 以外の誤り（拒否・検証・名前の
/// 衝突・見つからない・競合）は書込の前か、TX を巻き戻した後に返る（何も書かれていない）。`unavailable` は書込が
/// 済んだか分からない（commit の応答が届かない・DB の障害）ので外さない。
fn leaves_no_effect(e: &PortError) -> bool {
    e.code != "unavailable"
}

/// 前の実行の予約が結果の無いまま残っているときの誤り（[`EFFECT_IN_PROGRESS`]・retryable）。
///
/// step は試す回数を使わずに、予約を回収できるまで待ってやり直す（`retry::wait_policy`・GAP-28）。この文が履歴に
/// 残るのは、待つ上限を過ぎても予約が残っていたときだけ。`what` は何をしていたかを言う語（「保存」「AI の呼び出し」など）。
pub(super) fn effect_in_progress(what: &str) -> PortError {
    PortError::new(
        EFFECT_IN_PROGRESS,
        format!(
            "前に始めた{what}の結果を確かめられないため、続けられませんでした。\
             少し待ってから、失敗したところから再開してください。"
        ),
        true,
    )
}

/// 同じ冪等キーで別の操作をしようとしたときの誤り（permanent・engine.md §7.3 の操作 digest・付録 D の N3）。
pub(super) fn effect_conflict(what: &str) -> PortError {
    PortError::new(
        "effect_conflict",
        format!("このブロックが前と違う内容で{what}をしようとしたため、止めました。"),
        false,
    )
}

/// 内部能力の書込を effect_journal で高々 1 回にする（予約 → 書込 → 結果の記録・cross-TX・PIT-31）。
///
/// node と script の `Shiki.*`、data・csv.patch・csv.write・workflow.start が同じ手順を通る（予約を外す決まりを
/// 1 か所に置く）。書込が副作用の無い誤り（[`leaves_no_effect`]）を返したら予約を外す。外さないと、原因を直して
/// 回収の猶予（[`crate::capability::journal::RESERVATION_RECLAIM_SECS`]）の内に再開した同じステップが
/// `effect_in_progress` で待たされる（BUG-188。いまは試す回数を使わずに待つ・GAP-28）。
///
/// 返すのは結果と、いま書いたか（`false` は記録済みの結果を返した。監査はいま書いたときだけ残す）。`what` は誤りの
/// 文で何をしていたかを言う語（「別のワークフローの実行」「CSV の保存」など）。
pub(crate) async fn journaled<F>(
    journal: &EffectJournal,
    tenant_id: &str,
    key: &str,
    digest: &str,
    what: &str,
    write: F,
) -> Result<(Value, bool), PortError>
where
    F: std::future::Future<Output = Result<Value, PortError>>,
{
    match journal
        .check(tenant_id, key, digest)
        .await
        .map_err(|e| PortError::unavailable(format!("journal: {e}")))?
    {
        JournalDecision::Proceed => {
            let out = match write.await {
                Ok(out) => out,
                Err(e) => {
                    // 外せなくても元の誤りを返す（本当の原因の代わりに「一時的な問題」を見せない）。予約は残るので、
                    // 回収の猶予の内に再開すると effect_in_progress で待ち、猶予の後に通る。
                    if leaves_no_effect(&e) {
                        if let Err(je) = journal.release(tenant_id, key, digest).await {
                            tracing::warn!(error = %je, code = %e.code,
                                "副作用の無い誤りの後に冪等の予約を外せません（回収の猶予の後に再開できる）");
                        }
                    }
                    return Err(e);
                }
            };
            journal
                .record(tenant_id, key, digest, &out)
                .await
                .map_err(|e| PortError::unavailable(format!("journal record: {e}")))?;
            Ok((out, true))
        }
        JournalDecision::AlreadyDone(v) => Ok((v, false)),
        JournalDecision::InProgress => Err(effect_in_progress(what)),
        JournalDecision::DigestMismatch => Err(effect_conflict(what)),
    }
}

/// csv.write が保存するファイル名（`.csv` で終わらなければ付ける・`TabularService::save_new` と同じ規則）。
///
/// 番号はこの名前の拡張子の前に付ける（`集計 (2).csv`）。
fn csv_file_name(name: &str) -> String {
    if name.to_lowercase().ends_with(".csv") {
        name.to_string()
    } else {
        format!("{name}.csv")
    }
}

/// 省略できるフォルダ指定。項目が無ければルート（`None`）、書いてあれば UUID に解決できること。
fn folder_param(
    e: Option<&crate::ir::expr::ValueExpr>,
    r: &ParamResolver<'_>,
    what: &str,
) -> Result<Option<uuid::Uuid>, PortError> {
    opt_param(e, r, what)?
        .map(|v| typed(&v, what, "UUID", as_uuid))
        .transpose()
}

/// 「フォルダ一覧」の続きの位置（`cursor`・D-131）。省略・null・空の文字は先頭から読む（`None`・ループの 1 回目に
/// 前の結果が無いときにそのまま渡せる）。文字でない値は `invalid`。
fn cursor_param(
    e: Option<&crate::ir::expr::ValueExpr>,
    r: &ParamResolver<'_>,
    what: &str,
) -> Result<Option<String>, PortError> {
    match opt_param(e, r, what)? {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let s = typed(&v, what, "文字列", as_string)?;
            Ok((!s.is_empty()).then_some(s))
        }
    }
}

impl CapabilityNodeExecutor {
    /// 監査（成功/失敗）を 1 行記録する（meta に secret 平文・本文を載せない）。
    pub(super) fn audit(&self, tenant_id: &str, api: &str, allowed: bool, meta: &Value) {
        self.audit.record(tenant_id, api, allowed, meta);
    }

    /// 外部 API のレート制御（未設定なら常に通過）。false は `rate_limited`（attempt 非消費・retryable）。
    pub(super) async fn rate_check(&self, ec: &ExecCtx, api: &str) -> Result<(), PortError> {
        let Some(rl) = &self.ratelimit else {
            return Ok(());
        };
        let key = format!("{}:{}", ec.tenant_id, api);
        match rl.try_acquire(&key, self.ratelimit_cfg, 1).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(PortError::new(
                "rate_limited",
                "レート上限に達しました",
                true,
            )),
            Err(e) => Err(PortError::unavailable(format!("ratelimit: {e}"))),
        }
    }

    // --- storage ---------------------------------------------------------

    pub(super) async fn node_storage_read(
        &self,
        params: &Value,
        _ctx: &NodeContext,
        ec: &ExecCtx,
        r: &ParamResolver<'_>,
    ) -> Result<Value, PortError> {
        let p: StorageReadParams = parse_params(params)?;
        let what = "storage.read: file";
        let file_id = typed(&param(&p.file, r, what)?, what, "UUID", as_uuid)?;
        // 文章のファイルは中身を、PDF・Office は社内文書の検索に取り込まれた本文を読む（D-111）。
        let mut out = self
            .ports
            .storage_read_document(ec, file_id, STORAGE_READ_MAX_DOCUMENT_CHARS)
            .await?;
        // 文章のファイルの中身は切らない（切ったかの項目は、どちらの読み方でも結果に載せる）。
        if let Value::Object(map) = &mut out {
            map.entry("truncated").or_insert(Value::Bool(false));
        }
        self.audit(
            &ec.tenant_id,
            "storage.read",
            true,
            &json!({ "file_id": file_id.to_string() }),
        );
        Ok(with_file_id(out, "id"))
    }

    pub(super) async fn node_storage_write(
        &self,
        params: &Value,
        ctx: &NodeContext,
        ec: &ExecCtx,
        r: &ParamResolver<'_>,
    ) -> Result<Value, PortError> {
        let p: StorageWriteParams = parse_params(params)?;
        // folder を書いたのに解決できない・UUID でないときはルートへ書かない（BUG-13）。
        let parent_id = folder_param(p.folder.as_ref(), r, "storage.write: folder")?;
        let what = "storage.write: name";
        let name = typed(&param(&p.name, r, what)?, what, "文字列", as_string)?;
        let content = param(&p.content, r, "storage.write: content")?;
        let what = "storage.write: content_type";
        // 種類を決めないときは、ファイル名の拡張子から決める（社内文書の検索に出るように・D-97）。
        let content_type = opt_param(p.content_type.as_ref(), r, what)?
            .map(|v| typed(&v, what, "文字列", as_string))
            .transpose()?
            .unwrap_or_else(|| {
                content_type_from_name(&name)
                    .unwrap_or("application/octet-stream")
                    .to_string()
            });
        // CSV として保存するときだけ、選んだ文字コードで書く（省略は BOM なしの UTF-8・D-120）。表と項目のまとまりの
        // リストは CSV に組み立てる（JSON の文字のまま .csv にしない・BUG-178）。
        let bytes = if is_csv_type(&content_type) {
            encode_csv(&content, p.encoding.unwrap_or_default(), "内容")?
        } else {
            as_bytes(&content)
        };

        // op_digest は書込の同一性（親/頼まれた名前/内容ハッシュ）で安定。冪等キーはステップ固定。番号は
        // StorageService が同じ TX の中で付けるので、頼まれた書込 1 つの digest のまま（D-112）。
        let digest = op_digest(
            "storage.write",
            &json!({
                "parent": parent_id.map(|p| p.to_string()),
                "name": name,
                "content_sha256": sha256_hex(&bytes),
            }),
        );
        let policy = p.on_name_taken.unwrap_or_default();
        let out = self
            .ports
            .storage_write(
                ec,
                StorageWriteReq {
                    parent_id,
                    name: name.clone(),
                    bytes,
                    content_type,
                    on_name_taken: policy,
                    idempotency_key: ctx.idempotency_key.clone(),
                    op_digest: digest,
                },
            )
            .await
            .map_err(|e| explain_name_taken(e, &name, policy))?;
        // 監査には実際に保存した名前（番号を付けたなら付けた名前）を残す。
        let saved = out.get("name").and_then(Value::as_str).unwrap_or(&name);
        self.audit(
            &ec.tenant_id,
            "storage.write",
            true,
            &json!({ "name": saved, "parent": parent_id.map(|p| p.to_string()) }),
        );
        Ok(with_file_id(out, "id"))
    }

    pub(super) async fn node_storage_list(
        &self,
        params: &Value,
        _ctx: &NodeContext,
        ec: &ExecCtx,
        r: &ParamResolver<'_>,
    ) -> Result<Value, PortError> {
        let p: StorageListParams = parse_params(params)?;
        let parent_id = folder_param(p.folder.as_ref(), r, "storage.list: folder")?;
        let cursor = cursor_param(p.cursor.as_ref(), r, "storage.list: cursor")?;
        let continued = cursor.is_some();
        let out = self
            .ports
            .storage_list(ec, StorageListReq { parent_id, cursor })
            .await?;
        self.audit(
            &ec.tenant_id,
            "storage.list",
            true,
            &json!({ "parent": parent_id.map(|p| p.to_string()), "continued": continued }),
        );
        Ok(out)
    }

    // --- rag -------------------------------------------------------------

    pub(super) async fn node_rag_search(
        &self,
        params: &Value,
        _ctx: &NodeContext,
        ec: &ExecCtx,
        r: &ParamResolver<'_>,
    ) -> Result<Value, PortError> {
        let p: RagSearchParams = parse_params(params)?;
        let what = "rag.search: query";
        let query = typed(&param(&p.query, r, what)?, what, "文字列", as_string)?;
        let what = "rag.search: top_k";
        let top_k = opt_param(p.top_k.as_ref(), r, what)?
            .map(|v| typed(&v, what, "0 以上の整数", as_u32))
            .transpose()?;
        let out = self.ports.rag_search(ec, &query, top_k).await?;
        // クエリ本文は監査に載せない（PII 混入回避）。件数のみ。
        let n = out
            .get("results")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        self.audit(&ec.tenant_id, "rag.search", true, &json!({ "results": n }));
        Ok(out)
    }

    // --- workflow.start（cross-TX effect_journal で start-once） ----------

    /// workflow.start（cross-TX effect_journal で start-once・node と script の `Shiki.workflow.start` の共用）。
    pub(crate) async fn workflow_start_once(
        ports: &dyn NodePorts,
        journal: &EffectJournal,
        ec: &ExecCtx,
        idempotency_key: &str,
        name: &str,
        input: &Value,
    ) -> Result<(Value, bool), PortError> {
        let digest = op_digest("workflow.start", &json!({ "name": name, "input": input }));
        journaled(
            journal,
            &ec.tenant_id,
            idempotency_key,
            &digest,
            "別のワークフローの実行",
            ports.workflow_start(ec, name, input),
        )
        .await
    }

    pub(super) async fn do_workflow_start(
        &self,
        ec: &ExecCtx,
        idempotency_key: &str,
        name: &str,
        input: &Value,
    ) -> Result<Value, PortError> {
        let (out, started) = Self::workflow_start_once(
            self.ports.as_ref(),
            &self.journal,
            ec,
            idempotency_key,
            name,
            input,
        )
        .await?;
        if started {
            self.audit(
                &ec.tenant_id,
                "workflow.start",
                true,
                &json!({ "name": name }),
            );
        }
        Ok(out)
    }

    pub(super) async fn node_workflow_start(
        &self,
        params: &Value,
        ctx: &NodeContext,
        ec: &ExecCtx,
        r: &ParamResolver<'_>,
    ) -> Result<Value, PortError> {
        let p: WorkflowStartParams = parse_params(params)?;
        let what = "workflow.start: name";
        let name = typed(&param(&p.name, r, what)?, what, "文字列", as_string)?;
        // input を書いたのに解決できないときは null で子を起動しない（BUG-13）。
        let input = opt_param(p.input.as_ref(), r, "workflow.start: input")?.unwrap_or(Value::Null);
        self.do_workflow_start(ec, &ctx.idempotency_key, &name, &input)
            .await
    }

    // --- csv（隔離 DuckDB 経由の tabular・Task 11P.9） -------------------

    /// csv.query（RO SQL・viewer・pure）。副作用が無いため journal 不要。
    pub(super) async fn node_csv_query(
        &self,
        params: &Value,
        _ctx: &NodeContext,
        ec: &ExecCtx,
        r: &ParamResolver<'_>,
    ) -> Result<Value, PortError> {
        let p: CsvQueryParams = parse_params(params)?;
        let what = "csv.query: file";
        let file_id = typed(&param(&p.file, r, what)?, what, "UUID", as_uuid)?;
        let what = "csv.query: sql";
        let sql = typed(&param(&p.sql, r, what)?, what, "文字列", as_string)?;
        let out = self.ports.csv_query(ec, file_id, &sql).await?;
        // SQL 本文は監査に載せない（PII/秘匿クエリ回避）。件数のみ。
        let rows = out
            .get("rows")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        self.audit(
            &ec.tenant_id,
            "csv.query",
            true,
            &json!({ "file_id": file_id.to_string(), "rows": rows }),
        );
        Ok(out)
    }

    /// csv.patch（editor・EngineDedup）。cross-TX effect_journal で at-least-once を高々 1 回に畳む
    /// （rev 楽観ロックのため、再試行時に version が進んでいると RevConflict になる—journal が
    /// 先に AlreadyDone を返すことで再適用も競合も避ける・PIT-31）。
    pub(super) async fn node_csv_patch(
        &self,
        params: &Value,
        ctx: &NodeContext,
        ec: &ExecCtx,
        r: &ParamResolver<'_>,
    ) -> Result<Value, PortError> {
        let p: CsvPatchParams = parse_params(params)?;
        let what = "csv.patch: file";
        let file_id = typed(&param(&p.file, r, what)?, what, "UUID", as_uuid)?;
        let what = "csv.patch: base_rev";
        let base_rev = typed(&param(&p.base_rev, r, what)?, what, "整数", Value::as_i64)?;
        let ops = param(&p.ops, r, "csv.patch: ops")?;
        if !ops.is_array() {
            return Err(PortError::invalid(
                "csv.patch: ops は配列である必要があります",
            ));
        }

        let digest = op_digest(
            "csv.patch",
            &json!({ "file": file_id.to_string(), "base_rev": base_rev, "ops": ops }),
        );
        let (out, applied) = journaled(
            &self.journal,
            &ec.tenant_id,
            &ctx.idempotency_key,
            &digest,
            "CSV の編集",
            self.ports.csv_patch(
                ec,
                CsvPatchReq {
                    file_id,
                    base_rev,
                    ops,
                },
            ),
        )
        .await?;
        if applied {
            self.audit(
                &ec.tenant_id,
                "csv.patch",
                true,
                &json!({ "file_id": file_id.to_string() }),
            );
        }
        Ok(with_file_id(out, "node_id"))
    }

    /// csv.write（作成権限・EngineDedup）。cross-TX effect_journal で重複保存を防ぐ。
    pub(super) async fn node_csv_write(
        &self,
        params: &Value,
        ctx: &NodeContext,
        ec: &ExecCtx,
        r: &ParamResolver<'_>,
    ) -> Result<Value, PortError> {
        let p: CsvWriteParams = parse_params(params)?;
        let parent_id = folder_param(p.folder.as_ref(), r, "csv.write: folder")?;
        let what = "csv.write: name";
        let name = typed(&param(&p.name, r, what)?, what, "文字列", as_string)?;
        // 選んだ文字コードで書く（省略は BOM なしの UTF-8・この欄が無かったころの定義の挙動・D-120）。表（「CSV を集計」の
        // 結果）と項目のまとまりのリストは CSV に組み立てる（JSON の文字のまま保存しない・BUG-178）。
        let csv_bytes = encode_csv(
            &param(&p.content, r, "csv.write: content")?,
            p.encoding.unwrap_or_default(),
            "CSV の内容",
        )?;
        let policy = p.on_name_taken.unwrap_or_default();

        let digest = op_digest(
            "csv.write",
            &json!({
                "parent": parent_id.map(|p| p.to_string()),
                "name": name,
                "content_sha256": sha256_hex(&csv_bytes),
            }),
        );
        // 番号は StorageService が書込の 1 つの TX の中で付ける（D-124）。journal の予約は頼まれた書込 1 つ（digest は
        // 頼まれた名前）で、試し直しは前に付けた名前の結果を返す。
        let file_name = csv_file_name(&name);
        let write = async {
            self.ports
                .csv_write(
                    ec,
                    CsvWriteReq {
                        parent_id,
                        name: file_name.clone(),
                        csv_bytes,
                        on_name_taken: policy,
                    },
                )
                .await
                .map_err(|e| explain_name_taken(e, &file_name, policy))
        };
        let (out, saved_now) = journaled(
            &self.journal,
            &ec.tenant_id,
            &ctx.idempotency_key,
            &digest,
            "CSV の保存",
            write,
        )
        .await?;
        if saved_now {
            let saved = out
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(&file_name);
            self.audit(
                &ec.tenant_id,
                "csv.write",
                true,
                &json!({ "name": saved, "parent": parent_id.map(|p| p.to_string()) }),
            );
        }
        Ok(with_file_id(out, "node_id"))
    }
}

#[cfg(test)]
#[path = "capability_tests.rs"]
mod tests;
