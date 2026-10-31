//! 本番 `NodePorts` 実装（executor → 既存チョークポイント結線・Stage A W3）。
//!
//! [`workflow_engine::CapabilityNodeExecutor`] が能力ゲートウェイを通した後に呼ぶポートを、
//! StorageService / SearchService / LlmGateway / Sandbox / SecretStore / RunLauncher へ結線する。
//! agent.invoke のループは `agent_port.rs`、その道具の実装は `agent_tools.rs`（BUG-6）。判定（ai.decide）は
//! `decide_port.rs`（FEAT-1）。
//! `ExecCtx` から実行主体の `AuthContext`（interactive=user / schedule・event=workflow）を組み、
//! 各チョークポイント内の OpenFGA 認可を通す（scope ceiling は executor 側＝二重ゲート）。

use std::sync::Arc;

use async_trait::async_trait;
use llm_gateway::LlmGateway;
use sandbox_client::Sandbox;
use secrets::SecretStore;
use serde_json::{json, Value};
use sqlx::PgPool;
use storage::model::ChildSort;

use super::auth::auth_ctx;
use storage::StorageService;
use uuid::Uuid;
use workflow_engine::nodes::ports::{name_taken_error, NAME_TAKEN};
use workflow_engine::{
    CsvPatchReq, CsvWriteReq, DataWriteReq, ExecCtx, HttpSendReq, HttpSendResp, LlmDecideReq,
    LlmInvokeReq, NodePorts, PortError, ResolvedSecretView, ResolvedSkillView, StorageWriteReq,
    WorkflowRunLauncher,
};

/// 本番ポート（AppState のチョークポイント参照を保持）。
pub struct ProdNodePorts {
    pub storage: Arc<StorageService>,
    pub search: Option<Arc<rag::SearchService>>,
    pub gateway: Arc<LlmGateway>,
    pub sandbox: Option<Arc<dyn Sandbox>>,
    /// コード実行系（agent.invoke の code.run）の隔離ティア（admin ポリシー・design §4.6）。
    pub sandbox_backend: sandbox_client::SandboxBackend,
    pub secrets: Option<Arc<SecretStore>>,
    /// CSV 表データ（csv.query/patch/write・隔離 DuckDB 経由の Task 11P.9）。
    pub tabular: Option<Arc<tabular::TabularService>>,
    /// skill.invoke の実行時解決（レジストリ→artifact を実行主体 ReBAC で読む・#344）。
    pub skill_artifacts: Option<Arc<artifact::ArtifactStore>>,
    pub launcher: WorkflowRunLauncher,
    /// http.request の外部送信クライアント（リダイレクト非追従の別クライアントも内部で使う）。
    pub http: reqwest::Client,
    /// workflow.start の名前解決に使う（artifact 名 → workflow_id）。委譲台帳（data の委譲元）も引く。
    pub db: PgPool,
    /// 構造化データ（data.* ノード・`Shiki.data.*`・Task 15.7）。未結線なら data ノードは「未構成」で失敗する。
    pub data: Option<Arc<data::DataStore>>,
    /// data.transition の FSM 定義の解決（artifact チョークポイント経由）。
    pub fsms: Option<Arc<data::FsmStore>>,
    /// 公開 web の検索（agent.invoke の web.search・チャットと同じ `SearchProvider`）。未結線なら提示しない。
    pub web_search: Option<Arc<dyn websearch::SearchProvider>>,
}

impl ProdNodePorts {
    /// data のポート（data と FSM の両方が結線されていること）。
    fn data_port(&self) -> Result<super::data_port::DataPort<'_>, PortError> {
        match (&self.data, &self.fsms) {
            (Some(data), Some(fsms)) => Ok(super::data_port::DataPort {
                data,
                fsms,
                db: &self.db,
            }),
            _ => Err(PortError::new(
                "unsupported",
                "構造化データ（data）が構成されていません",
                false,
            )),
        }
    }
}

pub(super) fn map_storage(e: storage::StorageError) -> PortError {
    use storage::StorageError as S;
    match e {
        S::Forbidden => PortError::forbidden("storage 権限なし"),
        S::NotFound => PortError::new("not_found", "対象が見つかりません", false),
        // 保存先の同じフォルダに同じ名前のファイルがある（BUG-97）。新しく保存する書込（storage.write・csv.write）は
        // 扱いに合わせて [`name_taken`] で言う。
        S::Conflict => PortError::new(NAME_TAKEN, "同じ名前のファイルがすでにあります", false),
        S::IdempotencyConflict => PortError::new(
            "effect_conflict",
            "同じ冪等キーで別の書き込みがあります",
            false,
        ),
        S::Invalid(m) => PortError::invalid(m),
        other => PortError::unavailable(format!("storage: {other}")),
    }
}

