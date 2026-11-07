-- ドライブの名前順を「自然順」にするための照合順序（#1040）。
--
-- ICU の日本語ロケールに数値比較（`kn`）を足したもの: 「資料2」<「資料10」、大文字小文字は
-- 一次比較で同列（a/A → b/B）、かな → 漢字の順。OneDrive / Finder の名前順に揃える。
-- 決定的照合（deterministic）なので、照合上等しい名前もバイト順で一意に並び keyset が壊れない。
--
-- 並べ替え（ORDER BY）と keyset 比較にだけ使い、索引・一意制約には使わない。ICU の collation
-- version が上がっても並び順が変わるだけで、索引破損（docs/data-platform.md §3.4 の懸念）は起きない。
create collation if not exists node_name_natural (provider = icu, locale = 'ja-u-kn');
