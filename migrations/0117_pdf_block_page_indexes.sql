-- PDF の保存済み OCR・表ブロックをページ単位で取得する。
-- RLS 下では jsonb の式/GIN 索引を使えないため、ページ境界を保存列へ写して btree で絞る。
-- 範囲の途中に provenance のないページがあっても、本文クエリの残余条件で除外する。
set local lock_timeout = '5s';

create function public.shiki_doc_block_prov_min_page(provenance jsonb) returns integer
language sql immutable strict parallel safe
as $$ select min((entry ->> 'page')::integer) from pg_catalog.jsonb_array_elements(provenance) entry $$;

create function public.shiki_doc_block_prov_max_page(provenance jsonb) returns integer
language sql immutable strict parallel safe
as $$ select max((entry ->> 'page')::integer) from pg_catalog.jsonb_array_elements(provenance) entry $$;

alter table doc_block
    add column prov_page_min integer generated always as (public.shiki_doc_block_prov_min_page(prov)) stored,
    add column prov_page_max integer generated always as (public.shiki_doc_block_prov_max_page(prov)) stored;

create index doc_block_pdf_page_idx
    on doc_block (tenant_id, org, node_id, version, page, ordinal)
    where page is not null;

-- 同じ単一ページの provenance は page 索引で取得できる。跨ページ・page 未設定のみを補助索引に載せる。
create index doc_block_pdf_prov_pages_idx
    on doc_block (tenant_id, org, node_id, version, prov_page_min, prov_page_max, ordinal)
    where prov_page_min is not null
      and (page is null or prov_page_min <> page or prov_page_max <> page);
