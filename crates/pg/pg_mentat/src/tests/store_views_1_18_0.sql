-- The views and helper functions pg_mentat 1.18.0's create_store('v118') made
-- (released v1.18.0 pg17 build; pg_get_viewdef / pg_get_functiondef). Test
-- fixture for the 1.18.0 -> 1.19.0 upgrade (named_store_tests.rs).
CREATE OR REPLACE VIEW mentat_v118.entities AS
 SELECT d.e AS entity_id,
    min(d.tx) AS first_tx,
    max(d.tx) AS last_tx,
    min(t.tx_instant) AS first_ts,
    max(t.tx_instant) AS last_ts,
    count(DISTINCT d.a) AS attribute_count
   FROM (( SELECT datoms_ref_new.e,
            datoms_ref_new.a,
            (0)::smallint AS value_type_tag,
            (datoms_ref_new.v)::text AS v_text,
            datoms_ref_new.tx
           FROM mentat_v118.datoms_ref_new
          WHERE ((datoms_ref_new.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (datoms_ref_new.added = true))
        UNION ALL
         SELECT datoms_boolean_new.e,
            datoms_boolean_new.a,
            (1)::smallint AS value_type_tag,
            (datoms_boolean_new.v)::text AS v_text,
            datoms_boolean_new.tx
           FROM mentat_v118.datoms_boolean_new
          WHERE ((datoms_boolean_new.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (datoms_boolean_new.added = true))
        UNION ALL
         SELECT datoms_long_new.e,
            datoms_long_new.a,
            (2)::smallint AS value_type_tag,
            (datoms_long_new.v)::text AS v_text,
            datoms_long_new.tx
           FROM mentat_v118.datoms_long_new
          WHERE ((datoms_long_new.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (datoms_long_new.added = true))
        UNION ALL
         SELECT datoms_double_new.e,
            datoms_double_new.a,
            (3)::smallint AS value_type_tag,
            (datoms_double_new.v)::text AS v_text,
            datoms_double_new.tx
           FROM mentat_v118.datoms_double_new
          WHERE ((datoms_double_new.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (datoms_double_new.added = true))
        UNION ALL
         SELECT datoms_instant_new.e,
            datoms_instant_new.a,
            (4)::smallint AS value_type_tag,
            (datoms_instant_new.v)::text AS v_text,
            datoms_instant_new.tx
           FROM mentat_v118.datoms_instant_new
          WHERE ((datoms_instant_new.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (datoms_instant_new.added = true))
        UNION ALL
         SELECT datoms_text_new.e,
            datoms_text_new.a,
            (7)::smallint AS value_type_tag,
            datoms_text_new.v AS v_text,
            datoms_text_new.tx
           FROM mentat_v118.datoms_text_new
          WHERE ((datoms_text_new.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (datoms_text_new.added = true))
        UNION ALL
         SELECT datoms_keyword_new.e,
            datoms_keyword_new.a,
            (8)::smallint AS value_type_tag,
            datoms_keyword_new.v AS v_text,
            datoms_keyword_new.tx
           FROM mentat_v118.datoms_keyword_new
          WHERE ((datoms_keyword_new.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (datoms_keyword_new.added = true))
        UNION ALL
         SELECT datoms_uuid_new.e,
            datoms_uuid_new.a,
            (10)::smallint AS value_type_tag,
            (datoms_uuid_new.v)::text AS v_text,
            datoms_uuid_new.tx
           FROM mentat_v118.datoms_uuid_new
          WHERE ((datoms_uuid_new.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (datoms_uuid_new.added = true))
        UNION ALL
         SELECT datoms_bytes_new.e,
            datoms_bytes_new.a,
            (11)::smallint AS value_type_tag,
            encode(datoms_bytes_new.v, 'hex'::text) AS v_text,
            datoms_bytes_new.tx
           FROM mentat_v118.datoms_bytes_new
          WHERE ((datoms_bytes_new.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (datoms_bytes_new.added = true))) d
     JOIN mentat_v118.transactions t ON ((t.tx = d.tx)))
  GROUP BY d.e
  ORDER BY d.e;
CREATE OR REPLACE VIEW mentat_v118.attributes AS
 SELECT entid,
    ident,
    (value_type)::text AS value_type,
    (cardinality)::text AS cardinality,
    (unique_constraint)::text AS unique_constraint,
    indexed,
    fulltext,
    component,
    no_history
   FROM mentat_v118.schema s
  ORDER BY entid;
CREATE OR REPLACE VIEW mentat_v118.facts AS
 SELECT d.e AS entity_id,
    COALESCE(s.ident, ('entid:'::text || (d.a)::text)) AS attribute,
    d.value,
    d.value_type,
    d.tx,
    t.tx_instant AS tx_time
   FROM ((( SELECT d_1.e,
            d_1.a,
            COALESCE(( SELECT ri.ident
                   FROM mentat_v118.idents ri
                  WHERE (ri.entid = d_1.v)), (d_1.v)::text) AS value,
            'ref'::text AS value_type,
            d_1.tx
           FROM mentat_v118.datoms_ref_new d_1
          WHERE ((d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (d_1.added = true))
        UNION ALL
         SELECT d_1.e,
            d_1.a,
            (d_1.v)::text AS value,
            'boolean'::text AS value_type,
            d_1.tx
           FROM mentat_v118.datoms_boolean_new d_1
          WHERE ((d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (d_1.added = true))
        UNION ALL
         SELECT d_1.e,
            d_1.a,
            (d_1.v)::text AS value,
            'long'::text AS value_type,
            d_1.tx
           FROM mentat_v118.datoms_long_new d_1
          WHERE ((d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (d_1.added = true))
        UNION ALL
         SELECT d_1.e,
            d_1.a,
            (d_1.v)::text AS value,
            'double'::text AS value_type,
            d_1.tx
           FROM mentat_v118.datoms_double_new d_1
          WHERE ((d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (d_1.added = true))
        UNION ALL
         SELECT d_1.e,
            d_1.a,
            (d_1.v)::text AS value,
            'instant'::text AS value_type,
            d_1.tx
           FROM mentat_v118.datoms_instant_new d_1
          WHERE ((d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (d_1.added = true))
        UNION ALL
         SELECT d_1.e,
            d_1.a,
            d_1.v AS value,
            'string'::text AS value_type,
            d_1.tx
           FROM mentat_v118.datoms_text_new d_1
          WHERE ((d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (d_1.added = true))
        UNION ALL
         SELECT d_1.e,
            d_1.a,
            (':'::text || d_1.v) AS value,
            'keyword'::text AS value_type,
            d_1.tx
           FROM mentat_v118.datoms_keyword_new d_1
          WHERE ((d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (d_1.added = true))
        UNION ALL
         SELECT d_1.e,
            d_1.a,
            (d_1.v)::text AS value,
            'uuid'::text AS value_type,
            d_1.tx
           FROM mentat_v118.datoms_uuid_new d_1
          WHERE ((d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (d_1.added = true))
        UNION ALL
         SELECT d_1.e,
            d_1.a,
            encode(d_1.v, 'hex'::text) AS value,
            'bytes'::text AS value_type,
            d_1.tx
           FROM mentat_v118.datoms_bytes_new d_1
          WHERE ((d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (d_1.added = true))) d
     LEFT JOIN mentat_v118.schema s ON ((s.entid = d.a)))
     LEFT JOIN mentat_v118.transactions t ON ((t.tx = d.tx)))
  ORDER BY d.e, d.a;
CREATE OR REPLACE VIEW mentat_v118.text_values AS
 SELECT d.e AS entity_id,
    COALESCE(s.ident, ('entid:'::text || (d.a)::text)) AS attribute,
    d.v AS value,
    d.tx
   FROM (mentat_v118.datoms_text_new d
     LEFT JOIN mentat_v118.schema s ON ((s.entid = d.a)))
  WHERE ((d.store_id = ( SELECT stores.store_id
           FROM mentat.stores
          WHERE (stores.store_name = 'v118'::text))) AND (d.added = true))
  ORDER BY d.e, d.a;
CREATE OR REPLACE VIEW mentat_v118.numeric_values AS
 SELECT d.e AS entity_id,
    COALESCE(s.ident, ('entid:'::text || (d.a)::text)) AS attribute,
    d.v AS value,
    d.tx
   FROM (mentat_v118.datoms_long_new d
     LEFT JOIN mentat_v118.schema s ON ((s.entid = d.a)))
  WHERE ((d.store_id = ( SELECT stores.store_id
           FROM mentat.stores
          WHERE (stores.store_name = 'v118'::text))) AND (d.added = true))
  ORDER BY d.e, d.a;
CREATE OR REPLACE VIEW mentat_v118.double_values AS
 SELECT d.e AS entity_id,
    COALESCE(s.ident, ('entid:'::text || (d.a)::text)) AS attribute,
    d.v AS value,
    d.tx
   FROM (mentat_v118.datoms_double_new d
     LEFT JOIN mentat_v118.schema s ON ((s.entid = d.a)))
  WHERE ((d.store_id = ( SELECT stores.store_id
           FROM mentat.stores
          WHERE (stores.store_name = 'v118'::text))) AND (d.added = true))
  ORDER BY d.e, d.a;
CREATE OR REPLACE VIEW mentat_v118.boolean_values AS
 SELECT d.e AS entity_id,
    COALESCE(s.ident, ('entid:'::text || (d.a)::text)) AS attribute,
    d.v AS value,
    d.tx
   FROM (mentat_v118.datoms_boolean_new d
     LEFT JOIN mentat_v118.schema s ON ((s.entid = d.a)))
  WHERE ((d.store_id = ( SELECT stores.store_id
           FROM mentat.stores
          WHERE (stores.store_name = 'v118'::text))) AND (d.added = true))
  ORDER BY d.e, d.a;
CREATE OR REPLACE VIEW mentat_v118."references" AS
 SELECT d.e AS entity_id,
    COALESCE(s.ident, ('entid:'::text || (d.a)::text)) AS attribute,
    d.v AS value,
    d.tx
   FROM (mentat_v118.datoms_ref_new d
     LEFT JOIN mentat_v118.schema s ON ((s.entid = d.a)))
  WHERE ((d.store_id = ( SELECT stores.store_id
           FROM mentat.stores
          WHERE (stores.store_name = 'v118'::text))) AND (d.added = true))
  ORDER BY d.e, d.a;
CREATE OR REPLACE VIEW mentat_v118.instant_values AS
 SELECT d.e AS entity_id,
    COALESCE(s.ident, ('entid:'::text || (d.a)::text)) AS attribute,
    d.v AS value,
    d.tx
   FROM (mentat_v118.datoms_instant_new d
     LEFT JOIN mentat_v118.schema s ON ((s.entid = d.a)))
  WHERE ((d.store_id = ( SELECT stores.store_id
           FROM mentat.stores
          WHERE (stores.store_name = 'v118'::text))) AND (d.added = true))
  ORDER BY d.e, d.a;
CREATE OR REPLACE VIEW mentat_v118.uuid_values AS
 SELECT d.e AS entity_id,
    COALESCE(s.ident, ('entid:'::text || (d.a)::text)) AS attribute,
    d.v AS value,
    d.tx
   FROM (mentat_v118.datoms_uuid_new d
     LEFT JOIN mentat_v118.schema s ON ((s.entid = d.a)))
  WHERE ((d.store_id = ( SELECT stores.store_id
           FROM mentat.stores
          WHERE (stores.store_name = 'v118'::text))) AND (d.added = true))
  ORDER BY d.e, d.a;
CREATE OR REPLACE VIEW mentat_v118.keyword_values AS
 SELECT d.e AS entity_id,
    COALESCE(s.ident, ('entid:'::text || (d.a)::text)) AS attribute,
    d.v AS value,
    d.tx
   FROM (mentat_v118.datoms_keyword_new d
     LEFT JOIN mentat_v118.schema s ON ((s.entid = d.a)))
  WHERE ((d.store_id = ( SELECT stores.store_id
           FROM mentat.stores
          WHERE (stores.store_name = 'v118'::text))) AND (d.added = true))
  ORDER BY d.e, d.a;
CREATE OR REPLACE VIEW mentat_v118.bytes_values AS
 SELECT d.e AS entity_id,
    COALESCE(s.ident, ('entid:'::text || (d.a)::text)) AS attribute,
    d.v AS value,
    d.tx
   FROM (mentat_v118.datoms_bytes_new d
     LEFT JOIN mentat_v118.schema s ON ((s.entid = d.a)))
  WHERE ((d.store_id = ( SELECT stores.store_id
           FROM mentat.stores
          WHERE (stores.store_name = 'v118'::text))) AND (d.added = true))
  ORDER BY d.e, d.a;
CREATE OR REPLACE VIEW mentat_v118.searchable_text AS
 SELECT d.e AS entity_id,
    COALESCE(s.ident, ('entid:'::text || (d.a)::text)) AS attribute,
    d.v AS value,
    to_tsvector('english'::regconfig, d.v) AS search_vector,
    d.tx
   FROM (mentat_v118.datoms_text_new d
     LEFT JOIN mentat_v118.schema s ON ((s.entid = d.a)))
  WHERE ((d.store_id = ( SELECT stores.store_id
           FROM mentat.stores
          WHERE (stores.store_name = 'v118'::text))) AND (d.added = true) AND (d.v IS NOT NULL))
  ORDER BY d.e, d.a;
CREATE OR REPLACE VIEW mentat_v118.entity_references AS
 SELECT d.e AS source_entity,
    COALESCE(s.ident, ('entid:'::text || (d.a)::text)) AS attribute,
    d.v AS target_entity,
    i.ident AS target_ident,
    d.tx
   FROM ((mentat_v118.datoms_ref_new d
     LEFT JOIN mentat_v118.schema s ON ((s.entid = d.a)))
     LEFT JOIN mentat_v118.idents i ON ((i.entid = d.v)))
  WHERE ((d.store_id = ( SELECT stores.store_id
           FROM mentat.stores
          WHERE (stores.store_name = 'v118'::text))) AND (d.added = true))
  ORDER BY d.e, d.a;
CREATE OR REPLACE VIEW mentat_v118.reverse_references AS
 SELECT d.v AS target_entity,
    COALESCE(s.ident, ('entid:'::text || (d.a)::text)) AS attribute,
    d.e AS source_entity,
    d.tx
   FROM (mentat_v118.datoms_ref_new d
     LEFT JOIN mentat_v118.schema s ON ((s.entid = d.a)))
  WHERE ((d.store_id = ( SELECT stores.store_id
           FROM mentat.stores
          WHERE (stores.store_name = 'v118'::text))) AND (d.added = true))
  ORDER BY d.v, d.a;
CREATE OR REPLACE VIEW mentat_v118.graph_edges AS
 SELECT d.e AS source,
    COALESCE(s.ident, ('entid:'::text || (d.a)::text)) AS edge_type,
    d.v AS target,
    d.tx
   FROM (mentat_v118.datoms_ref_new d
     LEFT JOIN mentat_v118.schema s ON ((s.entid = d.a)))
  WHERE ((d.store_id = ( SELECT stores.store_id
           FROM mentat.stores
          WHERE (stores.store_name = 'v118'::text))) AND (d.added = true));
CREATE OR REPLACE VIEW mentat_v118.tx_log AS
 SELECT t.tx,
    t.tx_instant AS tx_time,
    COALESCE(d.datom_count, (0)::bigint) AS datom_count
   FROM (mentat_v118.transactions t
     LEFT JOIN ( SELECT sub.tx,
            count(*) AS datom_count
           FROM ( SELECT datoms_ref_new.e,
                    datoms_ref_new.a,
                    (0)::smallint AS value_type_tag,
                    (datoms_ref_new.v)::text AS v_text,
                    datoms_ref_new.tx
                   FROM mentat_v118.datoms_ref_new
                  WHERE ((datoms_ref_new.store_id = ( SELECT stores.store_id
                           FROM mentat.stores
                          WHERE (stores.store_name = 'v118'::text))) AND (datoms_ref_new.added = true))
                UNION ALL
                 SELECT datoms_boolean_new.e,
                    datoms_boolean_new.a,
                    (1)::smallint AS value_type_tag,
                    (datoms_boolean_new.v)::text AS v_text,
                    datoms_boolean_new.tx
                   FROM mentat_v118.datoms_boolean_new
                  WHERE ((datoms_boolean_new.store_id = ( SELECT stores.store_id
                           FROM mentat.stores
                          WHERE (stores.store_name = 'v118'::text))) AND (datoms_boolean_new.added = true))
                UNION ALL
                 SELECT datoms_long_new.e,
                    datoms_long_new.a,
                    (2)::smallint AS value_type_tag,
                    (datoms_long_new.v)::text AS v_text,
                    datoms_long_new.tx
                   FROM mentat_v118.datoms_long_new
                  WHERE ((datoms_long_new.store_id = ( SELECT stores.store_id
                           FROM mentat.stores
                          WHERE (stores.store_name = 'v118'::text))) AND (datoms_long_new.added = true))
                UNION ALL
                 SELECT datoms_double_new.e,
                    datoms_double_new.a,
                    (3)::smallint AS value_type_tag,
                    (datoms_double_new.v)::text AS v_text,
                    datoms_double_new.tx
                   FROM mentat_v118.datoms_double_new
                  WHERE ((datoms_double_new.store_id = ( SELECT stores.store_id
                           FROM mentat.stores
                          WHERE (stores.store_name = 'v118'::text))) AND (datoms_double_new.added = true))
                UNION ALL
                 SELECT datoms_instant_new.e,
                    datoms_instant_new.a,
                    (4)::smallint AS value_type_tag,
                    (datoms_instant_new.v)::text AS v_text,
                    datoms_instant_new.tx
                   FROM mentat_v118.datoms_instant_new
                  WHERE ((datoms_instant_new.store_id = ( SELECT stores.store_id
                           FROM mentat.stores
                          WHERE (stores.store_name = 'v118'::text))) AND (datoms_instant_new.added = true))
                UNION ALL
                 SELECT datoms_text_new.e,
                    datoms_text_new.a,
                    (7)::smallint AS value_type_tag,
                    datoms_text_new.v AS v_text,
                    datoms_text_new.tx
                   FROM mentat_v118.datoms_text_new
                  WHERE ((datoms_text_new.store_id = ( SELECT stores.store_id
                           FROM mentat.stores
                          WHERE (stores.store_name = 'v118'::text))) AND (datoms_text_new.added = true))
                UNION ALL
                 SELECT datoms_keyword_new.e,
                    datoms_keyword_new.a,
                    (8)::smallint AS value_type_tag,
                    datoms_keyword_new.v AS v_text,
                    datoms_keyword_new.tx
                   FROM mentat_v118.datoms_keyword_new
                  WHERE ((datoms_keyword_new.store_id = ( SELECT stores.store_id
                           FROM mentat.stores
                          WHERE (stores.store_name = 'v118'::text))) AND (datoms_keyword_new.added = true))
                UNION ALL
                 SELECT datoms_uuid_new.e,
                    datoms_uuid_new.a,
                    (10)::smallint AS value_type_tag,
                    (datoms_uuid_new.v)::text AS v_text,
                    datoms_uuid_new.tx
                   FROM mentat_v118.datoms_uuid_new
                  WHERE ((datoms_uuid_new.store_id = ( SELECT stores.store_id
                           FROM mentat.stores
                          WHERE (stores.store_name = 'v118'::text))) AND (datoms_uuid_new.added = true))
                UNION ALL
                 SELECT datoms_bytes_new.e,
                    datoms_bytes_new.a,
                    (11)::smallint AS value_type_tag,
                    encode(datoms_bytes_new.v, 'hex'::text) AS v_text,
                    datoms_bytes_new.tx
                   FROM mentat_v118.datoms_bytes_new
                  WHERE ((datoms_bytes_new.store_id = ( SELECT stores.store_id
                           FROM mentat.stores
                          WHERE (stores.store_name = 'v118'::text))) AND (datoms_bytes_new.added = true))) sub
          GROUP BY sub.tx) d ON ((d.tx = t.tx)))
  ORDER BY t.tx DESC;
CREATE OR REPLACE VIEW mentat_v118.entity_history AS
 SELECT d.e AS entity_id,
    COALESCE(s.ident, ('entid:'::text || (d.a)::text)) AS attribute,
    d.value,
    d.value_type,
    d.tx,
    t.tx_instant AS tx_time,
        CASE
            WHEN d.added THEN 'assert'::text
            ELSE 'retract'::text
        END AS operation
   FROM ((( SELECT d_1.e,
            d_1.a,
            COALESCE(( SELECT ri.ident
                   FROM mentat_v118.idents ri
                  WHERE (ri.entid = d_1.v)), (d_1.v)::text) AS value,
            'ref'::text AS value_type,
            d_1.tx,
            d_1.added
           FROM mentat_v118.datoms_ref_new d_1
          WHERE (d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text)))
        UNION ALL
         SELECT d_1.e,
            d_1.a,
            (d_1.v)::text AS value,
            'boolean'::text AS value_type,
            d_1.tx,
            d_1.added
           FROM mentat_v118.datoms_boolean_new d_1
          WHERE (d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text)))
        UNION ALL
         SELECT d_1.e,
            d_1.a,
            (d_1.v)::text AS value,
            'long'::text AS value_type,
            d_1.tx,
            d_1.added
           FROM mentat_v118.datoms_long_new d_1
          WHERE (d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text)))
        UNION ALL
         SELECT d_1.e,
            d_1.a,
            (d_1.v)::text AS value,
            'double'::text AS value_type,
            d_1.tx,
            d_1.added
           FROM mentat_v118.datoms_double_new d_1
          WHERE (d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text)))
        UNION ALL
         SELECT d_1.e,
            d_1.a,
            (d_1.v)::text AS value,
            'instant'::text AS value_type,
            d_1.tx,
            d_1.added
           FROM mentat_v118.datoms_instant_new d_1
          WHERE (d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text)))
        UNION ALL
         SELECT d_1.e,
            d_1.a,
            d_1.v AS value,
            'string'::text AS value_type,
            d_1.tx,
            d_1.added
           FROM mentat_v118.datoms_text_new d_1
          WHERE (d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text)))
        UNION ALL
         SELECT d_1.e,
            d_1.a,
            (':'::text || d_1.v) AS value,
            'keyword'::text AS value_type,
            d_1.tx,
            d_1.added
           FROM mentat_v118.datoms_keyword_new d_1
          WHERE (d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text)))
        UNION ALL
         SELECT d_1.e,
            d_1.a,
            (d_1.v)::text AS value,
            'uuid'::text AS value_type,
            d_1.tx,
            d_1.added
           FROM mentat_v118.datoms_uuid_new d_1
          WHERE (d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text)))
        UNION ALL
         SELECT d_1.e,
            d_1.a,
            encode(d_1.v, 'hex'::text) AS value,
            'bytes'::text AS value_type,
            d_1.tx,
            d_1.added
           FROM mentat_v118.datoms_bytes_new d_1
          WHERE (d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text)))) d
     LEFT JOIN mentat_v118.schema s ON ((s.entid = d.a)))
     LEFT JOIN mentat_v118.transactions t ON ((t.tx = d.tx)))
  ORDER BY d.tx DESC, d.e, d.a;
