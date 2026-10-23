//! ミニアプリの画面の表（```data_grid）の描画（Task 15.7）。
//!
//! 画面から送れるのは `action_id`（`data_grid:{別名}`）と params（ページ位置・件数）だけ。
//! サーバーは**保存済みの版の画面とピン**を読み直し、
//!
//! 1. 画面にその別名の ```data_grid があり、
//! 2. 別名がミニアプリの束ねた保存ビュー（`data_views`）にある
//!
//! ときだけ [`ActionBinding::DataView`] を作って [`ActionDispatcher`] に渡す。ビューの定義は
//! バンドル権限で読み、**行・列は閲覧者本人の権限で毎回評価する**（保存者の権限を引き継がない）。
//! 起動の記録（`miniapp_action_run`）は作らない（読取で、何度でも描き直す）。

use authz::AuthContext;
use serde_json::Value;
use uuid::Uuid;

use crate::action::{ActionBinding, DataViewBinding};
use crate::dispatch::{ActionDispatcher, ActionError, ActionSource};
use crate::miniapp_actions::{map_gui_err, MiniAppActionRequest};
use crate::miniapp_store::MiniAppStore;

impl MiniAppStore {
    /// 画面の表を描く（本体 viewer・画面とピンから束縛を作り直す・閲覧者本人の権限で実行）。
    pub(crate) async fn invoke_data_grid(
        &self,
        ctx: &AuthContext,
        dispatcher: &ActionDispatcher,
        app_id: Uuid,
        req: &MiniAppActionRequest,
        alias: &str,
        trace_id: Option<&str>,
    ) -> Result<Value, ActionError> {
        // 1. 本体の viewer（解決が認可する・保存済みの版を検証し直す）。
        let resolved = self
            .resolve(ctx, app_id, Some(req.version), trace_id)
            .await
            .map_err(map_gui_err)?;
        let source = ActionSource::MiniApp {
            artifact_id: resolved.id,
            version: resolved.version,
        };
        let key = req.action_id.as_str();
        // 2. 保存済みの版の画面に、その別名の表がある（画面の表の宣言は信用しない）。
        let declared = crate::data_grid::data_grid_blocks(&resolved.body.screen)
            .iter()
            .any(|g| g.alias.as_deref() == Some(alias));
        let pin = resolved.body.data_views.iter().find(|v| v.alias == alias);
        let Some(pin) = pin.filter(|_| declared) else {
            dispatcher
                .deny(ctx, &source, key, "undeclared_data_grid", trace_id)
                .await;
            return Err(ActionError::NotFound);
        };
        // 3. 束縛はピンから作る（条件・並べ替えはピンした版のビューの DataQuery だけ）。
        let binding = ActionBinding::DataView(DataViewBinding {
            id: key.to_string(),
            view_id: pin.artifact_id,
            pinned_version: pin.version,
        });
        dispatcher
            .dispatch_binding(
                ctx,
                &source,
                &binding,
                key,
                Value::Object(req.params.clone()),
                trace_id,
            )
            .await
    }
}
