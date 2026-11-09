//! ワークフロー検証カタログの単一構築点（Task 10.13）。
//!
//! カタログ（発話ユーザーの secret 名→許可ホスト・使えるモデル一覧）は
//! **保存 API と AI 編集ツール（emit_workflow）が同一実装を共有する**。ここが分裂すると
//! 「エディタでは通るが AI 経由では落ちる」乖離が生まれるため、構築ロジックはこの 1 箇所に置く。

use std::sync::Arc;

use workflow_engine::Catalog;

/// llm.invoke の model を照合する集合の材料。
///
/// gateway があれば**組織の許可リストから本人専用のモデルを除いた chat モデル**（BUG-11・BUG-12・FEAT-1）。
/// 実行時は gateway が同じ設定（[`llm_gateway::LlmGateway::settings_without_personal`]）でモデルを
/// 解決するので、保存時もそこへ揃える（管理者が無効にしたモデル・opt-in 前のモデルを保存で弾く）。
/// ChatGPT プランのモデルは連携した本人の枠で、ワークフローは誰が起動しても使わないため、本人であっても除く。
/// さらに**保存する人の利用者ごとのプロバイダの制限**を通す（D-169・[`llm_gateway::LlmGateway::settings_for_workflow_editor`]）。
/// 実行の主体（schedule・event ではワークフロー）は制限しないので、使えないプロバイダのモデルは保存で断り、モデルを
/// 省いたノードも実行時に動く既定のモデルが使えなければ断る（D-185）。既定・Auto の振り先・選択の禁止は実行時の設定の
/// まま読む。
/// gateway が無い構成（LLM 未構成）では従来どおりデプロイ設定のモデル id を使う（空なら照合しない）。
///
/// ai.decide のモデルも同じ材料から、判定用のモデル（`modality = "decision"`）だけを照合する（FEAT-1）。種類の
/// 振り分けはここ（[`WorkflowModels::check`]）だけで行い、保存 API と AI 編集（emit_workflow）が同じ結果になる。
#[derive(Clone)]
pub enum WorkflowModels {
    Gateway(llm_gateway::LlmGateway),
    /// デプロイ設定のモデル（id と種類）。
    Configured(Vec<(String, llm_gateway::Modality)>),
}

/// 保存時のモデル照合の材料（[`Catalog::models`] と [`Catalog::model_lock`] に入れる）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkflowModelCheck {
    /// llm.invoke に書けるモデル id（カタログ順）。`None` は照合しない（LLM 未構成）。
    /// gateway があれば、許可が 0 件でも `Some(空)`（明示したモデルを全部拒否する・BUG-37）。
    pub ids: Option<Vec<String>>,
    /// 組織がモデルの選択を禁じているときの既定（`auto` かモデル ID）。
    pub lock: Option<String>,
    /// 組織の既定が Auto か（[`Catalog::default_is_auto`]・ノードの思考の深さの警告・D-75）。LLM 未構成は false。
    pub default_is_auto: bool,
    /// ai.decide に書ける判定用のモデル id（カタログ順・[`Catalog::decision_models`]）。`None` は照合しない
    /// （LLM 未構成）。gateway があれば、組織が許可し、元にする chat モデルも許可されているものだけ
    /// （`/llm/models?purpose=workflow&modality=decision` と同じ集合）。
    pub decision_ids: Option<Vec<String>>,
    /// 組織では使えるが、保存する人のアカウントでは使えないモデル（[`Catalog::user_restricted_models`]・D-169）。
    pub restricted: Vec<String>,
    /// モデルを省いたノードが実行時に動くモデルが、保存する人のアカウントでは使えないとき、そのモデル
    /// （[`Catalog::default_model_unavailable`]・D-185）。
    pub default_unavailable: Option<String>,
}

/// 組織がモデルの選択を禁じているときの既定（[`Catalog::model_lock`]・規則はチャットとミニアプリの AI と同じ
/// [`llm_gateway::TenantLlmSettings::model_lock`] の 1 か所・D-30・D-117）。
fn model_lock(settings: &llm_gateway::TenantLlmSettings) -> Option<String> {
    settings.model_lock().map(str::to_string)
}

