//! 監査ログの操作の種類（action）の語彙（#830）。
//!
//! 監査の書き込みはすべて [`AuditAction`] を通す（文字列を手で書かない）。表示名と分類も
//! ここだけに持ち、`cargo run -p shiki-storage --bin export-audit-ts` で TS へ生成して、
//! 監査ログ画面の絞り込み・表示名に使う（手書きの対応物を作らない・codegen が正）。
//!
//! **一度記録した文字列は変えない。** 監査ログに残った行は書き換えないので、名前を変えると
//! 古い行と新しい行が別の操作に見える。やめた操作も、古い行を読むために残してよい。

/// 操作の分類（監査ログ画面の「分類」で絞る単位）。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum AuditCategory {
    File,
    Folder,
    Share,
    Search,
    Chat,
    Agent,
    Workflow,
    App,
    Data,
    Secret,
    Admin,
    Audit,
    Gateway,
    System,
}

impl AuditCategory {
    /// すべての分類（表示順）。
    pub const ALL: &'static [AuditCategory] = &[
        AuditCategory::File,
        AuditCategory::Folder,
        AuditCategory::Share,
        AuditCategory::Search,
        AuditCategory::Chat,
        AuditCategory::Agent,
        AuditCategory::Workflow,
        AuditCategory::App,
        AuditCategory::Data,
        AuditCategory::Secret,
        AuditCategory::Admin,
        AuditCategory::Audit,
        AuditCategory::Gateway,
        AuditCategory::System,
    ];

    /// API・TS で使う識別子。
    pub const fn as_str(self) -> &'static str {
        match self {
            AuditCategory::File => "file",
            AuditCategory::Folder => "folder",
            AuditCategory::Share => "share",
            AuditCategory::Search => "search",
            AuditCategory::Chat => "chat",
            AuditCategory::Agent => "agent",
            AuditCategory::Workflow => "workflow",
            AuditCategory::App => "app",
            AuditCategory::Data => "data",
            AuditCategory::Secret => "secret",
            AuditCategory::Admin => "admin",
            AuditCategory::Audit => "audit",
            AuditCategory::Gateway => "gateway",
            AuditCategory::System => "system",
        }
    }

    /// 識別子から引く（API の絞り込み条件）。
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|c| c.as_str() == s)
    }

    /// 画面の表示名。
    pub const fn label(self) -> &'static str {
        match self {
            AuditCategory::File => "ファイル",
            AuditCategory::Folder => "フォルダ",
            AuditCategory::Share => "共有",
            AuditCategory::Search => "検索・参照",
            AuditCategory::Chat => "チャット",
            AuditCategory::Agent => "エージェント",
            AuditCategory::Workflow => "ワークフロー",
            AuditCategory::App => "アプリ・スキル",
            AuditCategory::Data => "データ",
            AuditCategory::Secret => "シークレット",
            AuditCategory::Admin => "管理",
            AuditCategory::Audit => "監査ログ",
            AuditCategory::Gateway => "公開 API",
            AuditCategory::System => "システム",
        }
    }

    /// この分類に属する操作。
    pub fn actions(self) -> impl Iterator<Item = AuditAction> {
        AuditAction::ALL
            .iter()
            .copied()
            .filter(move |a| a.category() == self)
    }
}

/// `名前 = "記録する文字列", "表示名", 分類;` の表から [`AuditAction`] を作る。
macro_rules! audit_actions {
    ($($variant:ident = $s:literal, $label:literal, $cat:ident;)*) => {
        /// 監査ログの操作の種類。
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum AuditAction {
            $($variant,)*
        }

        impl AuditAction {
            /// すべての操作。
            pub const ALL: &'static [AuditAction] = &[$(AuditAction::$variant,)*];

            /// 監査ログに記録する文字列（`file.delete` など）。
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(AuditAction::$variant => $s,)*
                }
            }

            /// 画面の表示名。
            pub const fn label(self) -> &'static str {
                match self {
                    $(AuditAction::$variant => $label,)*
                }
            }

            /// 分類。
            pub const fn category(self) -> AuditCategory {
                match self {
                    $(AuditAction::$variant => AuditCategory::$cat,)*
                }
            }
        }
    };
}

