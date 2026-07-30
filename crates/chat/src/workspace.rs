//! ワークスペース保存アダプタ（Task 5.4/5.8・Durable Workspace）。
//!
//! agent-core の [`WorkspaceStore`] を `StorageService`（単一チョークポイント・認可/監査/版管理/書込イベント）
//! へ配線する。ワークスペースは **thread ごとの Drive フォルダ**（`root_folder_id`）で、全操作は
//! **発話ユーザーの `AuthContext`** で実行し昇格しない（confused-deputy 回避）。名前空間はフラット
//! （サブディレクトリ非対応・アルファ）。書込/削除は書込イベント→自動再索引に乗る（PIT-5 と経路分離）。
//!
//! # 封じ込め不変条件（#350 で明示化・テストは `tests/workspace_containment_it.rs`）
//!
//! 自律エージェントの fs ツールは**起動フォルダ（`root_folder_id`）配下から出られない**:
//!
//! 1. **名前解決は root の直下限定**: read/delete は `resolve_child_file(root, name)`、list は
//!    `list_children(root)`、write は `write_file_at(root, name)` のみを呼ぶ。名前は SQL の
//!    `(parent_id, name)` 完全一致で解決され、パスとして解釈されない（`..`・`/` 入り名は
//!    `validate_name` が拒否し、拒否をすり抜けても他フォルダの行に一致しようがない）。
//! 2. **node_id を受け付けない**: ツール入力は名前のみ。他フォルダのファイルは同名でも root 配下に
//!    無ければ「見つからない」になる（存在秘匿）。
//! 3. **権限は本人のまま**: 全操作は発話ユーザーの `AuthContext` で StorageService の認可
//!    （folder viewer/editor）を通る。エージェントだから読める/書けるものは何一つ増えない。
//!
//! フォルダ未指定の thread は `agent-workspace-<thread>` を自動生成して root にする（既存ファイルに
//! 触れない安全既定）。「このフォルダで作業」を明示選択した場合はその配下に作る
//! （worker/generate.rs `ensure_workspace`）。「未指定なら Drive 全体」は採らない（#350 決定）。
//!
//! # 生成は遅延（#392）
//!
//! フォルダは [`LazyWorkspace`] が **fs ツールが実際に呼ばれた時に初めて**作る。自律 run は必ず
//! fs ツールを提示するが、使わない run も多く（調査だけ・会話だけ）、無条件生成では空の
//! `agent-workspace-<uuid>` がドライブに増え続けるため。
//!
//! # システム領域（#392）
//!
//! 自動生成のワークスペースは**システム領域**（`node.system`）として作る。ドライブ一覧・名前検索・
//! ゴミ箱に出さず、書込イベントを RAG へ relay しない（使い捨ての作業メモを社内検索に載せない）。
//! 「このフォルダで作業」を明示選択した場合は**従来どおり可視・索引**（ユーザーがその場所を選んだ
//! のは成果物を見たいからで、勝手に隠さない）。認可は両者で完全に同一。

use std::sync::Arc;

use agent_core::{ToolError, WorkspaceEntry, WorkspaceStore, WorkspaceWrite};
use authz::AuthContext;
use storage::{ChildSort, NodeKind, StorageError, StorageService};
use tokio::sync::OnceCell;
use uuid::Uuid;

use crate::store::ChatStore;

/// 1 ページの取得件数（storage 側の 100 件クランプに合わせる）。
const LIST_PAGE: usize = 100;
/// ワークスペース列挙の全体上限（暴走防止・フラット namespace のアルファ既定）。
const MAX_WORKSPACE_FILES: usize = 2000;

/// `StorageService` 裏のワークスペース CRUD（shiki-server 本番配線）。
pub struct StorageWorkspaceStore {
    storage: Arc<StorageService>,
    /// thread ごとのワークスペースフォルダ（Drive 上の実フォルダ）。
    root_folder_id: Uuid,
}

impl StorageWorkspaceStore {
    pub fn new(storage: Arc<StorageService>, root_folder_id: Uuid) -> Self {
        StorageWorkspaceStore {
            storage,
            root_folder_id,
        }
    }
}

/// ワークスペースフォルダを**初回 fs ツール呼び出し時に**解決/作成する遅延ラッパ（#392）。
///
/// 自律 run は fs ツールを常に提示するが、使わない run でフォルダを作ってしまうと空の
/// `agent-workspace-<uuid>` がドライブに増える。生成をここに閉じ込め、
/// [`WorkspaceStore`] の各操作の直前に一度だけ ensure する（`OnceCell` で 1 回に収める）。
pub struct LazyWorkspace {
    store: ChatStore,
    storage: Arc<StorageService>,
    thread_id: Uuid,
    /// 自動生成のワークスペースか（true ならシステム領域として作る・#392）。
    system: bool,
    inner: OnceCell<StorageWorkspaceStore>,
}

