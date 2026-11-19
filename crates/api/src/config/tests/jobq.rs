use super::*;

#[test]
fn jobq_defaults_and_bounds() {
    let cfg = valid_config();
    assert_eq!(cfg.jobq.dead_retention_days, 30);
    assert_eq!(cfg.jobq.dead_monitor_interval_secs, 60);
    assert_eq!(
        cfg.jobq.dead_retention(),
        std::time::Duration::from_secs(30 * 86_400)
    );
    assert_eq!(
        cfg.jobq.dead_monitor_interval(),
        std::time::Duration::from_secs(60)
    );

    for (days, ok) in [(0, false), (1, true), (3650, true), (3651, false)] {
        let mut c = valid_config();
        c.jobq.dead_retention_days = days;
        assert_eq!(c.validate().is_ok(), ok, "dead_retention_days={days}");
    }
    for (secs, ok) in [(9, false), (10, true), (3600, true), (3601, false)] {
        let mut c = valid_config();
        c.jobq.dead_monitor_interval_secs = secs;
        assert_eq!(
            c.validate().is_ok(),
            ok,
            "dead_monitor_interval_secs={secs}"
        );
    }
}

#[test]
fn single_connection_pool_is_rejected_because_listen_holds_one() {
    let mut cfg = valid_config();
    cfg.auth.tenancy = Tenancy::Single;
    cfg.storage.version_storage.enabled = false;
    cfg.database.max_connections = 1;
    let err = cfg.validate().unwrap_err().to_string();
    assert!(err.contains("LISTEN"), "{err}");
    cfg.database.max_connections = 2;
    assert!(cfg.validate().is_ok());
}
