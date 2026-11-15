-- 職員間メッセージ（Phase 14・design §4.14・requirements FR-18）。
-- DM は 1 対 1 に限り、独立テーブルにせず kind = 'dm' のチャンネルとして持つ（3 人以上は非公開チャンネル）。
-- 利用者と自分のパーソナルエージェントの DM（承認の通知先・Task 14.17）も kind = 'dm' で、参加者は本人だけ。
-- 全テーブルは (tenant_id, org) を先頭に持ち、チャンネルは 1 つの org に閉じる（PIT-45）。
-- 閲覧・投稿の可否は OpenFGA の channel 型が決める。channel_member は表示・未読・参加前発言の境界
-- （visible_from）のための写しで、認可の正本ではない。

CREATE TABLE channel (
    tenant_id text NOT NULL,
    org text NOT NULL,
    id uuid NOT NULL,
    kind text NOT NULL CHECK (kind IN ('public', 'private', 'dm')),
    -- DM は名前を持たない（表示名は相手から組む）。
    name text CHECK (name IS NULL OR char_length(name) BETWEEN 1 AND 80),
    -- 名前の一意判定の鍵（大文字小文字を区別しない）。RLS の表では式インデックスが効かないため生成列に持つ。
    name_key text GENERATED ALWAYS AS (lower(name)) STORED,
    topic text NOT NULL DEFAULT '' CHECK (char_length(topic) <= 250),
    -- アナウンス専用（投稿者を個別に指定する）。DM では持たない。
    announcement boolean NOT NULL DEFAULT false,
    -- DM の 2 人（正規化済み・同じ 2 人の DM を 1 つに保つ冪等キー）。DM 以外は NULL。
    dm_key text,
    created_by text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    archived_at timestamptz,
    PRIMARY KEY (tenant_id, org, id),
    CHECK ((kind = 'dm') = (dm_key IS NOT NULL)),
    CHECK ((kind = 'dm') = (name IS NULL)),
    CHECK (kind <> 'dm' OR NOT announcement)
);
CREATE UNIQUE INDEX channel_dm_key ON channel (tenant_id, org, dm_key) WHERE kind = 'dm';
-- チャンネル名は org 内で一意（公開・非公開とも・大文字小文字を区別しない）。
CREATE UNIQUE INDEX channel_name ON channel (tenant_id, org, name_key) WHERE kind <> 'dm';
CREATE INDEX channel_public ON channel (tenant_id, org, created_at DESC) WHERE kind = 'public';

CREATE TABLE channel_member (
    tenant_id text NOT NULL,
    org text NOT NULL,
    channel_id uuid NOT NULL,
    user_id text NOT NULL,
    -- 参加（入り直し）した時刻。非公開と DM はこれより前の発言を読めない（公開は制約しない）。
    visible_from timestamptz NOT NULL DEFAULT now(),
    -- 通知の受け方（all / mentions / none）。
    notify text NOT NULL DEFAULT 'all' CHECK (notify IN ('all', 'mentions', 'none')),
    -- DM は退出できず非表示にするだけ。新しい発言が来たら表示に戻す。
    hidden_at timestamptz,
    PRIMARY KEY (tenant_id, org, channel_id, user_id),
    FOREIGN KEY (tenant_id, org, channel_id) REFERENCES channel (tenant_id, org, id) ON DELETE CASCADE
);
CREATE INDEX channel_member_user ON channel_member (tenant_id, org, user_id);

