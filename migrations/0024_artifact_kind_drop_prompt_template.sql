-- #152 追補: artifact.kind から prompt_template を撤去し skill に統合する。
--
-- CodeRabbit/Codex 指摘: 0014_artifact.sql は sqlx::migrate! で既に適用済みの可能性があり、直接編集すると
-- _sqlx_migrations のチェックサム不一致で既存環境の migrate run が失敗する。CHECK 制約の変更は
-- 新規 migration の ALTER TABLE で行う。
--
-- 万一 prompt_template で作成済みの行があれば skill へ寄せる（ArtifactKind::PromptTemplate は
-- 実利用コードが無くテストのみの参照だったため、実データは想定していない）。
update artifact set kind = 'skill' where kind = 'prompt_template';

alter table artifact drop constraint artifact_kind_check;
alter table artifact add constraint artifact_kind_check
    check (kind in ('workflow', 'ui_spec', 'mini_app', 'skill', 'script'));
