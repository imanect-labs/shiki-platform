-- #814: シークレット暗号文を行（tenant_id・org・secret_id）へ束縛する（AES-256-GCM の AAD）。
--
-- enc_version = 暗号化方式の版（crates/secrets/src/row_aad.rs が正本）。
--   0 = AAD なし（本 migration 以前の行）。読み出し時に後方互換で復号し、現行版へ遅延再暗号化する。
--   1 = 行識別子を AAD に束縛。新規書込・ローテーションは常にこれ。
--
-- **既存行は書き換えない**（列追加のみ・既定 0 = 既存の暗号文はそのまま有効）。旧版行の一括再暗号化は
-- `shiki-admin secrets-reencrypt` が行単位・条件付き UPDATE で行う（元の暗号文と同じ DEK を使う）。
-- 既定を 0 にするのは、migration 適用後に旧バイナリ（enc_version を書かない）が INSERT しても
-- 「AAD なし」として正しく記録されるため（INSERT に限った安全網）。
-- **旧バイナリと新バイナリを並走させないこと**: 旧バイナリの rotate は enc_version に触れないので、
-- 新バイナリが遅延移行した行（enc_version=1）を旧バイナリが書き直すと「AAD なしの暗号文で 1」の
-- 復号不能な行ができる。停止して入れ替える（docs/guides/tenant-ops.md）。
alter table secret add column enc_version smallint not null default 0;

-- 再暗号化の走査（旧版行のみ・keyset ページング）用。旧版行が無くなれば空の索引になる。
create index secret_legacy_enc_idx on secret (tenant_id, id) where enc_version = 0;