/// 組織の既定が Auto か（[`Catalog::default_is_auto`]）。
fn default_is_auto(settings: &llm_gateway::TenantLlmSettings) -> bool {
    settings.default_model == llm_gateway::AUTO_MODEL
}

impl WorkflowModels {
    /// gateway があればそれを、無ければデプロイ設定の id を材料にする。
    #[must_use]
    pub fn new(
        gateway: Option<llm_gateway::LlmGateway>,
        configured: Vec<(String, llm_gateway::Modality)>,
    ) -> Self {
        match gateway {
            Some(gw) => WorkflowModels::Gateway(gw),
            None => WorkflowModels::Configured(configured),
        }
    }

    /// 保存時の照合の材料（呼び出し主体の組織の設定・本人専用を除き、呼び出し主体＝保存する人の制限を通す）。
    pub async fn check(&self, ctx: &authz::AuthContext) -> Result<WorkflowModelCheck, String> {
        match self {
            WorkflowModels::Configured(models) => {
                let of = |kind: llm_gateway::Modality| -> Vec<String> {
                    models
                        .iter()
                        .filter(|(_, m)| *m == kind)
                        .map(|(id, _)| id.clone())
                        .collect()
                };
                // 何も設定していなければ照合しない（従来どおり）。
                let configured = !models.is_empty();
                Ok(WorkflowModelCheck {
                    ids: configured.then(|| of(llm_gateway::Modality::Chat)),
                    lock: None,
                    default_is_auto: false,
                    decision_ids: configured.then(|| of(llm_gateway::Modality::Decision)),
                    restricted: Vec::new(),
                    default_unavailable: None,
                })
            }
            WorkflowModels::Gateway(gw) => {
                let view = editor_view(gw, ctx).await?;
                let settings = view.runtime();
                let ids = |models: Vec<&llm_gateway::ModelEntry>| {
                    models.into_iter().map(|m| m.id.clone()).collect()
                };
                Ok(WorkflowModelCheck {
                    // llm.invoke に書けるのは chat モデルだけ（判定用のモデルは生成に使えない・FEAT-1）。保存する人の
                    // 制限で使えないものは除く（D-169）。
                    ids: Some(ids(view.chat_models(gw.catalog()))),
                    lock: model_lock(settings),
                    // 実行時の解決（`resolve_workflow_choice`）と同じ設定で、省略したモデルが Auto で動くか。
                    default_is_auto: default_is_auto(settings),
                    // ai.decide に書けるのは判定用のモデルだけ（元の chat モデルが無効なもの・保存する人の制限で
                    // 使えないものは除く・FEAT-1・D-169）。
                    decision_ids: Some(ids(view.decision_models(gw.catalog()))),
                    restricted: view.restricted().to_vec(),
                    default_unavailable: view.default_restricted().map(str::to_string),
                })
            }
        }
    }

    /// llm.invoke に書けるモデル id（カタログ順・照合しない構成では空）。
    pub async fn ids(&self, ctx: &authz::AuthContext) -> Result<Vec<String>, String> {
        Ok(self.check(ctx).await?.ids.unwrap_or_default())
    }

    /// ai.decide に書ける判定用のモデルの id と表示名（カタログ順・照合しない構成では空・D-79）。
    ///
    /// 集合は [`Self::check`] の `decision_ids` と同じ（組織が許可し、元にする chat モデルも許可されているもの）。
    /// 表示名は管理画面・モデル欄と同じ（[`llm_gateway::ModelEntry::label`]）。デプロイ設定だけの構成では表示名を持たない。
    pub async fn decision_models(
        &self,
        ctx: &authz::AuthContext,
    ) -> Result<Vec<chat::DecisionModel>, String> {
        match self {
            WorkflowModels::Configured(_) => Ok(self
                .check(ctx)
                .await?
                .decision_ids
                .unwrap_or_default()
                .into_iter()
                .map(|id| chat::DecisionModel { id, label: None })
                .collect()),
            WorkflowModels::Gateway(gw) => {
                let view = editor_view(gw, ctx).await?;
                Ok(view
                    .decision_models(gw.catalog())
                    .into_iter()
                    .map(|m| chat::DecisionModel {
                        id: m.id.clone(),
                        label: Some(m.label().to_string()),
                    })
                    .collect())
            }
        }
    }
}

