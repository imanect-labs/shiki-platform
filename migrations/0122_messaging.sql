-- 職員間メッセージ（Phase 14・Task 14.1・design §4.14）。
-- DM は独立テーブルにせず kind = 'dm' のチャンネルとして持つ（グループ DM も同型）。
-- 全テーブルは (tenant_id, org) を先頭に持ち、チャンネルは 1 つの org に閉じる（PIT-45）。
-- 閲覧・投稿の可否は OpenFGA の channel 型が決める。ここの行は「誰が参加しているか」の
-- 表示・未読・参加前発言の境界（visible_from）のための写しで、認可の正本ではない。

CREATE TABLE channel (
    tenant_id text NOT NULL,
    org text NOT NULL,
    id uuid NOT NULL,
    kind text NOT NULL CHECK (kind IN ('public', 'private', 'dm')),
    -- DM は名前を持たない（表示名は参加者から組む）。
    name text,
    topic text NOT NULL DEFAULT '',
    -- アナウンス専用（投稿者を個別に指定する）。DM では持たない。
    announcement boolean NOT NULL DEFAULT false,
    -- DM の参加者集合（正規化済み・重複作成を防ぐ冪等キー）。DM 以外は NULL。
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
CREATE INDEX channel_public ON channel (tenant_id, org, created_at DESC) WHERE kind = 'public';

CREATE TABLE channel_member (
    tenant_id text NOT NULL,
    org text NOT NULL,
    channel_id uuid NOT NULL,
    user_id text NOT NULL,
    -- 参加（入り直し）した時刻。非公開と DM はこれより前の発言を読めない（公開は制約しない）。
    visible_from timestamptz NOT NULL DEFAULT now(),
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
    author_id text NOT NULL,
    -- content blocks（text / file_ref / mention）。file_ref は node_id だけを持ち本文を複製しない。
    blocks jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    edited_at timestamptz,
    -- 論理削除。削除後の本文は返さない。
    deleted_at timestamptz,
    PRIMARY KEY (tenant_id, org, id),
    UNIQUE (tenant_id, org, channel_id, id),
    FOREIGN KEY (tenant_id, org, channel_id) REFERENCES channel (tenant_id, org, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, org, channel_id, parent_id)
        REFERENCES channel_message (tenant_id, org, channel_id, id) ON DELETE CASCADE
);
CREATE INDEX channel_message_timeline ON channel_message (tenant_id, org, channel_id, created_at, id) WHERE parent_id IS NULL;
CREATE INDEX channel_message_thread ON channel_message (tenant_id, org, parent_id, created_at, id) WHERE parent_id IS NOT NULL;

CREATE TABLE channel_message_reaction (
    tenant_id text NOT NULL,
    org text NOT NULL,
    message_id uuid NOT NULL,
    user_id text NOT NULL,
    emoji text NOT NULL CHECK (char_length(emoji) BETWEEN 1 AND 32),
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

ALTER TABLE channel ENABLE ROW LEVEL SECURITY;
ALTER TABLE channel FORCE ROW LEVEL SECURITY;
CREATE POLICY shiki_tenant_isolation ON channel USING (tenant_id = (SELECT shiki_rls_tenant())) WITH CHECK (tenant_id = (SELECT shiki_rls_tenant()));
ALTER TABLE channel_member ENABLE ROW LEVEL SECURITY;
ALTER TABLE channel_member FORCE ROW LEVEL SECURITY;
CREATE POLICY shiki_tenant_isolation ON channel_member USING (tenant_id = (SELECT shiki_rls_tenant())) WITH CHECK (tenant_id = (SELECT shiki_rls_tenant()));
ALTER TABLE channel_message ENABLE ROW LEVEL SECURITY;
ALTER TABLE channel_message FORCE ROW LEVEL SECURITY;
CREATE POLICY shiki_tenant_isolation ON channel_message USING (tenant_id = (SELECT shiki_rls_tenant())) WITH CHECK (tenant_id = (SELECT shiki_rls_tenant()));
ALTER TABLE channel_message_reaction ENABLE ROW LEVEL SECURITY;
ALTER TABLE channel_message_reaction FORCE ROW LEVEL SECURITY;
CREATE POLICY shiki_tenant_isolation ON channel_message_reaction USING (tenant_id = (SELECT shiki_rls_tenant())) WITH CHECK (tenant_id = (SELECT shiki_rls_tenant()));
ALTER TABLE channel_read_state ENABLE ROW LEVEL SECURITY;
ALTER TABLE channel_read_state FORCE ROW LEVEL SECURITY;
CREATE POLICY shiki_tenant_isolation ON channel_read_state USING (tenant_id = (SELECT shiki_rls_tenant())) WITH CHECK (tenant_id = (SELECT shiki_rls_tenant()));
