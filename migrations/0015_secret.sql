-- Phase 10 Task 10.9: シークレット管理（crates/secrets）。
--
-- **write-only / use-only**: 平文を読み返す列も API も存在しない。保存されるのは
-- envelope encryption の暗号文（ciphertext）と、マスターキーで包んだデータ暗号鍵（encrypted_dek）
-- のみ。解決（利用）は実行時にエンジン側でのみ行い、監査に残す（miniapp-platform.md §5）。
--
-- 宛先束縛: allowed_hosts に登録時宣言ホストを持ち、http.request 実行時に
-- 「このシークレットはこの宛先にしか添付できない」をエンジンが fail-closed 強制する（PIT-36）。
--
-- 規約: 全行 tenant_id not null・PK は tenant 先頭の複合キー（#91）。
-- 権限の正本は OpenFGA の secret 型（owner / can_use）。

create table secret (
    tenant_id     text        not null,
    id            uuid        not null default gen_random_uuid(),
    org           text        not null,
    -- tenant 内一意の参照名（IR は参照名のみを持ち、値には触れない・PIT-36）。
    name          text        not null,
    -- 作成者（subject の local id）。OpenFGA の owner タプルが権限の正本。
    owner         text        not null,
    -- 宛先束縛: 添付を許可するホスト（完全一致 or "*.suffix"・部分一致は禁止・PIT-36）。
    allowed_hosts text[]      not null default '{}',
    -- envelope encryption。**平文は保存しない**。
    -- ciphertext = AES-256-GCM(DEK, plaintext)。nonce は GCM の 96bit nonce。
    ciphertext    bytea       not null,
    nonce         bytea       not null,
    -- マスターキーで包んだデータ暗号鍵（KeyProvider が wrap/unwrap する）。
    encrypted_dek bytea       not null,
    dek_nonce     bytea       not null,
    -- どの KeyProvider で包んだか（ローテーション・移行の判別。例: "local-key-file"）。
    key_provider  text        not null,
    -- ローテーション世代（ローテーションのたびに +1・監査/移行補助）。
    version       bigint      not null default 1,
    created_at    timestamptz not null default now(),
    updated_at    timestamptz not null default now(),
    primary key (tenant_id, id)
);

-- 名前解決（IR 検証 V4・実行時の参照名→secret 解決）。生存行のみ一意。
create unique index secret_name_idx on secret (tenant_id, name);

-- 自分の owner シークレット一覧（参照名のみ・平文は返さない）。
create index secret_owner_idx on secret (tenant_id, org, owner);
