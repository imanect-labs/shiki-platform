use super::*;

#[test]
fn version_optimization_defaults_on_and_can_be_disabled() {
    assert!(valid_config().storage.version_storage.enabled);
    let partial: StorageConfig = serde_json::from_value(serde_json::json!({
        "backend": "s3", "version_storage": {"read_cost_bytes": 8192}
    }))
    .unwrap();
    assert!(partial.version_storage.enabled);
    assert_eq!(partial.version_storage.read_cost_bytes, 8192);
    let disabled: StorageConfig = serde_json::from_value(serde_json::json!({
        "backend": "s3", "version_storage": {"enabled": false}
    }))
    .unwrap();
    assert!(!disabled.version_storage.enabled);
    assert!(disabled.version_storage.validate().is_ok());
}

#[test]
fn version_optimization_rejects_invalid_resource_limits_at_startup() {
    let mut cfg = valid_config();
    // 2 本（jobq の LISTEN 1 本 ＋ 1 本）では版最適化の要件（3 本）に足りない。
    cfg.database.max_connections = 2;
    assert!(cfg
        .validate()
        .unwrap_err()
        .to_string()
        .contains("max_connections"));
    cfg.storage.version_storage.enabled = false;
    assert!(cfg.validate().is_ok());
    // 1 本はテナンシーに依らず jobq の LISTEN で拒否される（config/tests/jobq.rs）。
    cfg.auth.tenancy = Tenancy::Multi;
    assert!(cfg.validate().is_ok());
    cfg.database.max_connections = 1;
    assert!(cfg.validate().is_err());
    for (key, value) in [
        ("raw_grace_secs", 0),
        ("max_optimize_bytes", 67108865),
        ("read_cost_bytes", 1048577),
    ] {
        let mut cfg = valid_config();
        cfg.storage.version_storage =
            serde_json::from_value(serde_json::json!({key: value})).unwrap();
        assert!(cfg
            .validate()
            .unwrap_err()
            .to_string()
            .contains("version_storage"));
    }
}