impl LazyWorkspace {
    #[must_use]
    pub fn new(
        store: ChatStore,
        storage: Arc<StorageService>,
        thread_id: Uuid,
        system: bool,
    ) -> Self {
        LazyWorkspace {
            store,
            storage,
            thread_id,
            system,
            inner: OnceCell::new(),
        }
    }

    /// 解決済みのワークスペース（初回は作成する）。
    async fn resolved(&self, ctx: &AuthContext) -> Result<&StorageWorkspaceStore, ToolError> {
        self.inner
            .get_or_try_init(|| async {
                let folder_id = self.ensure_folder(ctx).await?;
                // 共有中の thread editor/owner にワークスペースフォルダの editor を行き渡らせる
                // （Task 5.6(a)・冪等）。失敗は操作を止めない（本人の書込には影響せず、次回再同期）。
                if let Err(e) = self
                    .store
                    .grant_workspace_to_members(ctx, self.thread_id)
                    .await
                {
                    tracing::warn!(thread_id = %self.thread_id, error = %e,
                        "workspace メンバー同期に失敗（次回再試行）");
                }
                Ok(StorageWorkspaceStore::new(self.storage.clone(), folder_id))
            })
            .await
    }

    /// thread のワークスペースフォルダを解決 or 作成する（Durable Workspace）。
    async fn ensure_folder(&self, ctx: &AuthContext) -> Result<Uuid, ToolError> {
        let tenant = &ctx.tenant_id;
        if let Some(id) = self
            .store
            .workspace_folder_id(self.thread_id, tenant)
            .await
            .map_err(|e| ToolError::Unavailable(format!("workspace 解決に失敗: {e}")))?
        {
            return Ok(id);
        }
        // 初回: ワークスペースフォルダを作り thread に紐づける。作成先の親は利用者が選んだ
        // workspace_parent_folder_id（無ければ Drive 直下＝None）。親フォルダの editor は
        // create_folder 内で本人 ctx により検証される（confused-deputy 防止）。
        // **thread ごとに一意な名前**にする（`node` の (parent,name) unique・別 thread と衝突しない）。
        let parent = self
            .store
            .workspace_parent_folder_id(self.thread_id, tenant)
            .await
            .map_err(|e| ToolError::Unavailable(format!("workspace 解決に失敗: {e}")))?;
        let name = format!("agent-workspace-{}", self.thread_id);
        let created = if self.system {
            self.storage
                .create_system_folder(ctx, parent, &name, None)
                .await
        } else {
            self.storage.create_folder(ctx, parent, &name, None).await
        };
        match created {
            Ok(node) => self
                .store
                .set_workspace_folder_if_absent(self.thread_id, tenant, node.id)
                .await
                .map_err(|e| ToolError::Unavailable(format!("workspace 紐付けに失敗: {e}"))),
            // 作成失敗は 2 種を区別する: ①同一 thread の並行 run が先に作った（unique 衝突）
            // なら workspace_folder_id が既に埋まっている → それを使う。②親フォルダが選択後に
            // 削除された/editor が剥奪された等の**実失敗**なら未設定のまま → 元エラーを返す
            // （黙って別の場所に作らない・fail-closed）。
            Err(e) => match self
                .store
                .workspace_folder_id(self.thread_id, tenant)
                .await
                .map_err(|e| ToolError::Unavailable(format!("workspace 解決に失敗: {e}")))?
            {
                Some(id) => Ok(id),
                None => Err(match e {
                    StorageError::Forbidden => ToolError::Invalid(
                        "ワークスペースの作成先フォルダに編集権限がありません".into(),
                    ),
                    StorageError::NotFound => ToolError::Invalid(
                        "ワークスペースの作成先フォルダが見つかりません（削除された可能性）".into(),
                    ),
                    other => ToolError::Unavailable(format!("workspace 作成に失敗: {other}")),
                }),
            },
        }
    }
}

#[async_trait::async_trait]
impl WorkspaceStore for LazyWorkspace {
    async fn list(
        &self,
        ctx: &AuthContext,
        trace_id: Option<&str>,
    ) -> Result<Vec<WorkspaceEntry>, ToolError> {
        self.resolved(ctx).await?.list(ctx, trace_id).await
    }

    async fn read(
        &self,
        ctx: &AuthContext,
        name: &str,
        trace_id: Option<&str>,
    ) -> Result<Vec<u8>, ToolError> {
        self.resolved(ctx).await?.read(ctx, name, trace_id).await
    }

    async fn write(
        &self,
        ctx: &AuthContext,
        name: &str,
        bytes: Vec<u8>,
        content_type: &str,
        trace_id: Option<&str>,
    ) -> Result<WorkspaceWrite, ToolError> {
        self.resolved(ctx)
            .await?
            .write(ctx, name, bytes, content_type, trace_id)
            .await
    }

    async fn append(
        &self,
        ctx: &AuthContext,
        name: &str,
        suffix: &str,
        content_type: &str,
        trace_id: Option<&str>,
    ) -> Result<WorkspaceWrite, ToolError> {
        self.resolved(ctx)
            .await?
            .append(ctx, name, suffix, content_type, trace_id)
            .await
    }