/// ワークフローの「同じ名前があるとき」を、StorageService の扱いに写す（値の名前は同じ・D-112）。
fn on_name_taken(p: workflow_engine::ir::params::NameTakenPolicy) -> storage::OnNameTaken {
    use workflow_engine::ir::params::NameTakenPolicy as P;
    match p {
        P::Fail => storage::OnNameTaken::Fail,
        P::AddNumber => storage::OnNameTaken::AddNumber,
    }
}

/// 名前の衝突（[`storage::StorageError::Conflict`]）を、頼まれた名前と扱いで言う（BUG-97・D-112・D-124）。
///
/// 番号を付ける扱いで上限まで埋まっていたときは、最後に試した名前（「名前 (50).csv」）を言う。番号の規則は
/// StorageService の 1 か所（`storage::numbered_name`）にあるので、その規則で名前を求める（エンジンは写しを
/// 持たない）。番号を付けない扱いの文は、呼び出した側（ブロック・script）が言い直す。
fn name_taken(name: &str, policy: storage::OnNameTaken) -> PortError {
    match policy {
        storage::OnNameTaken::Fail => {
            PortError::new(NAME_TAKEN, "同じ名前のファイルがすでにあります", false)
        }
        storage::OnNameTaken::AddNumber => name_taken_error(
            name,
            Some(&storage::numbered_name(name, storage::MAX_NUMBERED_NAME)),
        ),
    }
}

/// tabular エラー → PortError（認可拒否は fail-closed の forbidden・競合/クォータは非 retryable）。
fn map_tabular(e: tabular::TabularError) -> PortError {
    use tabular::TabularError as T;
    match e {
        T::Forbidden => PortError::forbidden("csv 権限なし"),
        T::Authz(_) => PortError::forbidden("csv 認可失敗"),
        T::Storage(s) => map_storage(s),
        T::NotFound(m) => PortError::new("not_found", format!("csv: {m}"), false),
        T::SqlRejected(m) => PortError::invalid(format!("csv SQL 拒否: {m}")),
        T::InvalidPatch(m) => PortError::invalid(format!("csv パッチ不正: {m}")),
        // 文字コードを決められない・文章でない CSV（「ファイルを読む」と同じ分類・やり直さない・GAP-45）。
        T::NotText(m) => PortError::new(workflow_engine::nodes::ports::NOT_TEXT_FILE, m, false),
        T::RevConflict { base, current } => PortError::new(
            "conflict",
            format!("csv 競合: base={base}, current={current}"),
            false,
        ),
        T::QuotaExceeded(m) => {
            PortError::new("quota_exceeded", format!("csv クォータ超過: {m}"), false)
        }
        T::Runner(m) => PortError::unavailable(format!("csv runner: {m}")),
        // ユーザー SQL の誤り＝入力不正（非 retryable。再試行しても直らない）。
        T::QueryFailed(m) => PortError::invalid(format!("csv クエリ失敗: {m}")),
        T::Internal(m) => PortError::unavailable(format!("csv internal: {m}")),
    }
}

#[async_trait]
impl NodePorts for ProdNodePorts {
    async fn storage_write(&self, ec: &ExecCtx, req: StorageWriteReq) -> Result<Value, PortError> {
        let ctx = auth_ctx(ec);
        // 番号を付けるのはチョークポイントの 1 か所（D-112）。
        let policy = on_name_taken(req.on_name_taken);
        self.storage
            .write_file_internal_idempotent(
                &ctx,
                req.parent_id,
                &req.name,
                &req.bytes,
                &req.content_type,
                policy,
                &req.idempotency_key,
                &req.op_digest,
                ec.trace_id.as_deref(),
            )
            .await
            .map_err(|e| match e {
                storage::StorageError::Conflict => name_taken(&req.name, policy),
                other => map_storage(other),
            })
    }