/// 保存する人（`ctx`）から見たモデルの設定（[`llm_gateway::LlmGateway::settings_for_workflow_editor`]・D-169）。
async fn editor_view(
    gw: &llm_gateway::LlmGateway,
    ctx: &authz::AuthContext,
) -> Result<llm_gateway::WorkflowEditorSettings, String> {
    gw.settings_for_workflow_editor(ctx)
        .await
        .map_err(|e| format!("組織の AI 設定: {e}"))
}

/// カタログを組む（secrets 未配線ならモデルのみ・skill_installs 未配線なら skill 照合は空集合）。
pub async fn build_catalog_from(
    secrets: Option<&secrets::SecretStore>,
    skill_installs: Option<&app_platform::SkillInstallService>,
    models: &WorkflowModels,
    ctx: &authz::AuthContext,
) -> Result<Catalog, String> {
    let mut catalog = Catalog::default();
    // secret の参照名→許可ホスト（V4 の宛先束縛事前照合に使う）。
    if let Some(secrets) = secrets {
        for meta in secrets
            .list_mine(ctx)
            .await
            .map_err(|e| format!("secret 一覧: {e}"))?
        {
            catalog.secrets.insert(meta.name, meta.allowed_hosts);
        }
    }
    // 保存ユーザーのインストール済み skill（V4 skill 照合・10.1b）。
    if let Some(installs) = skill_installs {
        catalog.skills = installs
            .installed_versions(ctx)
            .await
            .map_err(|e| format!("skill インストール一覧: {e}"))?;
    }
    // llm.invoke の model 照合（組織が許可したモデルと、選択を禁じた組織の既定）と、ai.decide の判定用のモデル。
    let check = models.check(ctx).await?;
    catalog.models = check.ids;
    catalog.model_lock = check.lock;
    catalog.default_is_auto = check.default_is_auto;
    catalog.decision_models = check.decision_ids;
    // 保存する人のアカウントでは使えないモデルと、省いたモデルが動く既定が使えないこと（D-169・D-185）。
    catalog.user_restricted_models = check.restricted;
    catalog.default_model_unavailable = check.default_unavailable;
    Ok(catalog)
}

