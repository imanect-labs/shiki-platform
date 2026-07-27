-- 共有リンク（複数発行・個別失効/延長・#342）。#338/#339 の一般アクセス（1 node 1 ポリシー・
-- migration 0051 の node_general_access）を作り替え、1 リソースに複数のリンクをぶら下げる台帳にする。
-- Google/MS 式の共有リンクに相当。
--
-- **なぜ 0051 を書き替えず 0052 を切るか（Codex/レビュー P1）**: 0051 は base ブランチで既に
-- version 51 として適用され得る（dev DB / 永続化 CI）。適用済みの版を別内容へ差し替えると
-- sqlx の checksum 検証で `VersionMismatch` になり起動時に落ちる。よって 0051（node_general_access）は
-- そのまま残し、この 0052 で「新テーブル作成 → 旧テーブルがあれば移行 → 旧 drop」を行う。
-- fresh DB では 0051 が旧テーブルを作り、この 0052 が即移行して落とす（無害・冪等）。
--
-- 認可の正本は OpenFGA タプル（file/folder の viewer/editor に organization#member）。ここはリンクの
-- **台帳**で、FGA の broad タプル集合は「active な全リンクの (subject,relation) 和集合」の射影として
-- reconcile される。password 付きリンクは broad タプルを張らず redeem 経由で per-user タプルを発行する。
-- audience='restricted'（既存アクセス者のみ）は付与ゼロの純ポインタ。owner のみが発行できる。
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
    -- 'organization'（社内＝現テナント/組織内） | 'anyone'（legacy・organization と同義に縮退） |
    -- 'restricted'（既存アクセス者のみ・付与ゼロ）。broad_subject: organization/anyone→organization#member、
    -- restricted→None＝付与ゼロの純ポインタ。user:* は将来の 'authenticated'（テナント跨ぎ閲覧・#341
    -- 系）専用に空けてある（#342 レビュー A-2）。
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

-- 旧一般アクセス（node_general_access・0051）があれば共有リンク台帳へ移行して drop する。
-- fresh DB には 0051 が直前に作った空テーブルが存在するので、この DO ブロックは常に走り得る
-- （空なら何も移行せず drop するだけ）。適用済み dev/CI では実データを 1 node 1 リンクへ変換する。
do $migrate$
begin
    if exists (
        select 1 from information_schema.tables
        where table_schema = 'public' and table_name = 'node_general_access'
    ) then
        -- 1 node 1 ポリシー → 1 リンク。link_id/token を採番（token は 64 hex＝uuid 2 本連結）。
        insert into node_share_link
            (link_id, node_id, tenant_id, org, kind, audience, role, token,
             expires_at, password_hash, revoked_at, label, created_by, updated_by, created_at, updated_at)
        select gen_random_uuid(), g.node_id, g.tenant_id, g.org, g.kind, g.level, g.role,
               replace(gen_random_uuid()::text, '-', '') || replace(gen_random_uuid()::text, '-', ''),
               g.expires_at, g.password_hash, null, null, g.created_by, g.updated_by, g.created_at, g.updated_at
        from node_general_access g;

        -- redeem 台帳: 旧は 1 node 1 policy なので node→link は一意に定まる。
        insert into node_share_link_grant
            (link_id, node_id, user_id, tenant_id, kind, role, expires_at, granted_at)
        select l.link_id, gg.node_id, gg.user_id, gg.tenant_id, gg.kind, gg.role, gg.expires_at, gg.granted_at
        from node_general_access_grant gg
        join node_share_link l on l.node_id = gg.node_id;

        drop table node_general_access_grant;
        drop table node_general_access;
    end if;
end
$migrate$;