CREATE OR REPLACE VIEW mentat_v118.recent_changes AS
 SELECT d.e AS entity_id,
    COALESCE(s.ident, ('entid:'::text || (d.a)::text)) AS attribute,
    d.value,
    d.value_type,
    d.tx,
    t.tx_instant AS tx_time
   FROM ((( SELECT d_1.e,
            d_1.a,
            COALESCE(( SELECT ri.ident
                   FROM mentat_v118.idents ri
                  WHERE (ri.entid = d_1.v)), (d_1.v)::text) AS value,
            'ref'::text AS value_type,
            d_1.tx
           FROM mentat_v118.datoms_ref_new d_1
          WHERE ((d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (d_1.added = true) AND (d_1.tx >= ( SELECT COALESCE((max(transactions.tx) - 100), (0)::bigint) AS "coalesce"
                   FROM mentat_v118.transactions)))
        UNION ALL
         SELECT d_1.e,
            d_1.a,
            (d_1.v)::text AS value,
            'boolean'::text AS value_type,
            d_1.tx
           FROM mentat_v118.datoms_boolean_new d_1
          WHERE ((d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (d_1.added = true) AND (d_1.tx >= ( SELECT COALESCE((max(transactions.tx) - 100), (0)::bigint) AS "coalesce"
                   FROM mentat_v118.transactions)))
        UNION ALL
         SELECT d_1.e,
            d_1.a,
            (d_1.v)::text AS value,
            'long'::text AS value_type,
            d_1.tx
           FROM mentat_v118.datoms_long_new d_1
          WHERE ((d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (d_1.added = true) AND (d_1.tx >= ( SELECT COALESCE((max(transactions.tx) - 100), (0)::bigint) AS "coalesce"
                   FROM mentat_v118.transactions)))
        UNION ALL
         SELECT d_1.e,
            d_1.a,
            (d_1.v)::text AS value,
            'double'::text AS value_type,
            d_1.tx
           FROM mentat_v118.datoms_double_new d_1
          WHERE ((d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (d_1.added = true) AND (d_1.tx >= ( SELECT COALESCE((max(transactions.tx) - 100), (0)::bigint) AS "coalesce"
                   FROM mentat_v118.transactions)))
        UNION ALL
         SELECT d_1.e,
            d_1.a,
            (d_1.v)::text AS value,
            'instant'::text AS value_type,
            d_1.tx
           FROM mentat_v118.datoms_instant_new d_1
          WHERE ((d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (d_1.added = true) AND (d_1.tx >= ( SELECT COALESCE((max(transactions.tx) - 100), (0)::bigint) AS "coalesce"
                   FROM mentat_v118.transactions)))
        UNION ALL
         SELECT d_1.e,
            d_1.a,
            d_1.v AS value,
            'string'::text AS value_type,
            d_1.tx
           FROM mentat_v118.datoms_text_new d_1
          WHERE ((d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (d_1.added = true) AND (d_1.tx >= ( SELECT COALESCE((max(transactions.tx) - 100), (0)::bigint) AS "coalesce"
                   FROM mentat_v118.transactions)))
        UNION ALL
         SELECT d_1.e,
            d_1.a,
            (':'::text || d_1.v) AS value,
            'keyword'::text AS value_type,
            d_1.tx
           FROM mentat_v118.datoms_keyword_new d_1
          WHERE ((d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (d_1.added = true) AND (d_1.tx >= ( SELECT COALESCE((max(transactions.tx) - 100), (0)::bigint) AS "coalesce"
                   FROM mentat_v118.transactions)))
        UNION ALL
         SELECT d_1.e,
            d_1.a,
            (d_1.v)::text AS value,
            'uuid'::text AS value_type,
            d_1.tx
           FROM mentat_v118.datoms_uuid_new d_1
          WHERE ((d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (d_1.added = true) AND (d_1.tx >= ( SELECT COALESCE((max(transactions.tx) - 100), (0)::bigint) AS "coalesce"
                   FROM mentat_v118.transactions)))
        UNION ALL
         SELECT d_1.e,
            d_1.a,
            encode(d_1.v, 'hex'::text) AS value,
            'bytes'::text AS value_type,
            d_1.tx
           FROM mentat_v118.datoms_bytes_new d_1
          WHERE ((d_1.store_id = ( SELECT stores.store_id
                   FROM mentat.stores
                  WHERE (stores.store_name = 'v118'::text))) AND (d_1.added = true) AND (d_1.tx >= ( SELECT COALESCE((max(transactions.tx) - 100), (0)::bigint) AS "coalesce"
                   FROM mentat_v118.transactions)))) d
     LEFT JOIN mentat_v118.schema s ON ((s.entid = d.a)))
     LEFT JOIN mentat_v118.transactions t ON ((t.tx = d.tx)))
  ORDER BY d.tx DESC, d.e, d.a;
CREATE OR REPLACE VIEW mentat_v118.schema_summary AS
 SELECT s.ident,
    (s.value_type)::text AS value_type,
    (s.cardinality)::text AS cardinality,
    (s.unique_constraint)::text AS unique_constraint,
    s.indexed,
    s.fulltext,
    COALESCE(u.entity_count, (0)::bigint) AS entity_count,
    COALESCE(u.datom_count, (0)::bigint) AS datom_count
   FROM (mentat_v118.schema s
     LEFT JOIN ( SELECT sub.a,
            count(DISTINCT sub.e) AS entity_count,
            count(*) AS datom_count
           FROM ( SELECT datoms_ref_new.e,
                    datoms_ref_new.a,
                    (0)::smallint AS value_type_tag,
                    (datoms_ref_new.v)::text AS v_text,
                    datoms_ref_new.tx
                   FROM mentat_v118.datoms_ref_new
                  WHERE ((datoms_ref_new.store_id = ( SELECT stores.store_id
                           FROM mentat.stores
                          WHERE (stores.store_name = 'v118'::text))) AND (datoms_ref_new.added = true))
                UNION ALL
                 SELECT datoms_boolean_new.e,
                    datoms_boolean_new.a,
                    (1)::smallint AS value_type_tag,
                    (datoms_boolean_new.v)::text AS v_text,
                    datoms_boolean_new.tx
                   FROM mentat_v118.datoms_boolean_new
                  WHERE ((datoms_boolean_new.store_id = ( SELECT stores.store_id
                           FROM mentat.stores
                          WHERE (stores.store_name = 'v118'::text))) AND (datoms_boolean_new.added = true))
                UNION ALL
                 SELECT datoms_long_new.e,
                    datoms_long_new.a,
                    (2)::smallint AS value_type_tag,
                    (datoms_long_new.v)::text AS v_text,
                    datoms_long_new.tx
                   FROM mentat_v118.datoms_long_new
                  WHERE ((datoms_long_new.store_id = ( SELECT stores.store_id
                           FROM mentat.stores
                          WHERE (stores.store_name = 'v118'::text))) AND (datoms_long_new.added = true))
                UNION ALL
                 SELECT datoms_double_new.e,
                    datoms_double_new.a,
                    (3)::smallint AS value_type_tag,
                    (datoms_double_new.v)::text AS v_text,
                    datoms_double_new.tx
                   FROM mentat_v118.datoms_double_new
                  WHERE ((datoms_double_new.store_id = ( SELECT stores.store_id
                           FROM mentat.stores
                          WHERE (stores.store_name = 'v118'::text))) AND (datoms_double_new.added = true))
                UNION ALL
                 SELECT datoms_instant_new.e,
                    datoms_instant_new.a,
                    (4)::smallint AS value_type_tag,
                    (datoms_instant_new.v)::text AS v_text,
                    datoms_instant_new.tx
                   FROM mentat_v118.datoms_instant_new
                  WHERE ((datoms_instant_new.store_id = ( SELECT stores.store_id
                           FROM mentat.stores
                          WHERE (stores.store_name = 'v118'::text))) AND (datoms_instant_new.added = true))
                UNION ALL
                 SELECT datoms_text_new.e,
                    datoms_text_new.a,
                    (7)::smallint AS value_type_tag,
                    datoms_text_new.v AS v_text,
                    datoms_text_new.tx
                   FROM mentat_v118.datoms_text_new
                  WHERE ((datoms_text_new.store_id = ( SELECT stores.store_id
                           FROM mentat.stores
                          WHERE (stores.store_name = 'v118'::text))) AND (datoms_text_new.added = true))
                UNION ALL
                 SELECT datoms_keyword_new.e,
                    datoms_keyword_new.a,
                    (8)::smallint AS value_type_tag,
                    datoms_keyword_new.v AS v_text,
                    datoms_keyword_new.tx
                   FROM mentat_v118.datoms_keyword_new
                  WHERE ((datoms_keyword_new.store_id = ( SELECT stores.store_id
                           FROM mentat.stores
                          WHERE (stores.store_name = 'v118'::text))) AND (datoms_keyword_new.added = true))
                UNION ALL
                 SELECT datoms_uuid_new.e,
                    datoms_uuid_new.a,
                    (10)::smallint AS value_type_tag,
                    (datoms_uuid_new.v)::text AS v_text,
                    datoms_uuid_new.tx
                   FROM mentat_v118.datoms_uuid_new
                  WHERE ((datoms_uuid_new.store_id = ( SELECT stores.store_id
                           FROM mentat.stores
                          WHERE (stores.store_name = 'v118'::text))) AND (datoms_uuid_new.added = true))
                UNION ALL
                 SELECT datoms_bytes_new.e,
                    datoms_bytes_new.a,
                    (11)::smallint AS value_type_tag,
                    encode(datoms_bytes_new.v, 'hex'::text) AS v_text,
                    datoms_bytes_new.tx
                   FROM mentat_v118.datoms_bytes_new
                  WHERE ((datoms_bytes_new.store_id = ( SELECT stores.store_id
                           FROM mentat.stores
                          WHERE (stores.store_name = 'v118'::text))) AND (datoms_bytes_new.added = true))) sub
          GROUP BY sub.a) u ON ((u.a = s.entid)))
  WHERE (s.entid >= 100)
  ORDER BY s.ident;
CREATE OR REPLACE FUNCTION mentat_v118.entities_with_attribute(attr_ident text)
 RETURNS TABLE(entity_id bigint, tx bigint)
 LANGUAGE plpgsql
 STABLE
AS $function$
DECLARE
    attr_entid BIGINT;
BEGIN
    SELECT entid INTO attr_entid FROM mentat_v118.schema WHERE ident = attr_ident;
    IF attr_entid IS NULL THEN
        RAISE EXCEPTION 'Unknown attribute ident: %', attr_ident;
    END IF;

    RETURN QUERY
    SELECT DISTINCT sub.e, sub.tx FROM (
        SELECT DISTINCT d.e, d.tx FROM mentat_v118.datoms_ref_new d WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.a = attr_entid AND d.added = true
        UNION ALL
        SELECT DISTINCT d.e, d.tx FROM mentat_v118.datoms_boolean_new d WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.a = attr_entid AND d.added = true
        UNION ALL
        SELECT DISTINCT d.e, d.tx FROM mentat_v118.datoms_long_new d WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.a = attr_entid AND d.added = true
        UNION ALL
        SELECT DISTINCT d.e, d.tx FROM mentat_v118.datoms_double_new d WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.a = attr_entid AND d.added = true
        UNION ALL
        SELECT DISTINCT d.e, d.tx FROM mentat_v118.datoms_instant_new d WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.a = attr_entid AND d.added = true
        UNION ALL
        SELECT DISTINCT d.e, d.tx FROM mentat_v118.datoms_text_new d WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.a = attr_entid AND d.added = true
        UNION ALL
        SELECT DISTINCT d.e, d.tx FROM mentat_v118.datoms_keyword_new d WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.a = attr_entid AND d.added = true
        UNION ALL
        SELECT DISTINCT d.e, d.tx FROM mentat_v118.datoms_uuid_new d WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.a = attr_entid AND d.added = true
        UNION ALL
        SELECT DISTINCT d.e, d.tx FROM mentat_v118.datoms_bytes_new d WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.a = attr_entid AND d.added = true
    ) sub
    ORDER BY sub.e;
END;
$function$
;
CREATE OR REPLACE FUNCTION mentat_v118.lookup_entity(attr_ident text, search_value text)
 RETURNS TABLE(entity_id bigint, tx bigint)
 LANGUAGE plpgsql
 STABLE
AS $function$
DECLARE
    attr_entid BIGINT;
    attr_type TEXT;
BEGIN
    SELECT entid, value_type::TEXT INTO attr_entid, attr_type
    FROM mentat_v118.schema WHERE ident = attr_ident;
    IF attr_entid IS NULL THEN
        RAISE EXCEPTION 'Unknown attribute ident: %', attr_ident;
    END IF;

    -- One RETURN QUERY per type: a CASE *expression* can't yield a set (the
    -- old form here made every create_store() fail with a syntax error).
    IF attr_type = 'string' THEN
        RETURN QUERY SELECT d.e, d.tx FROM mentat_v118.datoms_text_new d
             WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.a = attr_entid AND d.added = true
             AND d.v = search_value;
    ELSIF attr_type = 'keyword' THEN
        RETURN QUERY SELECT d.e, d.tx FROM mentat_v118.datoms_keyword_new d
             WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.a = attr_entid AND d.added = true
             AND d.v = search_value;
    ELSIF attr_type = 'long' THEN
        RETURN QUERY SELECT d.e, d.tx FROM mentat_v118.datoms_long_new d
             WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.a = attr_entid AND d.added = true
             AND d.v = search_value::BIGINT;
    ELSIF attr_type = 'ref' THEN
        RETURN QUERY SELECT d.e, d.tx FROM mentat_v118.datoms_ref_new d
             WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.a = attr_entid AND d.added = true
             AND d.v = search_value::BIGINT;
    ELSIF attr_type = 'boolean' THEN
        RETURN QUERY SELECT d.e, d.tx FROM mentat_v118.datoms_boolean_new d
             WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.a = attr_entid AND d.added = true
             AND d.v = search_value::BOOLEAN;
    ELSIF attr_type = 'double' THEN
        RETURN QUERY SELECT d.e, d.tx FROM mentat_v118.datoms_double_new d
             WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.a = attr_entid AND d.added = true
             AND d.v = search_value::DOUBLE PRECISION;
    ELSIF attr_type = 'instant' THEN
        RETURN QUERY SELECT d.e, d.tx FROM mentat_v118.datoms_instant_new d
             WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.a = attr_entid AND d.added = true
             AND d.v = search_value::TIMESTAMPTZ;
    ELSIF attr_type = 'uuid' THEN
        RETURN QUERY SELECT d.e, d.tx FROM mentat_v118.datoms_uuid_new d
             WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.a = attr_entid AND d.added = true
             AND d.v = search_value::UUID;
    END IF;
END;
$function$
;
CREATE OR REPLACE FUNCTION mentat_v118.entity_value(eid bigint, attr_ident text)
 RETURNS text
 LANGUAGE plpgsql
 STABLE
AS $function$
DECLARE
    attr_entid BIGINT;
    attr_type TEXT;
    result TEXT;
BEGIN
    SELECT entid, value_type::TEXT INTO attr_entid, attr_type
    FROM mentat_v118.schema WHERE ident = attr_ident;
    IF attr_entid IS NULL THEN
        RAISE EXCEPTION 'Unknown attribute ident: %', attr_ident;
    END IF;

    CASE attr_type
        WHEN 'string' THEN
            SELECT d.v INTO result FROM mentat_v118.datoms_text_new d
            WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.e = eid AND d.a = attr_entid AND d.added = true
            ORDER BY d.tx DESC LIMIT 1;
        WHEN 'keyword' THEN
            SELECT ':' || d.v INTO result FROM mentat_v118.datoms_keyword_new d
            WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.e = eid AND d.a = attr_entid AND d.added = true
            ORDER BY d.tx DESC LIMIT 1;
        WHEN 'long' THEN
            SELECT d.v::TEXT INTO result FROM mentat_v118.datoms_long_new d
            WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.e = eid AND d.a = attr_entid AND d.added = true
            ORDER BY d.tx DESC LIMIT 1;
        WHEN 'ref' THEN
            SELECT COALESCE(
                (SELECT ri.ident FROM mentat_v118.idents ri WHERE ri.entid = d.v),
                d.v::TEXT
            ) INTO result FROM mentat_v118.datoms_ref_new d
            WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.e = eid AND d.a = attr_entid AND d.added = true
            ORDER BY d.tx DESC LIMIT 1;
        WHEN 'boolean' THEN
            SELECT d.v::TEXT INTO result FROM mentat_v118.datoms_boolean_new d
            WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.e = eid AND d.a = attr_entid AND d.added = true
            ORDER BY d.tx DESC LIMIT 1;
        WHEN 'double' THEN
            SELECT d.v::TEXT INTO result FROM mentat_v118.datoms_double_new d
            WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.e = eid AND d.a = attr_entid AND d.added = true
            ORDER BY d.tx DESC LIMIT 1;
        WHEN 'instant' THEN
            SELECT d.v::TEXT INTO result FROM mentat_v118.datoms_instant_new d
            WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.e = eid AND d.a = attr_entid AND d.added = true
            ORDER BY d.tx DESC LIMIT 1;
        WHEN 'uuid' THEN
            SELECT d.v::TEXT INTO result FROM mentat_v118.datoms_uuid_new d
            WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.e = eid AND d.a = attr_entid AND d.added = true
            ORDER BY d.tx DESC LIMIT 1;
        WHEN 'bytes' THEN
            SELECT encode(d.v, 'hex') INTO result FROM mentat_v118.datoms_bytes_new d
            WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND d.e = eid AND d.a = attr_entid AND d.added = true
            ORDER BY d.tx DESC LIMIT 1;
        ELSE
            result := NULL;
    END CASE;

    RETURN result;
END;
$function$
;
CREATE OR REPLACE FUNCTION mentat_v118.count_by_attribute()
 RETURNS TABLE(attribute text, entity_count bigint, datom_count bigint)
 LANGUAGE plpgsql
 STABLE
AS $function$
BEGIN
    RETURN QUERY
    SELECT
        COALESCE(s.ident, 'entid:' || d.a::TEXT) AS attribute,
        COUNT(DISTINCT d.e) AS entity_count,
        COUNT(*) AS datom_count
    FROM (
SELECT e, a, 0::SMALLINT AS value_type_tag, v::text AS v_text, tx FROM mentat_v118.datoms_ref_new WHERE store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND added = true
UNION ALL
SELECT e, a, 1::SMALLINT AS value_type_tag, v::text AS v_text, tx FROM mentat_v118.datoms_boolean_new WHERE store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND added = true
UNION ALL
SELECT e, a, 2::SMALLINT AS value_type_tag, v::text AS v_text, tx FROM mentat_v118.datoms_long_new WHERE store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND added = true
UNION ALL
SELECT e, a, 3::SMALLINT AS value_type_tag, v::text AS v_text, tx FROM mentat_v118.datoms_double_new WHERE store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND added = true
UNION ALL
SELECT e, a, 4::SMALLINT AS value_type_tag, v::text AS v_text, tx FROM mentat_v118.datoms_instant_new WHERE store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND added = true
UNION ALL
SELECT e, a, 7::SMALLINT AS value_type_tag, v AS v_text, tx FROM mentat_v118.datoms_text_new WHERE store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND added = true
UNION ALL
SELECT e, a, 8::SMALLINT AS value_type_tag, v AS v_text, tx FROM mentat_v118.datoms_keyword_new WHERE store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND added = true
UNION ALL
SELECT e, a, 10::SMALLINT AS value_type_tag, v::text AS v_text, tx FROM mentat_v118.datoms_uuid_new WHERE store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND added = true
UNION ALL
SELECT e, a, 11::SMALLINT AS value_type_tag, encode(v, 'hex') AS v_text, tx FROM mentat_v118.datoms_bytes_new WHERE store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118') AND added = true
    ) d
    LEFT JOIN mentat_v118.schema s ON s.entid = d.a
    GROUP BY d.a, s.ident
    ORDER BY datom_count DESC;
END;
$function$
;
CREATE OR REPLACE FUNCTION mentat_v118.find_text(search_query text)
 RETURNS TABLE(entity_id bigint, attribute text, value text, rank real)
 LANGUAGE plpgsql
 STABLE
AS $function$
BEGIN
    RETURN QUERY
    SELECT
        d.e,
        COALESCE(s.ident, 'entid:' || d.a::TEXT),
        d.v,
        ts_rank_cd(to_tsvector('english', d.v), plainto_tsquery('english', search_query))
    FROM mentat_v118.datoms_text_new d
    LEFT JOIN mentat_v118.schema s ON s.entid = d.a
    WHERE d.store_id = (SELECT store_id FROM mentat.stores WHERE store_name = 'v118')
      AND d.added = true
      AND to_tsvector('english', d.v) @@ plainto_tsquery('english', search_query)
    ORDER BY ts_rank_cd(to_tsvector('english', d.v), plainto_tsquery('english', search_query)) DESC;
END;
$function$
;