/// 承認（human.approval）・状態を進める（data.transition）・状態の変化のきっかけ・同じレコードを探す列と足す数
/// （キーで追加か更新・まとめて登録の照合・レコードの更新の足す数・GAP-16）・データを検索の列（BUG-244）・状態の列への
/// 書き込み（レコードの更新と追加・BUG-251）が束縛するテーブルを保存者の見え方で引いて、V4 の材料にする（GAP-08・BUG-105）。
/// データを検索のリンクの条件と結合が辿る先も、同じ見え方で引き足す（GAP-129）。
///
/// IR にそのどれも無ければ `None`（照合しない）。見えないテーブルは入れない（V4 が「見つからないか状態の進め方が
/// ない」と同じ文言で止める）。FSM の状態は状態の列の選択肢（FSM の保存時にこれと一致することを data が
/// 確かめている）。テーブルを参照（`$from`）で決める状態を進めるノードは引かない（実行時まで分からない）。
///
/// 定義は閲覧者向けの絞り込み（[`data::DataStore::get_table_for_viewer`]）を通す。テーブルの owner でない
/// 保存者には読める列だけが入るので、読めない列は無い列と同じ誤り（`ir.unknown_field`）になり、種別
/// （`ir.bad_assignee`）も返らない（無い列と読めない列を区別させない・data-platform §16.1）。状態の列が
/// 読めなければ状態を入れない（`ir.unknown_table` で止め、「使える状態」の一覧も出さない）。状態の列の名前も
/// 同じときだけ入れる（受信箱に出す列に状態の列を選ぶと止める・BUG-67）。レコードを追加するテーブルでは、作成時の状態も
/// 実行時の作成と同じく、テーブルにピンされた状態の進め方から引く（状態の列が読めなければ引かない）。
pub async fn fsm_tables(
    data: &data::DataStore,
    ctx: &authz::AuthContext,
    ir: &serde_json::Value,
) -> Result<Option<std::collections::BTreeMap<String, workflow_engine::CatalogTable>>, String> {
    use workflow_engine::vocab::{EventSource, NodeType};
    let nodes = || {
        ir.get("nodes")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
    };
    let of_type = |n: &serde_json::Value, t: NodeType| {
        n.get("type").and_then(serde_json::Value::as_str) == Some(t.as_str())
    };
    // 状態の変化のきっかけ（data.transition のイベントトリガ）の対象のテーブル。
    let trigger_tables: Vec<&str> = ir
        .get("triggers")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|t| {
            t.get("kind").and_then(serde_json::Value::as_str) == Some("event")
                && t.get("source").and_then(serde_json::Value::as_str)
                    == Some(EventSource::DataTransition.as_str())
        })
        .filter_map(|t| t.pointer("/scope/table")?.as_str())
        .collect();
    // 同じレコードを探す列（重複を許さない列）と足す数（数の列）を照らすブロック（GAP-16）。
    let keyed = |n: &serde_json::Value| {
        let has = |at: &str| {
            n.pointer(at).is_some_and(|v| match v {
                serde_json::Value::String(s) => !s.trim().is_empty(),
                serde_json::Value::Object(m) => !m.is_empty(),
                _ => false,
            })
        };
        of_type(n, NodeType::DataRecordUpsert)
            || (of_type(n, NodeType::DataBulkUpsert) && has("/params/match_on"))
            // 更新の変更する値・足す数に書いた状態の列（BUG-251）。
            || (of_type(n, NodeType::DataRecordUpdate)
                && (has("/params/add") || has("/params/patch")))
            // 追加の値に書いた状態（最初の状態だけ・BUG-251）。
            || (of_type(n, NodeType::DataRecordCreate) && has("/params/data"))
            // データを検索の絞り込み・並べ替え・まとめ・集計の列（BUG-244）。
            || (of_type(n, NodeType::DataQuery) && has("/params/query"))
    };
    // レコードを追加するブロックのテーブル（作成時の状態を引く・BUG-251）。
    let adding: std::collections::BTreeSet<uuid::Uuid> = nodes()
        .filter(|n| {
            of_type(n, NodeType::DataRecordCreate) || of_type(n, NodeType::DataRecordUpsert)
        })
        .filter_map(|n| n.pointer("/params/table")?.as_str())
        .filter_map(|t| uuid::Uuid::parse_str(t).ok())
        .collect();
    let uses = nodes().any(|n| {
        of_type(n, NodeType::HumanApproval) || of_type(n, NodeType::DataTransition) || keyed(n)
    }) || !trigger_tables.is_empty();
    if !uses {
        return Ok(None);
    }
    let ids: std::collections::BTreeSet<uuid::Uuid> = nodes()
        .filter_map(|n| {
            if of_type(n, NodeType::HumanApproval) {
                n.pointer("/params/record/table")?.as_str()
            } else if of_type(n, NodeType::DataTransition) || keyed(n) {
                n.pointer("/params/table")?.as_str()
            } else {
                None
            }
        })
        .chain(trigger_tables)
        .filter_map(|t| uuid::Uuid::parse_str(t).ok())
        .collect();
    let mut out = std::collections::BTreeMap::new();
    for id in ids {
        let Some(table) = viewer_table(data, ctx, id).await? else {
            continue;
        };
        // 作成時の状態（レコードを追加するテーブルで、状態の列を読めるときだけ・実行時の作成と同じ読み方・BUG-251）。
        let fsm_initial = if status_def(&table.schema).is_some() && adding.contains(&id) {
            match data.fsm_initial(ctx, &table, None).await {
                Ok(fi) => fi,
                Err(data::DataError::NotFound | data::DataError::Forbidden) => None,
                Err(e) => return Err(format!("状態の進め方を使うテーブル: {e}")),
            }
        } else {
            None
        };
        out.insert(
            id.hyphenated().to_string(),
            catalog_table(table, fsm_initial),
        );
    }
    // データを検索のリンクの条件と結合が辿る先（その先の列を照らす・GAP-129）。照合と同じ歩き方で、材料に無いものだけを
    // テーブルごとに 1 回引く。引き足すと、その先が分かる（入れ子のリンク・結合の連なりの深さの上限まで）。
    if let Ok(parsed) = workflow_engine::WorkflowIr::from_json(ir) {
        let mut tried = std::collections::BTreeSet::new();
        for _ in 0..data::MAX_JOIN_DEPTH {
            let need: Vec<uuid::Uuid> = workflow_engine::query_link_tables(&parsed, &out)
                .into_iter()
                .filter(|id| tried.insert(*id))
                .collect();
            if need.is_empty() {
                break;
            }
            for id in need {
                if let Some(table) = viewer_table(data, ctx, id).await? {
                    out.insert(id.hyphenated().to_string(), catalog_table(table, None));
                }
            }
        }
    }
    Ok(Some(out))
}

