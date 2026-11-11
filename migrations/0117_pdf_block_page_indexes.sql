-- PDF の保存済み OCR・表ブロックをページ単位で取得する。
-- 通常の page と跨ページの prov の OR を BitmapOr で絞り、版の全件走査を避ける。
create index doc_block_pdf_page_idx
    on doc_block (tenant_id, org, node_id, version, page, ordinal)
    where page is not null;

create index doc_block_pdf_prov_pages_idx
    on doc_block using gin ((jsonb_path_query_array(prov, '$[*].page')) jsonb_path_ops);
