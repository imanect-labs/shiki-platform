-- 共有リンク（複数発行・個別失効/延長・#342）。#338/#339 の一般アクセス（1 node 1 ポリシー）を
-- 作り替え、1 リソースに複数のリンクをぶら下げる台帳にする。Google/MS 式の共有リンクに相当。
--
-- 認可の正本は OpenFGA タプル（file/folder の viewer/editor に organization#member / user:*）。
-- ここはリンクの**台帳**で、FGA の broad タプル集合は「active な全リンクの (subject,relation) 和集合」の
-- 射影として reconcile される。password 付きリンクは broad タプルを張らず redeem 経由で per-user
-- タプルを発行する。audience='existing'（既存アクセス者のみ）は付与ゼロの純ポインタ。owner のみ発行できる。
--
-- 有効期限は OpenFGA にネイティブ TTL が無いため、① セッション開始時の遅延失効（reconcile で
-- 不要 broad タプルを先行剥奪）と ② イベント駆動タイマ（expires_at の瞬間に reconcile）で
-- 強制する。定期ポーリングはしない。
create table node_share_link (
    -- リンク識別子（アプリ生成の v4・サロゲート PK）。URL/redeem 起点の token とは別。
    link_id       uuid        not null,
    -- 対象ノード。node への FK は張らない（node 削除時は purge_tenant / 明示 revoke で回収。
    -- ソフトデリートとの整合を単純化）。
    node_id       uuid        not null,
    -- テナント/組織スコープ（SAAS.1。sweeper が AuthContext 無しで Namespace を再構成するため保持）。
    tenant_id     text        not null,
    org           text        not null,
    -- 'file' | 'folder'（タプル剥奪時に FgaObject を再構成するため保持・node JOIN 不要）。
    kind          text        not null,
    -- 'organization'（組織内） | 'anyone'（社内＝テナント全員） | 'restricted'（既存アクセス者のみ・付与ゼロ）。
    -- GeneralAccessLevel を再利用（broad_subject: organization→organization#member、anyone→user:*、
    -- restricted→None＝付与ゼロの純ポインタ）。
    audience      text        not null,
    -- 'viewer' | 'editor'。付与する権限。
    role          text        not null,
    -- URL / redeem 起点の不透明トークン（衝突検出のため unique）。
    token         text        not null,
    -- 有効期限（NULL = 無期限）。
    expires_at    timestamptz,
    -- パスワード（Argon2id PHC 文字列・NULL = パスワード無し）。設定時は broad タプルを書かず
    -- redeem 経由で per-user タプルを発行する。API には決して返さない（has_password のみ露出）。
    password_hash text,
    -- ソフト失効時刻（NULL = 有効）。履歴・監査のため hard-delete しない。
    -- active 述語 = revoked_at IS NULL AND (expires_at IS NULL OR expires_at > now())。
    revoked_at    timestamptz,
    -- 任意のリンク名（UX 用・NULL 可）。
    label         text,
    created_by    text        not null,
    updated_by    text        not null,
    created_at    timestamptz not null default now(),
    updated_at    timestamptz not null default now(),
    primary key (link_id)
);

-- token の一意性（redeem のトークン引き・衝突検出）。
create unique index node_share_link_token_idx
    on node_share_link (token);
-- node のリンク一覧・reconcile スキャン。
create index node_share_link_node_idx
    on node_share_link (node_id);
-- 期限切れ active リンクの走査（イベント駆動タイマ・遅延失効）。期限付き & 未失効のみ載る。
create index node_share_link_sweep_idx
    on node_share_link (expires_at) where expires_at is not null and revoked_at is null;
-- テナント撤去（purge_tenant）のスキャン。
create index node_share_link_tenant_idx
    on node_share_link (tenant_id);

-- パスワード redeem で発行した per-user タプルの台帳（#342）。
--
-- redeem で書く viewer/editor タプルは「明示共有」とバイト等価のため、失効処理が明示共有を
-- 誤って剥奪しないよう、redeem 由来の付与だけをここに記録する。複数リンクが同一 (node,user,role)
-- を redeem し得るので (link_id,user_id) 単位で持ち、剥奪時は同 (node,user,role) の active grant を
-- 参照カウントして、最後の 1 本まで残っていれば FGA タプルを消さない。
create table node_share_link_grant (
    link_id     uuid        not null,
    -- per-user 参照カウントは (node,user,role) 単位で集計するため node_id を保持。
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
    primary key (link_id, user_id)
);

create index node_share_link_grant_sweep_idx
    on node_share_link_grant (expires_at) where expires_at is not null;
-- per-user reconcile の参照カウント集計 (node,user,role)。
create index node_share_link_grant_node_user_idx
    on node_share_link_grant (node_id, user_id);
create index node_share_link_grant_tenant_idx
    on node_share_link_grant (tenant_id);