CREATE TABLE channel_message (
    tenant_id text NOT NULL,
    org text NOT NULL,
    id uuid NOT NULL,
    channel_id uuid NOT NULL,
    -- スレッド返信の親（同じチャンネルの発言に限る・入れ子は 1 段）。
    parent_id uuid,
    -- 人の発言は author_kind = 'user'。パーソナルエージェントの発言は 'assistant' で、
    -- author_id は持ち主（＝呼んだ人）。エージェントは独自の権限を持たない（新しい PrincipalKind を作らない）。
    author_kind text NOT NULL DEFAULT 'user' CHECK (author_kind IN ('user', 'assistant')),
    author_id text NOT NULL,
    -- content blocks（text / file_ref / mention）。file_ref は node_id だけを持ち本文を複製しない。
    blocks jsonb NOT NULL,
    -- 発言の全文検索の対象テキスト（発言専用の索引・RAG には入れない）。削除・期限切れで NULL にする。
    search_text text,
    -- リンクのプレビュー（取得できる構成でだけ埋まる）。
    link_previews jsonb NOT NULL DEFAULT '[]'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    -- 最後に変わった時刻（編集・削除・リアクション・プレビュー・返信数）。配信の差分取得のカーソル。
    updated_at timestamptz NOT NULL DEFAULT now(),
    edited_at timestamptz,
    -- 論理削除。削除後の本文は通常の読取経路に返さない（編集前・削除前の本文は channel_message_revision）。
    deleted_at timestamptz,
    deleted_by text,
    -- 保存期間で消した時刻。本文と改訂履歴は消え、行だけが「保存期間を過ぎました」として残る。
    expired_at timestamptz,
    PRIMARY KEY (tenant_id, org, id),
    UNIQUE (tenant_id, org, channel_id, id),
    FOREIGN KEY (tenant_id, org, channel_id) REFERENCES channel (tenant_id, org, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, org, channel_id, parent_id)
        REFERENCES channel_message (tenant_id, org, channel_id, id) ON DELETE CASCADE
);
CREATE INDEX channel_message_timeline ON channel_message (tenant_id, org, channel_id, created_at, id) WHERE parent_id IS NULL;
CREATE INDEX channel_message_thread ON channel_message (tenant_id, org, parent_id, created_at, id) WHERE parent_id IS NOT NULL;
CREATE INDEX channel_message_changes ON channel_message (tenant_id, org, channel_id, updated_at);
CREATE INDEX channel_message_expiry ON channel_message (tenant_id, org, created_at) WHERE expired_at IS NULL;

-- 編集前・削除前の本文（保存期間まで残し、管理者閲覧の手続でだけ読む・design §4.14 編集・削除）。
CREATE TABLE channel_message_revision (
    tenant_id text NOT NULL,
    org text NOT NULL,
    message_id uuid NOT NULL,
    rev integer NOT NULL,
    blocks jsonb NOT NULL,
    reason text NOT NULL CHECK (reason IN ('edit', 'delete')),
    recorded_by text NOT NULL,
    recorded_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, org, message_id, rev),
    FOREIGN KEY (tenant_id, org, message_id) REFERENCES channel_message (tenant_id, org, id) ON DELETE CASCADE
);

CREATE TABLE channel_message_reaction (
    tenant_id text NOT NULL,
    org text NOT NULL,
    message_id uuid NOT NULL,
    user_id text NOT NULL,
    emoji text NOT NULL CHECK (char_length(emoji) BETWEEN 1 AND 16),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, org, message_id, user_id, emoji),
    FOREIGN KEY (tenant_id, org, message_id) REFERENCES channel_message (tenant_id, org, id) ON DELETE CASCADE
);

-- 未読は last_read_at より新しい発言数から導出する（行ごとの既読フラグを持たない）。
CREATE TABLE channel_read_state (
    tenant_id text NOT NULL,
    org text NOT NULL,
    channel_id uuid NOT NULL,
    user_id text NOT NULL,
    last_read_at timestamptz NOT NULL,
    PRIMARY KEY (tenant_id, org, channel_id, user_id),
    FOREIGN KEY (tenant_id, org, channel_id) REFERENCES channel (tenant_id, org, id) ON DELETE CASCADE
);

-- メンション（個人・ロール・@channel を解決した宛先）。アプリ内通知の正本。
CREATE TABLE channel_mention (
    tenant_id text NOT NULL,
    org text NOT NULL,
    user_id text NOT NULL,
    message_id uuid NOT NULL,
    channel_id uuid NOT NULL,
    -- user / role / channel（どの書き方で宛先になったか）。
    via text NOT NULL CHECK (via IN ('user', 'role', 'channel')),
    created_at timestamptz NOT NULL DEFAULT now(),
    read_at timestamptz,
    PRIMARY KEY (tenant_id, org, user_id, message_id),
    FOREIGN KEY (tenant_id, org, message_id) REFERENCES channel_message (tenant_id, org, id) ON DELETE CASCADE
);
CREATE INDEX channel_mention_unread ON channel_mention (tenant_id, org, user_id, created_at DESC) WHERE read_at IS NULL;
-- 発言の削除・保存期間の削除で message_id から引く。
CREATE INDEX channel_mention_message ON channel_mention (tenant_id, org, message_id);