/// 保存者の見え方のテーブル（[`data::DataStore::get_table_for_viewer`]・見えない・無いテーブルは `None`）。
async fn viewer_table(
    data: &data::DataStore,
    ctx: &authz::AuthContext,
    id: uuid::Uuid,
) -> Result<Option<data::DataTable>, String> {
    match data.get_table_for_viewer(ctx, id, None).await {
        Ok(t) => Ok(Some(t)),
        Err(data::DataError::NotFound | data::DataError::Forbidden) => Ok(None),
        Err(e) => Err(format!("状態の進め方を使うテーブル: {e}")),
    }
}

/// 状態の列（FSM で管理し、保存者が読めるときだけ・読めなければ定義の絞り込みが列ごと落とす）。
fn status_def(schema: &data::TableSchema) -> Option<&data::FieldDef> {
    schema
        .fsm_ref
        .as_ref()
        .and(schema.status_field.as_deref())
        .and_then(|k| schema.field(k))
}

/// 保存者の見え方の定義から、V4 の材料のテーブルを作る。
fn catalog_table(
    table: data::DataTable,
    fsm_initial: Option<data::FsmInitial>,
) -> workflow_engine::CatalogTable {
    let schema = &table.schema;
    let status_col = status_def(schema);
    let states = status_col.map(|f| f.options.clone());
    let status_field = status_col.map(|f| f.key.clone());
    let fields = schema
        .fields
        .iter()
        .map(|f| {
            let ty = serde_json::to_value(f.field_type)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            (f.key.clone(), ty)
        })
        .collect();
    // 重複を許さない列（読める列だけ・同じレコードを探す列の照合・GAP-16）。
    let unique = schema
        .fields
        .iter()
        .filter(|f| f.unique)
        .map(|f| f.key.clone())
        .collect();
    workflow_engine::CatalogTable {
        states,
        status_field,
        fields,
        unique,
        // データを検索の列は、実行時の束縛と同じ data の判定で照らす（保存者の見え方の定義・BUG-244）。
        schema: Some(table.schema),
        fsm_initial,
    }
}