    async fn delete(
        &self,
        ctx: &AuthContext,
        name: &str,
        trace_id: Option<&str>,
    ) -> Result<(), ToolError> {
        self.resolved(ctx).await?.delete(ctx, name, trace_id).await
    }
}

/// StorageError をツール観測用の [`ToolError`] に写す（不正名はモデルが直せる Invalid へ）。
fn map_err(e: StorageError, what: &str) -> ToolError {
    match e {
        StorageError::Invalid(msg) => ToolError::Invalid(msg),
        StorageError::NotFound => ToolError::Invalid(format!("{what}: ファイルが見つかりません")),
        other => ToolError::Unavailable(format!("{what}: {other}")),
    }
}

#[async_trait::async_trait]
impl WorkspaceStore for StorageWorkspaceStore {
    async fn list(
        &self,
        ctx: &AuthContext,
        trace_id: Option<&str>,
    ) -> Result<Vec<WorkspaceEntry>, ToolError> {
        // `list_children` は 1 ページ 100 件上限にクランプされるため、`next_cursor` で**全件ページング**する
        // （100 超のワークスペースで file が切れないように）。全体上限 `MAX_WORKSPACE_FILES` で暴走を防ぐ。
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = self
                .storage
                .list_children(
                    ctx,
                    Some(self.root_folder_id),
                    ChildSort::default(),
                    cursor.as_deref(),
                    LIST_PAGE,
                    // エージェント自身のワークスペースは system でも列挙する（#392）。
                    // ここだけが include_system=true（他は全て false）。
                    true,
                    trace_id,
                )
                .await
                .map_err(|e| map_err(e, "list"))?;
            for n in page.items {
                if n.kind == NodeKind::File {
                    out.push(WorkspaceEntry {
                        name: n.name,
                        // size は非負に丸めてから u64 化（負値・欠損は 0）。
                        size: u64::try_from(n.size_bytes.unwrap_or(0)).unwrap_or(0),
                    });
                }
            }
            match page.next_cursor {
                Some(c) if out.len() < MAX_WORKSPACE_FILES => cursor = Some(c),
                _ => break,
            }
        }
        Ok(out)
    }

    async fn read(
        &self,
        ctx: &AuthContext,
        name: &str,
        trace_id: Option<&str>,
    ) -> Result<Vec<u8>, ToolError> {
        let node_id = self
            .storage
            .resolve_child_file(ctx, self.root_folder_id, name, trace_id)
            .await
            .map_err(|e| map_err(e, "read"))?
            .ok_or_else(|| ToolError::Invalid(format!("read: '{name}' が見つかりません")))?;
        let (_, bytes) = self
            .storage
            .read_file_internal(ctx, node_id, trace_id)
            .await
            .map_err(|e| map_err(e, "read"))?;
        Ok(bytes)
    }

    async fn write(
        &self,
        ctx: &AuthContext,
        name: &str,
        bytes: Vec<u8>,
        content_type: &str,
        trace_id: Option<&str>,
    ) -> Result<WorkspaceWrite, ToolError> {
        let out = self
            .storage
            .write_file_at(
                ctx,
                self.root_folder_id,
                name,
                &bytes,
                content_type,
                trace_id,
            )
            .await
            .map_err(|e| map_err(e, "write"))?;
        Ok(WorkspaceWrite {
            node_id: out.node_id.to_string(),
            name: name.to_string(),
            version: out.version,
            created: out.created,
        })
    }

    async fn append(
        &self,
        ctx: &AuthContext,
        name: &str,
        suffix: &str,
        content_type: &str,
        trace_id: Option<&str>,
    ) -> Result<WorkspaceWrite, ToolError> {
        // 既存内容の読み出し→連結→新版は StorageService 側の 1 txn（`(parent, name)` の
        // advisory lock 下）で行う。ここで read してから write すると並行追記で片方が消える。
        let out = self
            .storage
            .append_file_at(
                ctx,
                self.root_folder_id,
                name,
                suffix.as_bytes(),
                content_type,
                trace_id,
            )
            .await
            .map_err(|e| map_err(e, "append"))?;
        Ok(WorkspaceWrite {
            node_id: out.node_id.to_string(),
            name: name.to_string(),
            version: out.version,
            created: out.created,
        })
    }

    async fn delete(
        &self,
        ctx: &AuthContext,
        name: &str,
        trace_id: Option<&str>,
    ) -> Result<(), ToolError> {
        let node_id = self
            .storage
            .resolve_child_file(ctx, self.root_folder_id, name, trace_id)
            .await
            .map_err(|e| map_err(e, "delete"))?
            .ok_or_else(|| ToolError::Invalid(format!("delete: '{name}' が見つかりません")))?;
        self.storage
            .soft_delete_file(ctx, node_id, trace_id)
            .await
            .map_err(|e| map_err(e, "delete"))?;
        Ok(())
    }
}
