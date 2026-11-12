//! ファイルの複製。読み出し・新規作成はともに本人のStorageServiceを通す。
use std::sync::Arc;

use agent_core::{ArtifactRef, Tool, ToolError, ToolName, ToolOutcome};
use authz::AuthContext;
use serde::Deserialize;
use serde_json::{json, Value};
use storage::{StorageError, StorageService};
use uuid::Uuid;

pub struct DriveCopyTool {
    storage: Arc<StorageService>,
    run_id: Uuid,
}

impl DriveCopyTool {
    pub fn new(storage: Arc<StorageService>, run_id: Uuid) -> Self {
        Self { storage, run_id }
    }
}

#[derive(Deserialize)]
struct CopyInput {
    node_id: Uuid,
    new_name: String,
    #[serde(default)]
    destination_folder_id: Option<String>,
}

fn destination(raw: Option<&str>, current: Option<Uuid>) -> Result<Option<Uuid>, ToolError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(current),
        Some("root") => Ok(None),
        Some(s) => Uuid::parse_str(s).map(Some).map_err(|_| {
            ToolError::Invalid(
                "destination_folder_id はフォルダのUUIDかrootを指定してください".into(),
            )
        }),
    }
}

fn copy_error(error: StorageError) -> ToolOutcome {
    ToolOutcome::error(match error {
        StorageError::Forbidden | StorageError::NotFound => {
            "元ファイルまたは複製先にアクセスできません。".into()
        }
        StorageError::Conflict => {
            "複製先に同名ファイルがあります。別のnew_nameを指定してください。".into()
        }
        StorageError::Invalid(reason) => format!("複製できません: {reason}"),
        _ => "ファイルの複製に失敗しました。".into(),
    })
}

#[async_trait::async_trait]
impl Tool for DriveCopyTool {
    fn name(&self) -> &str {
        ToolName::DriveCopy.as_str()
    }
    fn description(&self) -> &'static str {
        "ドライブのファイルを複製する。元ファイルを残し、内容が同じ新しいファイルを作成する。Word/Excel/CSV/画像/ノート等のファイルが対象（フォルダは対象外）。new_nameで拡張子を保った別名を指定する。destination_folder_id省略時は元と同じフォルダ、rootはマイドライブ直下。元の閲覧権限と複製先の作成権限が必要。元の共有設定や版履歴は複製しない。"
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{
            "node_id":{"type":"string","format":"uuid","description":"元ファイルのID。drive.findで探す"},
            "new_name":{"type":"string","description":"拡張子を保った複製後のファイル名"},
            "destination_folder_id":{"type":"string","description":"複製先フォルダのIDまたはroot。省略時は元と同じフォルダ"}
        },"required":["node_id","new_name"]})
    }
    fn requires_confirmation(&self) -> bool {
        true
    }
    async fn call(
        &self,
        ctx: &AuthContext,
        input: Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let input: CopyInput = serde_json::from_value(input)
            .map_err(|_| ToolError::Invalid("node_idとnew_nameを指定してください".into()))?;
        let new_name = input.new_name.trim();
        if new_name.is_empty() {
            return Err(ToolError::Invalid("new_nameを指定してください".into()));
        }
        let requested_parent = destination(input.destination_folder_id.as_deref(), None)?;
        let metadata = match self
            .storage
            .get_metadata(ctx, input.node_id, trace_id)
            .await
        {
            Ok(node) => node,
            Err(e) => return Ok(copy_error(e)),
        };
        if std::path::Path::new(new_name)
            .extension()
            .map(std::ffi::OsStr::to_ascii_lowercase)
            != std::path::Path::new(&metadata.name)
                .extension()
                .map(std::ffi::OsStr::to_ascii_lowercase)
        {
            return Err(ToolError::Invalid(
                "元ファイルの拡張子を保ったnew_nameを指定してください".into(),
            ));
        }
        // 50MiBに固定上限を置く。StorageService自身のアップロード上限も適用される。
        let (source, bytes) = match self
            .storage
            .read_file_internal_bounded(ctx, input.node_id, 50 * 1024 * 1024, trace_id)
            .await
        {
            Ok(source) => source,
            Err(e) => return Ok(copy_error(e)),
        };
        let parent = if input
            .destination_folder_id
            .as_deref()
            .is_none_or(|s| s.trim().is_empty())
        {
            source.parent_id
        } else {
            requested_parent
        };
        let operation = json!({"run_id":self.run_id,"actor":ctx.subject().to_string(),"org":ctx.org(),"source":source.id,"name":new_name,"destination":input.destination_folder_id.as_deref().map(str::trim).filter(|s| !s.is_empty())});
        let key = format!(
            "chat-copy:{}",
            storage::content_address::sha256_hex(operation.to_string().as_bytes())
        );
        let digest = storage::content_address::sha256_hex(json!({"operation":operation,"parent":parent,"source_version":source.version,"sha256":source.blob_sha256}).to_string().as_bytes());
        let copied = match self
            .storage
            .write_file_internal_idempotent(
                ctx,
                parent,
                new_name,
                &bytes,
                source
                    .content_type
                    .as_deref()
                    .unwrap_or("application/octet-stream"),
                &key,
                &digest,
                trace_id,
            )
            .await
        {
            Ok(node) => node,
            Err(e) => return Ok(copy_error(e)),
        };
        let copied_id = copied
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| Uuid::parse_str(id).ok())
            .ok_or_else(|| ToolError::Internal("複製結果のIDが不正です".into()))?;
        let mut outcome = ToolOutcome::ok(format!("ファイルを複製しました。元ファイルは変更していません。\n元: {} (id: {})\n複製: {} (id: {})", source.name, source.id, new_name, copied_id));
        outcome.artifacts.push(ArtifactRef {
            node_id: copied_id.to_string(),
            name: new_name.into(),
        });
        Ok(outcome)
    }
}
