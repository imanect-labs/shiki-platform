-- 組織の member と部署の定期の照合（D-136・migration 0141）の続きの位置を直す。
--
-- 1. 列の名前を cursor_tenant_id・cursor_user_id にする。tenant_id の列を持つ表は、テナントの撤去と移行
--    （storage::tenant_scope）がテナントの持ち物として集める。この行はどこまで読んだかの位置で、テナントのデータでも
--    副産物でもない。tenant_id のままだと、照合がそのテナントの人を読んでいる間は retenant が移行を断っていた。
-- 2. last_run_at（最後に照合を始めた時刻・DB の時刻）を足す。サーバーが何台あっても、照合は全体で
--    membership_reconcile.interval_secs に 1 回だけ進める（各サーバーのタイマーがロックを取れた回をすべて進めて
--    いたので、Keycloak と OpenFGA への読みがサーバーの台数倍になっていた）。
-- 3. 最後まで読んだら、行を消さずに位置だけを NULL にする（last_run_at を残す）。
--
--   cursor_tenant_id, cursor_user_id = 最後に照合を終えた人（NULL なら頭から）。

alter table membership_reconcile_cursor rename column tenant_id to cursor_tenant_id;
alter table membership_reconcile_cursor rename column user_id to cursor_user_id;

alter table membership_reconcile_cursor
    alter column cursor_tenant_id drop not null,
    alter column cursor_user_id drop not null,
    add column last_run_at timestamptz,
    add constraint membership_reconcile_cursor_pair
        check ((cursor_tenant_id is null) = (cursor_user_id is null));
