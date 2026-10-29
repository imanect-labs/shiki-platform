//! 能力呼び出しの本体（storage / rag / workflow.start）。node 経路と script の `Shiki.*` 経路で共用。
//!
//! effect_journal（storage.write は in-TX・workflow.start は cross-TX）・監査はここで一点合流する。
//! 個別ノードに認可検査を書かせない（INV-2）。scope ceiling は [`exec`](super::exec) が事前に担保する。

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::capability::{op_digest, JournalDecision};
use crate::ir::params::{
    content_type_from_name, numbered_name, CsvPatchParams, CsvQueryParams, CsvWriteParams,
    NameTakenPolicy, RagSearchParams, StorageListParams, StorageReadParams, StorageWriteParams,
    WorkflowStartParams, MAX_NUMBERED_NAME,
};
use crate::run::NodeContext;
use crate::vocab::STORAGE_READ_MAX_DOCUMENT_CHARS;

use super::exec::CapabilityNodeExecutor;
use super::ports::{CsvPatchReq, CsvWriteReq, ExecCtx, PortError, StorageWriteReq, NAME_TAKEN};
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

/// 保存先に同じ名前のファイルがあって保存できなかったときの誤り（BUG-97・permanent）。
fn name_taken_error(name: &str, policy: NameTakenPolicy) -> PortError {
    let message = match policy {
        NameTakenPolicy::Fail => format!(
            "保存先に同じ名前のファイル「{name}」がすでにあります。\
             「同じ名前があるとき」を 番号を付けて保存 にするか、ファイル名に日付などを差し込んでください。"
        ),
        NameTakenPolicy::AddNumber => format!(
            "保存先に「{name}」と、番号を付けた「{}」までのファイルがすでにあります。\
             ファイル名に日付などを差し込んでください。",
            numbered_name(name, MAX_NUMBERED_NAME)
        ),
    };
    PortError::new(NAME_TAKEN, message, false)
}

/// 名前の衝突（[`NAME_TAKEN`]）を、頼まれた名前と扱いで言い直す。ほかの失敗はそのまま返す。
fn explain_name_taken(e: PortError, name: &str, policy: NameTakenPolicy) -> PortError {
    if e.code == NAME_TAKEN {
        name_taken_error(name, policy)
    } else {
        e
    }
}