/// IR が名指しするテーブル・フォルダー・ファイルのうち、保存者に見えるものを引いて V4 の材料にする（GAP-35・GAP-91）。
///
/// 何を引くかは workflow-engine の [`workflow_engine::referenced_tables`]・[`workflow_engine::referenced_folders`]・
/// [`workflow_engine::referenced_files`]（照合と同じ位置の表）が決める。テーブルは閲覧か追加だけ
/// （[`data::DataStore::table_labels`]）、フォルダーとファイルは閲覧（[`storage::StorageService::node_labels`]・
/// 種類つきで一緒に引く）で見えるものを、まとめて引く（1 回に引ける数ずつ）。見えない・無い・消した・種類の違うものは
/// 入れない（V4 が同じ誤りで止める・存在を明かさない）。IR が型どおりに読めなければ引かない（V1 が止める）。引く口が
/// 無い構成は `None`（照合しない）。UUID の名指しが無くても、引く口があれば空の集合にする（UUID でない書き損じだけの
/// IR も V4 が止める）。
pub async fn visible_refs(
    data: Option<&data::DataStore>,
    storage: Option<&storage::StorageService>,
    ctx: &authz::AuthContext,
    ir: &serde_json::Value,
) -> Result<VisibleRefs, String> {
    let Ok(parsed) = workflow_engine::WorkflowIr::from_json(ir) else {
        return Ok(VisibleRefs::default());
    };
    let tables: Vec<uuid::Uuid> = workflow_engine::referenced_tables(&parsed)
        .into_iter()
        .collect();
    let folders = workflow_engine::referenced_folders(&parsed);
    let files = workflow_engine::referenced_files(&parsed);
    let mut out = VisibleRefs::default();
    if let Some(data) = data {
        let mut seen = std::collections::BTreeSet::new();
        for chunk in tables.chunks(data::MAX_TABLE_LABELS) {
            let labels = data
                .table_labels(ctx, chunk)
                .await
                .map_err(|e| format!("ワークフローが使うテーブル: {e}"))?;
            seen.extend(labels.into_keys().map(|id| id.hyphenated().to_string()));
        }
        out.tables = Some(seen);
    }
    if let Some(storage) = storage {
        let wanted: Vec<(uuid::Uuid, storage::NodeKind)> = folders
            .iter()
            .map(|id| (*id, storage::NodeKind::Folder))
            .chain(files.iter().map(|id| (*id, storage::NodeKind::File)))
            .collect();
        let mut seen_folders = std::collections::BTreeSet::new();
        let mut seen_files = std::collections::BTreeSet::new();
        for chunk in wanted.chunks(storage::MAX_NODE_LABELS) {
            let labels = storage
                .node_labels(ctx, chunk)
                .await
                .map_err(|e| format!("ワークフローが使うフォルダー・ファイル: {e}"))?;
            for (id, label) in labels {
                let key = id.hyphenated().to_string();
                match label.kind {
                    storage::NodeKind::Folder => seen_folders.insert(key),
                    storage::NodeKind::File => seen_files.insert(key),
                };
            }
        }
        out.folders = Some(seen_folders);
        out.files = Some(seen_files);
    }
    Ok(out)
}

/// 保存者に見える、IR が名指しするテーブル・フォルダー・ファイル（[`visible_refs`]・`None` は照合しない）。
#[derive(Debug, Default)]
pub struct VisibleRefs {
    pub tables: Option<std::collections::BTreeSet<String>>,
    pub folders: Option<std::collections::BTreeSet<String>>,
    pub files: Option<std::collections::BTreeSet<String>>,
}

impl VisibleRefs {
    /// カタログに詰める（[`Catalog::table_ids`]・[`Catalog::folder_ids`]・[`Catalog::file_ids`]）。
    pub fn fill(self, catalog: &mut Catalog) {
        catalog.table_ids = self.tables;
        catalog.folder_ids = self.folders;
        catalog.file_ids = self.files;
    }
}

