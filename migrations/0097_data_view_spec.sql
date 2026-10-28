-- Phase 15 Task 15.5: 型付き ViewSpec（data-platform §8.2）。
--
-- 1 ビューで表示（解決）できるルックアップ・ロールアップ列の上限をテナント設定で変えられるようにする。
-- 各列がページごとに参照先の行述語つきの解決を要するため、既定は 12 本（SharePoint のルックアップ 12 結合に倣う）。
-- NULL は既定。変更は組織管理者のみ（/org-admin/data/settings・監査に残す）。
--
-- 保存済みビュー（artifact kind=data_view）の本文は書き換えない。v1 の `display`（サーバ非解釈の JSON）は
-- 読取時に既定の表として読む（crates/data/src/view.rs の DataViewBody::from_stored）。
alter table data_tenant_settings
    add column max_view_lookup_columns integer
        check (max_view_lookup_columns is null or max_view_lookup_columns between 1 and 100);