    async fn storage_read(&self, ec: &ExecCtx, file_id: Uuid) -> Result<Value, PortError> {
        use super::text_file::{is_binary, not_text_kind};
        let ctx = auth_ctx(ec);
        let trace = ec.trace_id.as_deref();
        // 文章でない種類（PDF・Office・画像など）は中身を読む前に断る（大きな文書を読み込んでから捨てない）。
        let meta = self
            .storage
            .get_metadata(&ctx, file_id, trace)
            .await
            .map_err(map_storage)?;
        if is_binary(&meta.name, meta.content_type.as_deref()) {
            return Err(not_text_kind());
        }
        super::document_read::read_text_file(
            &self.storage,
            &ctx,
            file_id,
            trace,
            map_storage,
            not_text_kind,
        )
        .await
    }

    async fn storage_read_document(
        &self,
        ec: &ExecCtx,
        file_id: Uuid,
        max_chars: u32,
    ) -> Result<Value, PortError> {
        super::document_read::storage_read_document(self, ec, file_id, max_chars).await
    }

    async fn storage_list(
        &self,
        ec: &ExecCtx,
        parent_id: Option<Uuid>,
    ) -> Result<Value, PortError> {
        let ctx = auth_ctx(ec);
        let page = self
            .storage
            .list_children(
                &ctx,
                parent_id,
                ChildSort::default(),
                None,
                100,
                // ワークフローの ls もユーザー向けの列挙（システム領域は出さない・#392）。
                false,
                ec.trace_id.as_deref(),
            )
            .await
            .map_err(map_storage)?;
        let items: Vec<Value> = page
            .items
            .iter()
            .map(|n| {
                json!({
                    "id": n.id.to_string(),
                    "name": n.name,
                    "kind": format!("{:?}", n.kind).to_lowercase(),
                    "size": n.size_bytes,
                })
            })
            .collect();
        Ok(json!({ "items": items }))
    }

    async fn rag_search(
        &self,
        ec: &ExecCtx,
        query: &str,
        top_k: Option<u32>,
    ) -> Result<Value, PortError> {
        let ctx = auth_ctx(ec);
        let search = self
            .search
            .as_ref()
            .ok_or_else(|| PortError::forbidden("rag が未構成です"))?;
        let out = search
            .search(
                &ctx,
                query,
                top_k,
                rag::SearchMode::Hybrid,
                None,
                ec.trace_id.as_deref(),
            )
            .await
            .map_err(|e| PortError::unavailable(format!("rag: {e}")))?;
        let results: Vec<Value> = out.results.iter().map(rag_result).collect();
        Ok(json!({ "results": results }))
    }

    async fn llm_invoke(&self, ec: &ExecCtx, req: LlmInvokeReq) -> Result<Value, PortError> {
        super::llm_port::invoke(self, ec, req).await
    }

    async fn llm_decide(&self, ec: &ExecCtx, req: LlmDecideReq) -> Result<Value, PortError> {
        super::decide_port::decide(self, ec, req).await
    }

    async fn agent_invoke(
        &self,
        ec: &ExecCtx,
        req: workflow_engine::AgentInvokeReq,
        tools: Arc<dyn workflow_engine::AgentToolGateway>,
    ) -> Result<Value, PortError> {
        self.invoke_agent(ec, req, tools).await
    }

    async fn web_search(&self, ec: &ExecCtx, query: &str) -> Result<Value, PortError> {
        super::agent_tools::web_search(self, ec, query).await
    }

    async fn web_fetch(
        &self,
        ec: &ExecCtx,
        req: workflow_engine::WebFetchReq,
    ) -> Result<Value, PortError> {
        super::agent_tools::web_fetch(ec, req).await
    }

    async fn code_run(
        &self,
        ec: &ExecCtx,
        req: workflow_engine::CodeRunReq,
    ) -> Result<Value, PortError> {
        super::agent_tools::code_run(self, ec, req).await
    }

    async fn doc_read(
        &self,
        ec: &ExecCtx,
        req: workflow_engine::DocReadReq,
    ) -> Result<Value, PortError> {
        super::agent_tools::doc_read(self, ec, req).await
    }

    async fn storage_read_window(
        &self,
        ec: &ExecCtx,
        file_id: Uuid,
        offset: u64,
        max_chars: u32,
    ) -> Result<Value, PortError> {
        super::agent_tools::storage_read_window(self, ec, file_id, offset, max_chars).await
    }

