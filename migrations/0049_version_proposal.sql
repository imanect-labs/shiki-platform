-- AI Office 編集の提案バージョン（Task 11.8・PIT-44）。
--
-- WOPI 編集セッション中（office_lock 保持中）の AI 編集は、人間の未保存編集を
-- 上書きしないよう「提案バージョン」として保存する:
--   * node.version（current）を進めない（node 行は一切触らない）
--   * 書込イベント outbox を発火しない（＝RAG 再索引の対象外）
--   * バージョン履歴 UI で editor が「採用」して初めて通常の新バージョンになる
--
-- 版番号は node_version の PK (node_id, version) を提案とも共有する単調増加空間。
-- 提案が存在する間の通常版作成は GREATEST(node.version, MAX(node_version.version)) + 1
-- で採番して衝突を避ける（crates/storage の各書込経路が同じ式を使う）。
alter table node_version
    add column is_proposal boolean not null default false,
    add column proposed_by text;

-- 提案の一覧・採用判定用の部分 index（提案は常に少数・本体 index を汚さない）。
create index node_version_proposal_idx
    on node_version (node_id, version desc)
    where is_proposal;