impl AuditAction {
    /// 記録された文字列から引く（語彙に無い古い行は `None`）。
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|a| a.as_str() == s)
    }
}

impl std::fmt::Display for AuditAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// 記録する文字列は変えない（古い行と食い違う）。足すときは分類と表示名も一緒に決める。
audit_actions! {
    AgentApprovalDecide = "agent.approval.decide", "承認の判断（権限確認）", Agent;
    AgentApprovalDecision = "agent.approval.decision", "承認を判断", Agent;
    AppBundlePut = "app.bundle.put", "アプリの部品を登録", App;
    AppImport = "app.import", "アプリを取り込み", App;
    AppInstall = "app.install", "アプリをインストール", App;
    AppInstallSignature = "app.install.signature", "アプリの署名を検証", App;
    AppUninstall = "app.uninstall", "アプリをアンインストール", App;
    ArtifactCreate = "artifact.create", "アーティファクトを作成", App;
    ArtifactDelete = "artifact.delete", "アーティファクトを削除", App;
    ArtifactGet = "artifact.get", "アーティファクトを参照", App;
    ArtifactReadViaBundle = "artifact.read_via_bundle", "バンドル経由で部品を参照", App;
    ArtifactReadViaReference = "artifact.read_via_reference", "参照元にピンされた版を参照", App;
    ArtifactShare = "artifact.share", "アーティファクトを共有", Share;
    ArtifactSharesList = "artifact.shares.list", "アーティファクトの共有先を参照", Share;
    ArtifactUnshare = "artifact.unshare", "アーティファクトの共有を解除", Share;
    ArtifactVersionAppend = "artifact.version.append", "アーティファクトの版を追加", App;
    ArtifactVersionGet = "artifact.version.get", "アーティファクトの版を参照", App;
    ArtifactVersionsList = "artifact.versions.list", "アーティファクトの版の一覧", App;
    DataDerivedDemote = "data.derived.demote", "材料化した列を降格", Data;
    DataIndexApprove = "data.index.approve", "索引の作成を承認", Data;
    DataIndexList = "data.index.list", "索引の一覧", Data;
    DataIndexPromote = "data.index.promote", "列を索引に昇格", Data;
    DataQuery = "data.query", "データを検索", Data;
    DataQueryAggregate = "data.query.aggregate", "データを集計", Data;
    DataQueryLink = "data.query.link", "リンク先のテーブルを参照（検索）", Data;
    DataQuerySql = "data.query.sql", "SQL でデータを検索", Data;
    DataRagPostFilter = "data.rag.post_filter", "検索で見つけたレコードを権限で絞る（AI）", Search;
    DataRecordBulkApply = "data.record.bulk_apply", "レコードを一括で反映", Data;
    DataRecordBulkImport = "data.record.bulk_import", "レコードを一括で取り込み", Data;
    DataRecordBulkImportStatus = "data.record.bulk_import.status", "一括取り込みの状態を参照", Data;
    DataRecordCount = "data.record.count", "レコードを数える", Data;
    DataRecordCreate = "data.record.create", "レコードを作成", Data;
    DataRecordDelete = "data.record.delete", "レコードを削除", Data;
    DataRecordGet = "data.record.get", "レコードを参照", Data;
    DataRecordList = "data.record.list", "レコードの一覧", Data;
    DataRecordRevisions = "data.record.revisions", "レコードの変更履歴", Data;
    DataRecordShare = "data.record.share", "レコードを共有", Share;
    DataRecordTransition = "data.record.transition", "レコードの状態を遷移", Data;
    DataRecordUnshare = "data.record.unshare", "レコードの共有を解除", Share;
    DataRecordUpdate = "data.record.update", "レコードを更新", Data;
    DataStreamAwareness = "data.stream.awareness", "カーソル位置を共有", Data;
    DataStreamEnd = "data.stream.end", "データの購読を終了", Data;
    DataStreamSubscribe = "data.stream.subscribe", "データを購読", Data;
    DataTableCreate = "data.table.create", "テーブルを作成", Data;
    DataTableDelete = "data.table.delete", "テーブルを削除", Data;
    DataTableFixedPlan = "data.table.fixed_plan", "テーブルの固定計画を切り替え", Admin;
    DataTableGet = "data.table.get", "テーブルを参照", Data;
    DataTableUpdateSchema = "data.table.update_schema", "テーブルの列を変更", Data;
    DataTenantSettingsUpdate = "data.tenant_settings.update", "データの設定を変更", Admin;
    DataViewList = "data.view.list", "ビューの一覧", Data;
    FileBlocksRead = "file.blocks.read", "ファイルの本文を読む", File;
    FileContentUpdate = "file.content.update", "ファイルの内容を更新", File;
    FileDelete = "file.delete", "ファイルを削除", File;
    FileDownloadUrlIssue = "file.download_url.issue", "ダウンロード URL を発行", File;
    FileMetadataRead = "file.metadata.read", "ファイル情報を参照", File;
    FileReadInternal = "file.read.internal", "ファイルを読む（内部）", File;
    FileResolveWorkspace = "file.resolve.workspace", "作業フォルダのファイルを解決", File;
    FileRestore = "file.restore", "ファイルを復元", File;
    FileUpdate = "file.update", "ファイルを移動・名前変更", File;
    FileUploadFinalize = "file.upload.finalize", "アップロードを確定", File;
    FileUploadUrlIssue = "file.upload_url.issue", "アップロード URL を発行", File;
    FileVersionAdopt = "file.version.adopt", "版を採用", File;
    FileVersionDownloadUrlIssue = "file.version.download_url.issue", "版のダウンロード URL を発行", File;
    FileVersionRestore = "file.version.restore", "版を復元", File;
    FileVersionsList = "file.versions.list", "版の一覧", File;
    FileWriteContent = "file.write.content", "ファイルに書き込み", File;
    FileWriteInternal = "file.write.internal", "ファイルを作成（内部）", File;
    FileWritePropose = "file.write.propose", "ファイルの変更を提案", File;
    FileWriteWorkspace = "file.write.workspace", "作業フォルダに書き込み（権限確認）", File;
    FileWriteWorkspaceCreate = "file.write.workspace.create", "作業フォルダにファイルを作成", File;
    FileWriteWorkspaceUpdate = "file.write.workspace.update", "作業フォルダのファイルを更新", File;
    FolderChildrenList = "folder.children.list", "フォルダの中身の一覧", Folder;
    FolderCreate = "folder.create", "フォルダを作成", Folder;
    FolderDelete = "folder.delete", "フォルダを削除", Folder;
    FolderRestore = "folder.restore", "フォルダを復元", Folder;
    FolderUpdate = "folder.update", "フォルダを移動・名前変更", Folder;
    GatewayCall = "gateway.call", "公開 API を呼び出し", Gateway;
    GatewayNotifySend = "gateway.notify.send", "通知を送信", Gateway;
    InsightsClassify = "insights.classify", "AI 活用の用途を分類", System;
    MeChatgptConnect = "me.chatgpt.connect", "ChatGPT プランを連携", Chat;
    MeChatgptDisconnect = "me.chatgpt.disconnect", "ChatGPT プランの連携を解除", Chat;
    MeCustomInstructionsUpdate = "me.custom_instructions.update", "カスタム指示を変更", Chat;
    MiniappResolve = "miniapp.resolve", "ミニアプリを開く", App;
    NodeBreadcrumbRead = "node.breadcrumb.read", "フォルダの階層を参照", Folder;
    NodeSearch = "node.search", "ドライブを検索", Search;
    NodeShare = "node.share", "ファイル・フォルダを共有", Share;
    NodeShareLinkCreate = "node.share_link.create", "共有リンクを作成", Share;
    NodeShareLinkExpire = "node.share_link.expire", "共有リンクが期限切れ", Share;
    NodeShareLinkExtend = "node.share_link.extend", "共有リンクの期限を延長", Share;
    NodeShareLinkGrantRevoke = "node.share_link.grant.revoke", "共有リンクで得た権限を取り消し", Share;
    NodeShareLinkGrantsList = "node.share_link.grants.list", "共有リンクで得た権限の一覧", Share;
    NodeShareLinkList = "node.share_link.list", "共有リンクの一覧", Share;
    NodeShareLinkRedeem = "node.share_link.redeem", "共有リンクを使用", Share;
    NodeShareLinkRevoke = "node.share_link.revoke", "共有リンクを無効化", Share;
    NodeSharedWithMeList = "node.shared_with_me.list", "共有されたものの一覧", Share;
    NodeSharesList = "node.shares.list", "共有先の一覧", Share;
    NodeSystemGet = "node.system.get", "システムフォルダか確認", Folder;
    NodeUnshare = "node.unshare", "ファイル・フォルダの共有を解除", Share;
    OrgAdminAccess = "org_admin.access", "管理画面へのアクセス", Admin;
    OrgAdminAiSettingsUpdate = "org_admin.ai_settings.update", "AI 設定を変更", Admin;
    OrgAdminInsightsPeopleView = "org_admin.insights.people.view", "利用者別の AI 活用を閲覧", Admin;
    OrgAdminInsightsSettingsUpdate = "org_admin.insights_settings.update", "AI 活用分析の設定を変更", Admin;
    OrgAuditAccess = "org_audit.access", "監査ログへのアクセス", Audit;
    OrgAuditExport = "org_audit.export", "監査ログを書き出し", Audit;
    OrgAuditSearch = "org_audit.search", "監査ログを検索", Audit;
    OrgAuditViewerGrant = "org_audit.viewer.grant", "監査閲覧者を付与", Audit;
    OrgAuditViewerRevoke = "org_audit.viewer.revoke", "監査閲覧者を剥奪", Audit;
    RagDocRead = "rag.doc_read", "文書を読む（AI）", Search;
    RagSearch = "rag.search", "文書を検索（AI）", Search;
    SecretBindingUpdate = "secret.binding.update", "シークレットの紐付けを変更", Secret;
    SecretCreate = "secret.create", "シークレットを作成", Secret;
    SecretDelete = "secret.delete", "シークレットを削除", Secret;
    SecretDestinationDenied = "secret.destination_denied", "シークレットの送信先を拒否", Secret;
    SecretGetMeta = "secret.get_meta", "シークレットの情報を参照", Secret;
    SecretGrantCanUse = "secret.grant_can_use", "シークレットの利用を許可", Secret;
    SecretResolve = "secret.resolve", "シークレットを使用", Secret;
    SecretRotate = "secret.rotate", "シークレットを更新", Secret;
    SkillApply = "skill.apply", "スキルを適用", App;
    SkillImport = "skill.import", "スキルを取り込み", App;
    SkillImportSignature = "skill.import.signature", "取り込むスキルの署名を検証", App;
    SkillInstall = "skill.install", "スキルをインストール", App;
    SkillInstallSignature = "skill.install.signature", "スキルの署名を検証", App;
    SkillInvoke = "skill.invoke", "スキルを呼び出し", App;
    SkillPublish = "skill.publish", "スキルを公開", App;
    SkillPublishSignature = "skill.publish.signature", "公開するスキルの署名を検証", App;
    SkillUninstall = "skill.uninstall", "スキルをアンインストール", App;
    StorageUploadGcExpire = "storage.upload_gc.expire", "未完了のアップロードを破棄", System;
    StorageUploadGcOrphanDelete = "storage.upload_gc.orphan_delete", "孤立したアップロードを削除", System;
    TenantProvision = "tenant.provision", "テナントを作成", Admin;
    TenantPurge = "tenant.purge", "テナントを撤去", Admin;
    TenantRetenant = "tenant.retenant", "テナント ID を変更", Admin;
    ThreadCreate = "thread.create", "スレッドを作成", Chat;
    ThreadDelete = "thread.delete", "スレッドを削除", Chat;
    ThreadFeedback = "thread.feedback", "回答を評価", Chat;
    ThreadGet = "thread.get", "スレッドを参照", Chat;
    ThreadMessage = "thread.message", "メッセージを参照", Chat;
    ThreadMessages = "thread.messages", "メッセージの一覧", Chat;
    ThreadPin = "thread.pin", "スレッドのピン留めを変更", Chat;
    ThreadPost = "thread.post", "メッセージを送信", Chat;
    ThreadRename = "thread.rename", "スレッドの名前を変更", Chat;
    ThreadRunCancel = "thread.run.cancel", "生成を中止", Chat;
    ThreadRunGet = "thread.run.get", "生成の状態を参照", Chat;
    ThreadSetAutonomousMode = "thread.set_autonomous_mode", "承認モードを変更", Chat;
    ThreadSetOrigin = "thread.set_origin", "スレッドの起点を設定", Chat;
    ThreadSetPins = "thread.set_pins", "固定する文書を変更", Chat;
    ThreadSetSkills = "thread.set_skills", "スキルを変更", Chat;
    ThreadShare = "thread.share", "スレッドを共有", Share;
    ThreadSharesList = "thread.shares.list", "スレッドの共有先の一覧", Share;
    ThreadStream = "thread.stream", "生成の経過を受信", Chat;
    ThreadUiAction = "thread.ui_action", "画面部品の操作を受け付け", Chat;
    ThreadUnshare = "thread.unshare", "スレッドの共有を解除", Share;
    ThreadYuiSaveAsApp = "thread.yui_save_as_app", "図をアプリとして保存", Chat;
    TrashList = "trash.list", "ごみ箱の一覧", File;
    UiActionInvoke = "ui_action.invoke", "画面部品の操作を実行", Agent;
    WorkflowCapabilityOutOfScope = "workflow.capability.out_of_scope", "宣言にない能力の呼び出しを拒否", Workflow;
    WorkflowDataDelegated = "workflow.data.delegated", "委譲されたテーブルかを確認", Workflow;
    WorkflowDisable = "workflow.disable", "ワークフローを無効化", Workflow;
    WorkflowEnable = "workflow.enable", "ワークフローを有効化", Workflow;
    WorkspaceSelect = "workspace.select", "作業フォルダを選択", Folder;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn strings_are_unique_and_well_formed() {
        let mut seen = HashSet::new();
        for a in AuditAction::ALL {
            let s = a.as_str();
            assert!(seen.insert(s), "重複: {s}");
            assert!(
                s.split('.').count() >= 2
                    && s.split('.')
                        .all(|p| !p.is_empty()
                            && p.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')),
                "形式（小文字と _ を . で 2 段以上）: {s}"
            );
            assert!(!a.label().is_empty() && a.label() != s, "表示名: {s}");
            assert_eq!(AuditAction::parse(s), Some(*a));
        }
    }

    #[test]
    fn categories_round_trip_and_partition_actions() {
        let mut total = 0;
        for c in AuditCategory::ALL {
            assert_eq!(AuditCategory::parse(c.as_str()), Some(*c));
            // API（serde）の表記と識別子が一致する。
            assert_eq!(
                serde_json::to_value(c).ok(),
                Some(serde_json::json!(c.as_str()))
            );
            total += c.actions().count();
        }
        assert_eq!(total, AuditAction::ALL.len());
        assert_eq!(AuditCategory::parse("nope"), None);
        assert_eq!(AuditAction::parse("nope.nope"), None);
    }
}