    async fn http_send(&self, _ec: &ExecCtx, req: HttpSendReq) -> Result<HttpSendResp, PortError> {
        // Stage A は常に非追従（executor が redirect を拒否する）。SSRF/内部 rebind を防ぐため
        // auto-follow は使わない（`follow_redirects` が来ても Policy::none）。
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| PortError::unavailable(format!("http client: {e}")))?;
        let method = reqwest::Method::from_bytes(req.method.as_bytes())
            .map_err(|_| PortError::invalid("不正な HTTP メソッド"))?;
        let mut builder = client.request(method, &req.url);
        for (k, v) in &req.headers {
            builder = builder.header(k, v);
        }
        if let Some(body) = req.body {
            builder = builder.body(body);
        }
        if let Some(ms) = req.timeout_ms {
            builder = builder.timeout(std::time::Duration::from_millis(ms));
        }
        let mut resp = builder
            .send()
            .await
            .map_err(|e| PortError::unavailable(format!("http 送信: {e}")))?;
        let status = resp.status().as_u16();
        // 本文は上限まで**ストリーミング読み**する（巨大レスポンスの全量バッファで OOM しない）。
        const MAX_BODY: usize = 8 * 1024 * 1024;
        let mut body = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| PortError::unavailable(format!("http 本文: {e}")))?
        {
            if body.len() + chunk.len() > MAX_BODY {
                body.extend_from_slice(&chunk[..MAX_BODY - body.len()]);
                break;
            }
            body.extend_from_slice(&chunk);
        }
        Ok(HttpSendResp { status, body })
    }

    async fn resolve_secret(
        &self,
        ec: &ExecCtx,
        name: &str,
    ) -> Result<ResolvedSecretView, PortError> {
        let ctx = auth_ctx(ec);
        let secrets = self
            .secrets
            .as_ref()
            .ok_or_else(|| PortError::forbidden("secrets が未構成です"))?;
        // 宛先束縛の host 照合は executor が allowed_hosts で行うため、ここでは None（can_use 認可＋監査は実施）。
        let resolved = secrets
            .resolve(&ctx, name, None, ec.trace_id.as_deref())
            .await
            .map_err(|e| PortError::forbidden(format!("secret 解決: {e}")))?;
        Ok(ResolvedSecretView {
            plaintext: resolved.plaintext,
            allowed_hosts: resolved.binding.hosts().to_vec(),
        })
    }

    async fn skill_resolve(
        &self,
        ec: &ExecCtx,
        name: &str,
        version: &str,
    ) -> Result<ResolvedSkillView, PortError> {
        let Some(artifacts) = &self.skill_artifacts else {
            return Err(PortError::unavailable("skill 解決が未配線です"));
        };
        let ctx = auth_ctx(ec);
        super::skill_port::resolve_skill(
            &self.db,
            artifacts,
            &ctx,
            name,
            version,
            ec.trace_id.as_deref(),
        )
        .await
    }

    async fn workflow_start(
        &self,
        ec: &ExecCtx,
        name: &str,
        input: &Value,
    ) -> Result<Value, PortError> {
        let ctx = auth_ctx(ec);
        // 名前 → workflow_id（artifact 名の一意性・tenant×kind 内）。
        let id: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM artifact \
             WHERE tenant_id = $1 AND kind = 'workflow' AND name = $2 AND deleted_at IS NULL",
        )
        .bind(&ec.tenant_id)
        .bind(name)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| PortError::unavailable(format!("workflow 名前解決: {e}")))?;
        // 無い名前と見えない相手は同じ誤りにする（存在を明かさない・何度やっても通らないのでやり直さない・BUG-107）。
        let Some(workflow_id) = id else {
            return Err(workflow_not_visible());
        };
        // 実行主体の権限で子 run を起動（fire-and-forget）。子は親の run の因果の鎖を引き継ぐ（子の書込が
        // 親のデータ変更トリガを起こす循環を抑止する・D-26）。
        let run_id = self
            .launcher
            .start_child(&ctx, workflow_id, input, ec.run_id)
            .await
            .map_err(|e| match e {
                // 入力が子ワークフローの input_schema に合わない＝何度やっても通らない（再試行しない）。
                workflow_engine::run::LauncherError::Input(issues) => PortError::invalid(format!(
                    "workflow.start の入力: {}",
                    workflow_engine::run::LauncherError::input_message(&issues)
                )),
                // 子ワークフローの必須の文字の欄が空（直すまで通らない・D-43）。
                workflow_engine::run::LauncherError::NotReady(errors) => {
                    PortError::invalid(format!(
                        "workflow.start の起動先: {}",
                        workflow_engine::run::LauncherError::messages(&errors)
                    ))
                }
                // 見えない相手（権限の誤り）は、やり直せる失敗として繰り返さない（BUG-107）。
                workflow_engine::run::LauncherError::NotVisible => workflow_not_visible(),
                other => PortError::unavailable(format!("workflow.start: {other}")),
            })?;
        Ok(json!({ "run_id": run_id.map(|r| r.to_string()) }))
    }

    async fn csv_query(&self, ec: &ExecCtx, file_id: Uuid, sql: &str) -> Result<Value, PortError> {
        let ctx = auth_ctx(ec);
        let tabular = self
            .tabular
            .as_ref()
            .ok_or_else(|| PortError::forbidden("tabular が未構成です"))?;
        let (resp, version) = tabular
            .query_with_version(&ctx, file_id, sql, ec.trace_id.as_deref())
            .await
            .map_err(map_tabular)?;
        let rows: Vec<Value> = resp
            .rows
            .iter()
            .map(|row| Value::Array(row.iter().map(|c| cell_to_json(c.as_deref())).collect()))
            .collect();
        Ok(json!({
            "columns": resp.columns,
            "column_types": resp.column_types,
            "rows": rows,
            "total_rows": resp.total_rows,
            "truncated": resp.truncated,
            // 集計した中身のバージョン（後ろの「CSV を編集」の前提にできる・D-96）。
            "version": version,
        }))
    }

    async fn csv_patch(&self, ec: &ExecCtx, req: CsvPatchReq) -> Result<Value, PortError> {
        let ctx = auth_ctx(ec);
        let tabular = self
            .tabular
            .as_ref()
            .ok_or_else(|| PortError::forbidden("tabular が未構成です"))?;
        // ops（JSON 配列）を tabular の PatchOp へ型変換する（不正は permanent の invalid）。
        let ops: Vec<tabular::PatchOp> = serde_json::from_value(req.ops)
            .map_err(|e| PortError::invalid(format!("csv.patch: ops が不正: {e}")))?;
        let applied = tabular
            .patch(
                &ctx,
                req.file_id,
                req.base_rev,
                &ops,
                ec.trace_id.as_deref(),
            )
            .await
            .map_err(map_tabular)?;
        Ok(json!({
            "node_id": applied.node_id.to_string(),
            "version": applied.version,
            "rows": applied.rows,
            "cols": applied.cols,
        }))
    }

    async fn csv_write(&self, ec: &ExecCtx, req: CsvWriteReq) -> Result<Value, PortError> {
        let ctx = auth_ctx(ec);
        let tabular = self
            .tabular
            .as_ref()
            .ok_or_else(|| PortError::forbidden("tabular が未構成です"))?;
        // 番号は StorageService が書込の 1 つの TX の中で付ける（ファイルを保存・ドライブの新規作成と同じ規則・D-124）。
        let policy = on_name_taken(req.on_name_taken);
        let saved = tabular
            .save_new(
                &ctx,
                req.parent_id,
                &req.name,
                &req.csv_bytes,
                policy,
                ec.trace_id.as_deref(),
            )
            .await
            .map_err(|e| match e {
                tabular::TabularError::Storage(storage::StorageError::Conflict) => {
                    name_taken(&req.name, policy)
                }
                other => map_tabular(other),
            })?;
        Ok(json!({
            "node_id": saved.node_id.to_string(),
            "version": saved.version,
            "name": saved.name,
        }))
    }

    async fn data_list_tables(&self, ec: &ExecCtx) -> Result<Value, PortError> {
        self.data_port()?.list_tables(ec).await
    }

    async fn data_query(
        &self,
        ec: &ExecCtx,
        table_id: Uuid,
        query: &data::DataQuery,
    ) -> Result<Value, PortError> {
        self.data_port()?.query(ec, table_id, query).await
    }

    async fn data_get(
        &self,
        ec: &ExecCtx,
        table_id: Uuid,
        record_id: Uuid,
    ) -> Result<Value, PortError> {
        self.data_port()?.get(ec, table_id, record_id).await
    }

    async fn data_write(&self, ec: &ExecCtx, req: DataWriteReq) -> Result<Value, PortError> {
        self.data_port()?.write(ec, req).await
    }

    async fn data_approval_target(
        &self,
        ec: &ExecCtx,
        table_id: Uuid,
        record_id: Uuid,
    ) -> Result<workflow_engine::ApprovalTarget, PortError> {
        self.data_port()?
            .approval_target(ec, table_id, record_id)
            .await
    }
}