/// 別のワークフローを実行（workflow.start）が名前で指す相手のうち、保存者に見えるものを引いて V4 の材料にする
/// （BUG-107）。
///
/// IR に workflow.start が無ければ `None`（照合しない）。引くのは IR の名前（リテラル）だけで、ワークフローの
/// 一覧を全部は引かない。無い名前と見えない相手はどちらも入れない（V4 が同じ文言で止める・存在を明かさない）。
pub async fn workflow_start_targets(
    artifacts: &artifact::ArtifactStore,
    ctx: &authz::AuthContext,
    ir: &serde_json::Value,
) -> Result<Option<std::collections::BTreeSet<String>>, String> {
    let starts: Vec<&serde_json::Value> = ir
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|n| {
            n.get("type").and_then(serde_json::Value::as_str)
                == Some(workflow_engine::vocab::NodeType::WorkflowStart.as_str())
        })
        .collect();
    if starts.is_empty() {
        return Ok(None);
    }
    let names: std::collections::BTreeSet<&str> = starts
        .iter()
        .filter_map(|n| n.pointer("/params/name")?.as_str())
        .filter(|name| !name.trim().is_empty())
        .collect();
    let mut visible = std::collections::BTreeSet::new();
    for name in names {
        match artifacts
            .get_by_name(ctx, artifact::ArtifactKind::Workflow, name, None)
            .await
        {
            Ok(_) => {
                visible.insert(name.to_string());
            }
            Err(artifact::ArtifactError::NotFound | artifact::ArtifactError::Forbidden) => {}
            Err(e) => return Err(format!("実行するワークフロー: {e}")),
        }
    }
    Ok(Some(visible))
}

/// chat ワーカー（emit_workflow）へ注入するカタログ源。保存 API と同じ材料を持つ。
pub struct ApiWorkflowCatalogSource {
    secrets: Option<Arc<secrets::SecretStore>>,
    skill_installs: Option<Arc<app_platform::SkillInstallService>>,
    models: WorkflowModels,
    /// AI エージェントの web.fetch が読んでよいサイトの天井（保存 API と同じ設定・BUG-6）。
    web_ceiling: Option<Vec<String>>,
    /// 承認（human.approval）が束縛するテーブルを引く（保存 API の `build_catalog_for` と同じ・GAP-08）。
    data: Option<Arc<data::DataStore>>,
    /// workflow.start の相手を引く（保存 API の `build_catalog_for` と同じ・BUG-107）。
    artifacts: Option<Arc<artifact::ArtifactStore>>,
    /// IR が名指しするフォルダーを引く（保存 API の `build_catalog_for` と同じ・V4・GAP-35）。
    storage: Option<Arc<storage::StorageService>>,
}

impl ApiWorkflowCatalogSource {
    #[must_use]
    pub fn new(
        secrets: Option<Arc<secrets::SecretStore>>,
        skill_installs: Option<Arc<app_platform::SkillInstallService>>,
        models: WorkflowModels,
    ) -> Self {
        ApiWorkflowCatalogSource {
            secrets,
            skill_installs,
            models,
            web_ceiling: None,
            data: None,
            artifacts: None,
            storage: None,
        }
    }

    /// IR が名指しするフォルダーを照合する（保存 API と同じ StorageService を渡す・V4・GAP-35）。
    #[must_use]
    pub fn with_storage(mut self, storage: Arc<storage::StorageService>) -> Self {
        self.storage = Some(storage);
        self
    }

    /// workflow.start の相手を照合する（保存 API と同じ ArtifactStore を渡す・V4・BUG-107）。
    #[must_use]
    pub fn with_artifacts(mut self, artifacts: Arc<artifact::ArtifactStore>) -> Self {
        self.artifacts = Some(artifacts);
        self
    }

    /// 承認のテーブルを照合する（保存 API と同じ DataStore を渡す・V4・GAP-08）。
    #[must_use]
    pub fn with_data(mut self, data: Arc<data::DataStore>) -> Self {
        self.data = Some(data);
        self
    }

    /// web.fetch の天井を照合する（`workflow.agent.web_allowlist`・保存 API と同じ値を渡す）。
    #[must_use]
    pub fn with_web_ceiling(mut self, hosts: Vec<String>) -> Self {
        self.web_ceiling = Some(hosts);
        self
    }
}

#[async_trait::async_trait]
impl chat::WorkflowCatalogSource for ApiWorkflowCatalogSource {
    async fn catalog(&self, ctx: &authz::AuthContext) -> Result<Catalog, String> {
        let mut catalog = build_catalog_from(
            self.secrets.as_deref(),
            self.skill_installs.as_deref(),
            &self.models,
            ctx,
        )
        .await
        .map_err(|e| format!("カタログ構築に失敗: {e}"))?;
        catalog.web_ceiling.clone_from(&self.web_ceiling);
        Ok(catalog)
    }

