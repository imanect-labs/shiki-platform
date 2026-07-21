-- 一般アクセス（共有リンクの公開範囲・#338）。Google Drive の「一般アクセス」に相当。
--
-- 認可の正本は OpenFGA タプル（file/folder の viewer/editor に organization#member / user:*）で、
-- ここはその**ポリシー台帳**（レベル・役割・有効期限・パスワード）を持つ。行が無い node は
-- restricted（＝既存アクセス者のみ・現状の ReBAC）を意味する。owner のみが設定できる。
--
-- 有効期限は OpenFGA にネイティブ TTL が無いため、① セッション開始時の遅延失効
-- （expires_at 判定でタプル先行剥奪）と ② イベント駆動タイマ（expires_at の瞬間に剥奪）で
-- 強制する。定期ポーリングはしない。
create table node_general_access (
    -- 対象ノード（1 ノード 1 ポリシー）。node への FK は張らない（node 削除時は
    -- purge_tenant / 明示 clear で回収。ソフトデリートとの整合を単純化）。
    node_id       uuid        not null,
    -- テナント/組織スコープ（SAAS.1。sweeper が AuthContext 無しで Namespace を再構成するために保持）。
    tenant_id     text        not null,
    org           text        not null,
    -- 'file' | 'folder'（タイマ/遅延失効がタプル剥奪時に FgaObject を再構成するため保持）。
    kind          text        not null,
    -- 'organization'（組織内） | 'anyone'（すべての認証済みユーザー）。restricted は行の不在で表す。
    level         text        not null,
    -- 'viewer' | 'editor'。付与する権限。
    role          text        not null,
    -- 有効期限（NULL = 無期限）。一般アクセスにのみ設定できる。
    expires_at    timestamptz,
    -- パスワード（Argon2id PHC 文字列・NULL = パスワード無し）。設定時は broad タプルを書かず
    -- redeem 経由で per-user タプルを発行する。API には決して返さない（has_password のみ露出）。
    password_hash text,
    created_by    text        not null,
    updated_by    text        not null,
    created_at    timestamptz not null default now(),
    updated_at    timestamptz not null default now(),
    primary key (node_id)
);

-- 期限切れ行の走査（イベント駆動タイマ・遅延失効）。期限付き行のみ載る部分インデックス。
create index node_general_access_sweep_idx
    on node_general_access (expires_at) where expires_at is not null;
-- テナント撤去（purge_tenant）のスキャン。
create index node_general_access_tenant_idx
    on node_general_access (tenant_id);

-- パスワード redeem で発行した per-user タプルの台帳（#338）。
--
-- redeem で書く viewer/editor タプルは「明示共有」とバイト等価のため、失効処理が
-- 明示共有を誤って剥奪しないよう、redeem 由来の付与だけをここに記録する（sweeper は
-- この台帳の user のみ剥奪する）。
create table node_general_access_grant (
    node_id     uuid        not null,
    -- ローカル user id（subject = Namespace::user(user_id) で再構成）。
    user_id     text        not null,
    tenant_id   text        not null,
    -- 'file' | 'folder'（失効処理がタプル剥奪時に FgaObject を再構成するため保持・node JOIN 不要）。
    kind        text        not null,
    -- redeem 時点の role スナップショット（後でポリシーが変わっても剥奪対象を一意に決める）。
    role        text        not null,
    -- redeem 時点の expires_at スナップショット（NULL = 無期限）。
    expires_at  timestamptz,
    granted_at  timestamptz not null default now(),
    primary key (node_id, user_id)
);

create index node_general_access_grant_sweep_idx
    on node_general_access_grant (expires_at) where expires_at is not null;
create index node_general_access_grant_tenant_idx
    on node_general_access_grant (tenant_id);
