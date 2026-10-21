-- llm_usage にプロンプトキャッシュの内訳を足す（#581）。
--
-- prompt_tokens はキャッシュ分を含む入力の総数（OpenAI 互換の prompt_tokens と同じ定義。
-- Anthropic の input_tokens はキャッシュ分を含まないので、アダプタが足し合わせて揃える）。
-- 下の 2 列はその内数で、cost_usd_micros はカタログのキャッシュ単価で
--   (prompt - read - write) × 入力単価 ＋ read × 読み出し単価 ＋ write × 書き込み単価 ＋ completion × 出力単価
-- として刻む。
--
-- 既存行は 0（内訳不明・当時は全入力を入力単価で数えていた）。遡って直さない: 金額は記録時点の
-- 会計として確定しており、Anthropic の旧行は prompt_tokens にキャッシュ分を含んでいない。
-- 定数の既定値を持つ列の追加はメタデータの更新だけで済む（全行の書き換えは起きない）。
alter table llm_usage
    add column cache_read_tokens  bigint not null default 0,
    add column cache_write_tokens bigint not null default 0;
