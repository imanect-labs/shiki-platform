-- Phase 6 Task 6.7/6.9/6.10: チャットへの skill / ミニアプリ適用（バージョンピン）。
--
-- thread: 作成時に選択した skill / mini_app を **version 込みで固定**する（再現性）。
-- generation_run: post 時点の thread ピンをコピーし、ワーカーが適用する
-- （0027 の autonomous と同パターン。run 行が生成材料の単一ソース）。
-- id と version は必ず対で持つ（片方だけの中途半端な行を CHECK で防ぐ）。

alter table thread
    add column skill_id        uuid,
    add column skill_version   bigint,
    add column mini_app_id     uuid,
    add column mini_app_version bigint;

alter table thread
    add constraint thread_skill_pin_chk
        check ((skill_id is null) = (skill_version is null)),
    add constraint thread_mini_app_pin_chk
        check ((mini_app_id is null) = (mini_app_version is null));

alter table generation_run
    add column skill_id        uuid,
    add column skill_version   bigint,
    add column mini_app_id     uuid,
    add column mini_app_version bigint;

alter table generation_run
    add constraint run_skill_pin_chk
        check ((skill_id is null) = (skill_version is null)),
    add constraint run_mini_app_pin_chk
        check ((mini_app_id is null) = (mini_app_version is null));
