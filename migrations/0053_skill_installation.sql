-- Phase 10 Task 10.11（#344）: skill の同意インストール（ユーザー単位）。
--
-- スキルストア = Phase 9 レジストリ（registry_entry・kind='skill' で流用・DDL 不要）に、
-- 「誰が何を自分のカタログへ入れたか」を持つインストール台帳を足す。インストールは
-- **ユーザー単位**（human 決定・#344）: カタログ（モデルに見せる一覧）はパーソナルで、
-- ワークフロー保存時の V4 照合は保存ユーザーのインストール集合に対して行い、
-- 実行時は実行主体の ReBAC で再検証する（fail-closed が最終防衛線・ir.md §8）。
--
-- (skill_id, skill_version) はインストール時点の解決結果の非正規化（カタログ表示・
-- ツール解決をレジストリ再解決なしで引くため）。registry_version はレジストリの
-- 公開バージョン文字列（IR の skill:<name>@<version> と同じ語彙）。

create table skill_installation (
    tenant_id         text        not null,
    org               text        not null,
    user_id           text        not null,
    name              text        not null,
    registry_entry_id uuid        not null,
    registry_version  text        not null,
    skill_id          uuid        not null,
    skill_version     bigint      not null,
    trust_tier        text        not null,
    created_at        timestamptz not null default now(),
    primary key (tenant_id, user_id, name),
    foreign key (tenant_id, registry_entry_id) references registry_entry (tenant_id, id)
);

-- カタログ列挙（ユーザーのインストール済み一覧）用。
create index skill_installation_user_idx on skill_installation (tenant_id, user_id, created_at desc);