-- パーソナルエージェントが呼んだ人だけに見せた回答（「自分だけに表示」・design §4.14）。
-- 照会者が「チャンネルに投稿」したときだけ channel_message に写す。
CREATE TABLE channel_agent_reply (
    tenant_id text NOT NULL,
    org text NOT NULL,
    id uuid NOT NULL,
    channel_id uuid NOT NULL,
    -- 呼び出しを書いた発言（スレッドの親を決める）。
    trigger_message_id uuid NOT NULL,
    invoked_by text NOT NULL,
    -- public: チャンネルに返信した / private: 本人だけに表示中 / posted: 本人が投稿した。
    visibility text NOT NULL CHECK (visibility IN ('public', 'private', 'posted')),
    blocks jsonb NOT NULL,
    -- 回答が参照した資料（file の node_id）。投稿前の「読めない参加者がいる」警告に使う。
    sources jsonb NOT NULL DEFAULT '[]'::jsonb,
    -- 作業を移したチャット（§4.4 のスレッド・Task 14.17）。
    chat_thread_id uuid,
    chat_title text,
    -- 照会者へ DM で知らせた承認待ち（run_id:tool_call_id・同じ承認待ちを二度知らせない）。
    notified_approvals text[] NOT NULL DEFAULT '{}',
    posted_message_id uuid,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, org, id),
    FOREIGN KEY (tenant_id, org, channel_id) REFERENCES channel (tenant_id, org, id) ON DELETE CASCADE
);
CREATE INDEX channel_agent_reply_owner ON channel_agent_reply (tenant_id, org, invoked_by, channel_id, created_at DESC);
CREATE INDEX channel_agent_reply_chat ON channel_agent_reply (created_at) WHERE chat_thread_id IS NOT NULL;
CREATE INDEX channel_agent_reply_trigger ON channel_agent_reply (tenant_id, org, trigger_message_id);

-- パーソナルエージェントの名前とアイコン（見た目だけ・権限に影響しない）。
CREATE TABLE messaging_agent_profile (
    tenant_id text NOT NULL,
    org text NOT NULL,
    user_id text NOT NULL,
    name text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 32),
    -- アイコン（PNG / JPEG / WebP・プロフィール画像と同じ上限）。
    icon bytea,
    icon_mime text,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, org, user_id),
    CHECK ((icon IS NULL) = (icon_mime IS NULL))
);

-- org ごとの設定（公開チャンネルを作れる人・発言の保存期間・閲覧承認者）。
CREATE TABLE messaging_settings (
    tenant_id text NOT NULL,
    org text NOT NULL,
    -- everyone: 組織メンバー全員 / admins: 組織管理者だけ。
    public_channel_creators text NOT NULL DEFAULT 'everyone' CHECK (public_channel_creators IN ('everyone', 'admins')),
    -- NULL = 無期限（既定）。監査ログの保存期間より短くはできない（サービス層で検証）。
    retention_days integer CHECK (retention_days IS NULL OR retention_days > 0),
    -- DM・非公開チャンネルの閲覧を承認する人（テナントが指名・空なら閲覧できない）。
    access_approvers text[] NOT NULL DEFAULT '{}',
    rev bigint NOT NULL DEFAULT 0,
    updated_by text,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, org)
);

-- 管理者閲覧の承認レコード（DM・非公開チャンネル・design §4.14 eDiscovery・PIT-65）。
CREATE TABLE messaging_access_grant (
    tenant_id text NOT NULL,
    org text NOT NULL,
    id uuid NOT NULL,
    channel_id uuid NOT NULL,
    requester text NOT NULL,
    reason text NOT NULL CHECK (char_length(reason) BETWEEN 1 AND 2000),
    -- 閲覧の対象にする発言の期間。
    messages_from timestamptz NOT NULL,
    messages_until timestamptz NOT NULL,
    -- 閲覧できる日数（既定 7・最長 30）。承認の時刻から数える。
    valid_days integer NOT NULL CHECK (valid_days BETWEEN 1 AND 30),
    status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'approved', 'rejected', 'revoked')),
    approver text,
    decided_at timestamptz,
    valid_until timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, org, id),
    CHECK (messages_from < messages_until),
    CHECK (approver IS NULL OR approver <> requester),
    CHECK ((status = 'approved') = (valid_until IS NOT NULL) OR status = 'revoked'),
    FOREIGN KEY (tenant_id, org, channel_id) REFERENCES channel (tenant_id, org, id) ON DELETE CASCADE
);
CREATE INDEX messaging_access_grant_channel ON messaging_access_grant (tenant_id, org, channel_id, created_at DESC);

