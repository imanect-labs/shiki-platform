-- Task 15.3: 構造化データのテナント設定（data-platform §5.4: 件数の上限はテナント設定で変更可能）。
--
-- 行が無いテナントは既定（件数の上限 10,000）。小さいテーブルしか持たない顧客には大きくして正確な件数を出せる。
-- 変更は組織管理者のみ（/org-admin/data/settings・監査に残す）。
create table data_tenant_settings (
    tenant_id  text        primary key,
    -- 上限付き正確カウントの上限（`Exact(n)` / `AtLeast(上限)`）。NULL は既定。
    count_cap  bigint      check (count_cap is null or count_cap between 1 and 10000000),
    updated_at timestamptz not null default now()
);
