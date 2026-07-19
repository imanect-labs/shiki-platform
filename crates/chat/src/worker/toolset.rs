//! ツール束の構築（generate.rs から分割）。配線状況に応じて提示ツールを組み立てる。
//!
//! 提示可否のポリシー（未配線なら提示しない・下書き系は無条件）はここに集約する。

use std::sync::Arc;

use agent_core::{
    FsDeleteTool, FsEditTool, FsListTool, FsReadTool, FsWriteTool, GrepTool, ShellTool, Tool,
    WorkspaceStore,
};

use super::ChatWorker;

impl ChatWorker {
    /// ドキュメント共同編集ツールの配線（ノート=Task 11P.4／スライド=Task 11.3）。
    ///
    /// collab ハブと storage が両方配線されている時のみ提示する。編集は共有 Yjs へ
    /// 適用され、権限は実行主体の editor@file（human と同一経路・昇格しない・排他なし）。
    pub(super) fn push_collab_tools(&self, tools: &mut Vec<Arc<dyn Tool>>) {
        // 下書きツールは保存も共同編集もしない（確定は UI 保存）ため、collab/storage の
        // 配線に依存させない（下書き生成フローを任意配線構成でも使えるようにする）。
        tools.push(Arc::new(crate::document_tool::SaveNoteTool::new()));
        tools.push(Arc::new(crate::slide_tool::SaveSlideTool::new()));
        // 下書き CSV（csv_draft・下書き確定型・Task 11.11・storage 非依存・確定は UI 保存）。
        tools.push(Arc::new(crate::csv_tool::SaveCsvTool::new()));
        // 下書き Word 文書（document_draft・下書き確定型・#332・storage 非依存・確定は UI 保存）。
        tools.push(Arc::new(crate::office_draft_tool::SaveDocumentTool::new()));
        let (Some(collab), Some(storage)) = (&self.collab, &self.storage) else {
            return;
        };
        tools.push(Arc::new(crate::document_tool::DocumentReadTool::new(
            collab.clone(),
            storage.clone(),
        )));
        tools.push(Arc::new(crate::document_tool::DocumentEditTool::new(
            collab.clone(),
            storage.clone(),
        )));
        tools.push(Arc::new(crate::document_tool::DocumentEmbedTool::new(
            collab.clone(), // 本文への genui 埋め込み（非破壊 append・確認不要・#282）。
            storage.clone(),
        )));
        // AI スライド共同編集（slide.read / slide.edit・Task 11.3）: ノートと同じ
        // 共同編集参加者モデル（排他なし・editor@file・HTML はサーバ側サニタイズ）。
        tools.push(Arc::new(crate::slide_tool::SlideReadTool::new(
            collab.clone(),
            storage.clone(),
        )));
        tools.push(Arc::new(crate::slide_tool::SlideEditTool::new(
            collab.clone(),
            storage.clone(),
        )));
    }

    /// 自律ツール（file CRUD/grep/shell）を tools へ追加する。
    pub(super) fn push_autonomous_tools(
        &self,
        tools: &mut Vec<Arc<dyn Tool>>,
        workspace: Arc<dyn WorkspaceStore>,
    ) {
        tools.push(Arc::new(FsListTool::new(workspace.clone())));
        tools.push(Arc::new(FsReadTool::new(workspace.clone())));
        tools.push(Arc::new(GrepTool::new(workspace.clone())));
        tools.push(Arc::new(FsWriteTool::new(workspace.clone())));
        tools.push(Arc::new(FsEditTool::new(workspace.clone())));
        tools.push(Arc::new(FsDeleteTool::new(workspace.clone())));
        // shell はワークスペースを seed→sync する（sandbox 必須）。
        if let Some(sandbox) = &self.sandbox {
            tools.push(Arc::new(ShellTool::new(
                sandbox.clone(),
                workspace,
                self.config.sandbox_software.clone(),
                self.config.sandbox_backend,
            )));
        }
    }
}
