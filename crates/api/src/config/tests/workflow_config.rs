//! ワークフローの設定（`workflow.*`）の起動時の確かめのテスト。

use super::*;

/// 内部の送り先（`workflow.http_internal_hosts`）に読めない項目（ワイルドカードなど）があれば、起動を止める（黙って
/// 外すと、管理者は社内の送り先を書いたつもりで、実行のときに断られる理由が分からない・D-135）。
#[test]
fn unreadable_internal_hosts_stop_the_start() {
    let mut cfg = valid_config();
    cfg.workflow.http_internal_hosts = vec!["erp.corp.example".into(), "10.0.0.5:8443".into()];
    assert!(cfg.validate().is_ok());
    cfg.workflow
        .http_internal_hosts
        .push("*.corp.example".into());
    let err = cfg.validate().unwrap_err().to_string();
    assert!(
        err.contains("workflow.http_internal_hosts の「*.corp.example」を読めません。"),
        "{err}"
    );
}
