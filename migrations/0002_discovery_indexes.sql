-- Query-driven B-tree paths for bounded keyset traversal. Both scoped and
-- all-scope paths are required because an all-scope order cannot use an
-- owner-leading index. The title indexes deliberately encode NULLS LAST in
-- each direction; a backward scan of an ASC NULLS LAST index puts NULL first.
CREATE INDEX links_created_at_id_idx
    ON links (created_at ASC, id ASC);
CREATE INDEX links_owner_created_at_id_idx
    ON links (owner_id, created_at ASC, id ASC);

-- Visit counts are updated by the Part 07 worker. These two indexes are a
-- deliberate write-amplification tradeoff for global and owner-scoped ordering.
CREATE INDEX links_visit_count_id_idx
    ON links (visit_count ASC, id ASC);
CREATE INDEX links_owner_visit_count_id_idx
    ON links (owner_id, visit_count ASC, id ASC);

CREATE INDEX links_title_search_id_asc_idx
    ON links (title_search COLLATE "C" ASC NULLS LAST, id ASC);
CREATE INDEX links_title_search_id_desc_idx
    ON links (title_search COLLATE "C" DESC NULLS LAST, id DESC);
CREATE INDEX links_owner_title_search_id_asc_idx
    ON links (owner_id, title_search COLLATE "C" ASC NULLS LAST, id ASC);
CREATE INDEX links_owner_title_search_id_desc_idx
    ON links (owner_id, title_search COLLATE "C" DESC NULLS LAST, id DESC);