ALTER TABLE channel ENABLE ROW LEVEL SECURITY;
ALTER TABLE channel FORCE ROW LEVEL SECURITY;
CREATE POLICY shiki_tenant_isolation ON channel USING (tenant_id = (SELECT shiki_rls_tenant())) WITH CHECK (tenant_id = (SELECT shiki_rls_tenant()));
ALTER TABLE channel_member ENABLE ROW LEVEL SECURITY;
ALTER TABLE channel_member FORCE ROW LEVEL SECURITY;
CREATE POLICY shiki_tenant_isolation ON channel_member USING (tenant_id = (SELECT shiki_rls_tenant())) WITH CHECK (tenant_id = (SELECT shiki_rls_tenant()));
ALTER TABLE channel_message ENABLE ROW LEVEL SECURITY;
ALTER TABLE channel_message FORCE ROW LEVEL SECURITY;
CREATE POLICY shiki_tenant_isolation ON channel_message USING (tenant_id = (SELECT shiki_rls_tenant())) WITH CHECK (tenant_id = (SELECT shiki_rls_tenant()));
ALTER TABLE channel_message_revision ENABLE ROW LEVEL SECURITY;
ALTER TABLE channel_message_revision FORCE ROW LEVEL SECURITY;
CREATE POLICY shiki_tenant_isolation ON channel_message_revision USING (tenant_id = (SELECT shiki_rls_tenant())) WITH CHECK (tenant_id = (SELECT shiki_rls_tenant()));
ALTER TABLE channel_message_reaction ENABLE ROW LEVEL SECURITY;
ALTER TABLE channel_message_reaction FORCE ROW LEVEL SECURITY;
CREATE POLICY shiki_tenant_isolation ON channel_message_reaction USING (tenant_id = (SELECT shiki_rls_tenant())) WITH CHECK (tenant_id = (SELECT shiki_rls_tenant()));
ALTER TABLE channel_read_state ENABLE ROW LEVEL SECURITY;
ALTER TABLE channel_read_state FORCE ROW LEVEL SECURITY;
CREATE POLICY shiki_tenant_isolation ON channel_read_state USING (tenant_id = (SELECT shiki_rls_tenant())) WITH CHECK (tenant_id = (SELECT shiki_rls_tenant()));
ALTER TABLE channel_mention ENABLE ROW LEVEL SECURITY;
ALTER TABLE channel_mention FORCE ROW LEVEL SECURITY;
CREATE POLICY shiki_tenant_isolation ON channel_mention USING (tenant_id = (SELECT shiki_rls_tenant())) WITH CHECK (tenant_id = (SELECT shiki_rls_tenant()));
ALTER TABLE channel_agent_reply ENABLE ROW LEVEL SECURITY;
ALTER TABLE channel_agent_reply FORCE ROW LEVEL SECURITY;
CREATE POLICY shiki_tenant_isolation ON channel_agent_reply USING (tenant_id = (SELECT shiki_rls_tenant())) WITH CHECK (tenant_id = (SELECT shiki_rls_tenant()));
ALTER TABLE messaging_agent_profile ENABLE ROW LEVEL SECURITY;
ALTER TABLE messaging_agent_profile FORCE ROW LEVEL SECURITY;
CREATE POLICY shiki_tenant_isolation ON messaging_agent_profile USING (tenant_id = (SELECT shiki_rls_tenant())) WITH CHECK (tenant_id = (SELECT shiki_rls_tenant()));
ALTER TABLE messaging_settings ENABLE ROW LEVEL SECURITY;
ALTER TABLE messaging_settings FORCE ROW LEVEL SECURITY;
CREATE POLICY shiki_tenant_isolation ON messaging_settings USING (tenant_id = (SELECT shiki_rls_tenant())) WITH CHECK (tenant_id = (SELECT shiki_rls_tenant()));
ALTER TABLE messaging_access_grant ENABLE ROW LEVEL SECURITY;
ALTER TABLE messaging_access_grant FORCE ROW LEVEL SECURITY;
CREATE POLICY shiki_tenant_isolation ON messaging_access_grant USING (tenant_id = (SELECT shiki_rls_tenant())) WITH CHECK (tenant_id = (SELECT shiki_rls_tenant()));