    /// 説明文用のモデル一覧（secret・skill の一覧は引かない・照合と同じ集合）。
    async fn models(&self, ctx: &authz::AuthContext) -> Result<Vec<String>, String> {
        self.models.ids(ctx).await
    }

    /// 説明文用の判定用のモデルの一覧（id と表示名・照合と同じ集合・保存する人の制限を通す・D-79・D-169）。
    async fn decision_models(
        &self,
        ctx: &authz::AuthContext,
    ) -> Result<Vec<chat::DecisionModel>, String> {
        self.models.decision_models(ctx).await
    }

    /// 保存 API（`build_catalog_for`）と同じく、IR の承認が束縛するテーブル・workflow.start の相手・名指しする
    /// テーブルとフォルダーを保存者の権限で引いて足す。
    async fn catalog_for(
        &self,
        ctx: &authz::AuthContext,
        ir: &serde_json::Value,
    ) -> Result<Catalog, String> {
        let mut catalog = chat::WorkflowCatalogSource::catalog(self, ctx).await?;
        if let Some(data) = &self.data {
            catalog.tables = fsm_tables(data, ctx, ir).await?;
        }
        if let Some(artifacts) = &self.artifacts {
            catalog.workflows = workflow_start_targets(artifacts, ctx, ir).await?;
        }
        visible_refs(self.data.as_deref(), self.storage.as_deref(), ctx, ir)
            .await?
            .fill(&mut catalog);
        Ok(catalog)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use llm_gateway::tenant::node_effort_applies;
    use llm_gateway::{TenantLlmSettings, AUTO_MODEL};
    use serde_json::json;
    use workflow_engine::{
        complete_declared_scopes, save_warnings, validate, Catalog, EFFORT_IGNORED,
    };

    use super::{default_is_auto, model_lock};

    /// 保存時の警告（使われない思考の深さ・workflow-engine）は、実行時の規則（llm-gateway の
    /// `node_effort_applies`）と、組織の設定とモデルの指定のすべての組み合わせで一致する（保存時の照合 V3 で
    /// 保存できない組み合わせは除く）。2 つの規則は別の crate にあるので、両方に依存する api で照らす。
    #[test]
    fn the_effort_warning_matches_the_runtime_rule() {
        let mut checked = 0;
        for default in [AUTO_MODEL, "a"] {
            for allow in [true, false] {
                let settings = TenantLlmSettings {
                    default_model: default.into(),
                    allow_user_choice: allow,
                    ..TenantLlmSettings::default()
                };
                let catalog = Catalog {
                    models: Some(vec!["a".into(), "b".into()]),
                    model_lock: model_lock(&settings),
                    default_is_auto: default_is_auto(&settings),
                    ..Catalog::default()
                };
                for model in [None, Some(""), Some("a"), Some("b")] {
                    let mut params = json!({ "prompt": "p", "effort": "low" });
                    if let Some(m) = model {
                        params["model"] = json!(m);
                    }
                    let ir = json!({
                        "ir_version": 1, "name": "wf", "triggers": [{ "kind": "interactive" }],
                        "nodes": [{ "id": "ai", "type": "llm.invoke", "params": params }],
                        "edges": []
                    });
                    let (ir, _) = complete_declared_scopes(&ir);
                    let Ok(parsed) = validate(&ir, &catalog) else {
                        // 選択を禁じた組織で既定と違うモデル（V3 の ir.model_choice_disabled）。
                        assert!(!allow, "{default} {model:?}");
                        continue;
                    };
                    let warned = save_warnings(&parsed, &catalog)
                        .iter()
                        .any(|w| w.code == EFFORT_IGNORED);
                    assert_eq!(
                        warned,
                        !node_effort_applies(&settings, model),
                        "既定 {default}・選択を許す {allow}・モデル {model:?}"
                    );
                    checked += 1;
                }
            }
        }
        // 選べる組織は 4 通りずつ、選択を禁じた組織は省略 2 通り（＋既定と同じモデル 1 通り）。
        assert_eq!(checked, 4 + 4 + 2 + 3);
    }
}