/// csv.write で同じ名前のファイルがあれば、扱い（`on_name_taken`）に従って番号を付けた名前で試し直す（BUG-97）。
///
/// storage.write は扱いを [`StorageWriteReq`] で頼み、番号は StorageService が付ける（D-112）。csv.write のポートの
/// 要求（[`CsvWriteReq`]）は扱いを運ばないので、ここで名前を変えて呼び直す（規則は storage と同じ写し）。
/// `save` は試す名前を受けて 1 回保存する。名前の衝突で失敗した試しは何も書かない。名前の衝突のほかの失敗は、
/// そのまま返す。
async fn save_avoiding_taken_names<F, Fut>(
    name: &str,
    policy: NameTakenPolicy,
    mut save: F,
) -> Result<Value, PortError>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<Value, PortError>>,
{
    let tries = match policy {
        NameTakenPolicy::Fail => 1,
        NameTakenPolicy::AddNumber => MAX_NUMBERED_NAME,
    };
    for attempt in 1..=tries {
        match save(numbered_name(name, attempt)).await {
            Err(e) if e.code == NAME_TAKEN => {}
            other => return other,
        }
    }
    Err(name_taken_error(name, policy))
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
        let bytes = as_bytes(&param(&p.content, r, "storage.write: content")?);
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
        let out = self.ports.storage_list(ec, parent_id).await?;
        self.audit(
            &ec.tenant_id,
            "storage.list",
            true,
            &json!({ "parent": parent_id.map(|p| p.to_string()) }),
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

    pub(super) async fn do_workflow_start(
        &self,
        ec: &ExecCtx,
        idempotency_key: &str,
        name: &str,
        input: &Value,
    ) -> Result<Value, PortError> {
        let digest = op_digest("workflow.start", &json!({ "name": name, "input": input }));
        match self
            .journal
            .check(&ec.tenant_id, idempotency_key, &digest)
            .await
            .map_err(|e| PortError::unavailable(format!("journal: {e}")))?
        {
            JournalDecision::Proceed => {
                let out = self.ports.workflow_start(ec, name, input).await?;
                self.journal
                    .record(&ec.tenant_id, idempotency_key, &digest, &out)
                    .await
                    .map_err(|e| PortError::unavailable(format!("journal record: {e}")))?;
                self.audit(
                    &ec.tenant_id,
                    "workflow.start",
                    true,
                    &json!({ "name": name }),
                );
                Ok(out)
            }
            JournalDecision::AlreadyDone(v) => Ok(v),
            JournalDecision::InProgress => Err(PortError::new(
                "effect_in_progress",
                "別ワーカーが起動処理中",
                true,
            )),
            JournalDecision::DigestMismatch => Err(PortError::new(
                "effect_conflict",
                "同一冪等キーで別の起動要求",
                false,
            )),
        }
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
        let key = &ctx.idempotency_key;
        match self
            .journal
            .check(&ec.tenant_id, key, &digest)
            .await
            .map_err(|e| PortError::unavailable(format!("journal: {e}")))?
        {
            JournalDecision::Proceed => {
                let out = self
                    .ports
                    .csv_patch(
                        ec,
                        CsvPatchReq {
                            file_id,
                            base_rev,
                            ops,
                        },
                    )
                    .await?;
                self.journal
                    .record(&ec.tenant_id, key, &digest, &out)
                    .await
                    .map_err(|e| PortError::unavailable(format!("journal record: {e}")))?;
                self.audit(
                    &ec.tenant_id,
                    "csv.patch",
                    true,
                    &json!({ "file_id": file_id.to_string() }),
                );
                Ok(with_file_id(out, "node_id"))
            }
            JournalDecision::AlreadyDone(v) => Ok(with_file_id(v, "node_id")),
            JournalDecision::InProgress => Err(PortError::new(
                "effect_in_progress",
                "別ワーカーが編集処理中",
                true,
            )),
            JournalDecision::DigestMismatch => Err(PortError::new(
                "effect_conflict",
                "同一冪等キーで別の編集要求",
                false,
            )),
        }
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
        let csv_bytes = as_bytes(&param(&p.content, r, "csv.write: content")?);
        let policy = p.on_name_taken.unwrap_or_default();

        let digest = op_digest(
            "csv.write",
            &json!({
                "parent": parent_id.map(|p| p.to_string()),
                "name": name,
                "content_sha256": sha256_hex(&csv_bytes),
            }),
        );
        let key = &ctx.idempotency_key;
        match self
            .journal
            .check(&ec.tenant_id, key, &digest)
            .await
            .map_err(|e| PortError::unavailable(format!("journal: {e}")))?
        {
            JournalDecision::Proceed => {
                // 番号を付けた名前で試し直しても、journal の予約は頼まれた書込 1 つ（digest は頼まれた名前）。
                let file_name = csv_file_name(&name);
                let out = save_avoiding_taken_names(&file_name, policy, |candidate| {
                    self.ports.csv_write(
                        ec,
                        CsvWriteReq {
                            parent_id,
                            name: candidate,
                            csv_bytes: csv_bytes.clone(),
                        },
                    )
                })
                .await?;
                self.journal
                    .record(&ec.tenant_id, key, &digest, &out)
                    .await
                    .map_err(|e| PortError::unavailable(format!("journal record: {e}")))?;
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
                Ok(with_file_id(out, "node_id"))
            }
            JournalDecision::AlreadyDone(v) => Ok(with_file_id(v, "node_id")),
            JournalDecision::InProgress => Err(PortError::new(
                "effect_in_progress",
                "別ワーカーが保存処理中",
                true,
            )),
            JournalDecision::DigestMismatch => Err(PortError::new(
                "effect_conflict",
                "同一冪等キーで別の保存要求",
                false,
            )),
        }
    }
}