/// workflow.start の相手が無いか見えないときの誤り（permanent・無いと見えないを言い分けない・BUG-107）。
fn workflow_not_visible() -> PortError {
    PortError::new(
        "not_found",
        format!(
            "{}自動実行では、オンにするときに使えるようにしたワークフローだけを実行できます。",
            workflow_engine::nodes::ports::hidden_not_found("実行するワークフロー")
        ),
        false,
    )
}

/// 社内文書の検索の結果 1 件（ワークフローの rag.search の出力・カタログの output_fields と同じ形）。
///
/// relevance は reranker の生の値を設定のしきい値で段階に落とした関連の強さ（high・medium・low・チャットの
/// 引用と同じ）。score は順位から決めた値（1/(順位+1)）で関連の強さではない。生の値はモデルで尺度が変わる
/// ので出さない（しきい値を数で書かせない・D-96）。
fn rag_result(r: &rag::SearchResult) -> Value {
    json!({
        "file_id": r.file_id.to_string(),
        "file_name": r.file_name,
        "content": r.content,
        "relevance": r.relevance,
        "score": r.score,
    })
}

/// CSV セル（NULL は None）を JSON 値へ（NULL → null・値 → 文字列）。
fn cell_to_json(cell: Option<&str>) -> Value {
    cell.map_or(Value::Null, |s| Value::String(s.to_string()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// 検索の結果に関連の強さ（段階）を載せ、生の reranker の値は載せない（D-96）。
    #[test]
    fn rag_results_carry_the_relevance_band_but_not_the_raw_score() {
        let r = rag::SearchResult {
            chunk_id: Uuid::nil(),
            file_id: Uuid::nil(),
            file_name: "規程.md".into(),
            folder_id: None,
            page: None,
            heading_path: vec![],
            content: "締め日は 25 日".into(),
            parent_content: None,
            score: 0.5,
            rerank_score: 0.91,
            relevance: rag::Relevance::High,
            version: 3,
            anchor: None,
            quote: None,
            boxes: vec![],
        };
        let v = rag_result(&r);
        assert_eq!(v["relevance"], "high");
        assert_eq!(v["score"], 0.5);
        assert!(v.get("rerank_score").is_none(), "{v}");
        let fields: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        // カタログの output_fields（rag.search の results の要素）と同じ項目。
        let entry = workflow_engine::vocab::node_catalog()
            .into_iter()
            .find(|e| e.node_type == workflow_engine::vocab::NodeType::RagSearch)
            .unwrap();
        for f in fields {
            assert!(entry.output_fields[0].ty.contains(&format!("{f}:")), "{f}");
        }
    }

    /// 「同じ名前があるとき」の値の名前（`fail`・`add_number`）は、StorageService の扱いと同じ（D-112）。
    #[test]
    fn name_taken_policies_keep_their_value_names() {
        use workflow_engine::ir::params::NameTakenPolicy;
        for p in [NameTakenPolicy::Fail, NameTakenPolicy::AddNumber] {
            assert_eq!(
                serde_json::to_value(p).unwrap(),
                serde_json::to_value(on_name_taken(p)).unwrap()
            );
        }
    }

    /// 名前の衝突は name_taken（やり直さない）。番号を付ける扱いで上限まで埋まっていたら、StorageService の規則で
    /// 求めた最後の名前（「集計 (50).csv」）を言う（エンジンは番号を付けた名前を作らない・D-124）。
    #[test]
    fn name_taken_names_the_last_numbered_name_by_the_storage_rule() {
        let plain = name_taken("集計.csv", storage::OnNameTaken::Fail);
        assert_eq!(plain.code, NAME_TAKEN);
        assert!(!plain.retryable);
        let full = name_taken("集計.csv", storage::OnNameTaken::AddNumber);
        assert_eq!(full.code, NAME_TAKEN);
        assert!(!full.retryable);
        assert_eq!(
            full.message,
            "保存先に「集計.csv」と、番号を付けた「集計 (50).csv」までのファイルがすでにあります。\
             ファイル名に日付などを差し込んでください。"
        );
        assert!(full.message.contains(&storage::numbered_name(
            "集計.csv",
            storage::MAX_NUMBERED_NAME
        )));
    }
}
