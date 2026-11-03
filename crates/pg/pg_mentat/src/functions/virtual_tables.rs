/// Virtual table views for pg_mentat stores.
///
/// Generates SQL views that provide human-readable access to the underlying
/// type-specific datoms tables, schema, and idents. These views are created
/// automatically when a new store is provisioned via `mentat_create_store()`
/// and can also be regenerated on demand.
///
/// As of Phase 3, views query the type-specific tables (`<schema>.datoms_*_new`)
/// via UNION ALL instead of the legacy wide `{schema}.datoms` table.
use crate::functions::store_management::{get_schema_for_store, quote_ident, validate_store_name};
use pgrx::datum::DatumWithOid;
use pgrx::prelude::*;

// ---------------------------------------------------------------------------
// Extension detection
// ---------------------------------------------------------------------------

/// The schema an extension is installed in, quoted as %I would, or None if
/// it isn't installed. Name its objects through it, never the search path:
/// an extension can live in any schema, the 1.18.0 upgrade calls
/// create_virtual_tables with the path pinned to pg_catalog, and on
/// PostgreSQL 17 CREATE / REFRESH MATERIALIZED VIEW run the SQL edn_q
/// generates with that path (#382).
///
/// Cached per backend (edn_q asks on every compile); a change to pg_proc or
/// pg_namespace (CREATE / DROP / ALTER EXTENSION, ALTER SCHEMA RENAME)
/// clears the cache. Read through the catalog snapshot, not the statement's:
/// a refill after another session's commit must see that commit, or the old
/// schema stays cached until the next unrelated pg_proc change. Takes only
/// AccessShareLock and no XID, so a hot standby can run it.
pub(crate) fn extension_schema(ext_name: &str) -> Option<String> {
    thread_local! {
        static CACHE: std::cell::RefCell<std::collections::HashMap<String, Option<String>>> =
            std::cell::RefCell::new(std::collections::HashMap::new());
        static REGISTERED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        // Bumped by every clear: a read an invalidation overtook isn't cached.
        static GENERATION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }
    #[pg_guard]
    unsafe extern "C-unwind" fn clear(_arg: pg_sys::Datum, _cacheid: i32, _hash: u32) {
        CACHE.with(|c| c.borrow_mut().clear());
        GENERATION.with(|g| g.set(g.get() + 1));
    }
    if !REGISTERED.with(|r| r.replace(true)) {
        for id in [
            pg_sys::SysCacheIdentifier::PROCOID,
            pg_sys::SysCacheIdentifier::NAMESPACEOID,
        ] {
            // SAFETY: a plain C callback with no captured state, registered
            // once per backend; it only clears a thread-local map.
            unsafe {
                pg_sys::CacheRegisterSyscacheCallback(id as _, Some(clear), pg_sys::Datum::from(0))
            };
        }
    }
    if let Some(hit) = CACHE.with(|c| c.borrow().get(ext_name).cloned()) {
        return hit;
    }
    let name = std::ffi::CString::new(ext_name).ok()?;
    // What get_extension_oid does (pg13 has no get_extension_schema): an
    // index scan of pg_extension by name with a NULL snapshot, which is the
    // catalog snapshot. table_open processes pending invalidations first.
    // SAFETY: the scan key points at `name`, alive for the scan; the tuple is
    // read before systable_endscan; errors abort the transaction, which
    // releases the relation and the scan.
    let (generation, schema) = unsafe {
        let rel = pg_sys::table_open(pg_sys::ExtensionRelationId, pg_sys::AccessShareLock as _);
        let generation = GENERATION.with(|g| g.get());
        let mut key: pg_sys::ScanKeyData = std::mem::zeroed();
        pg_sys::ScanKeyInit(
            &mut key,
            pg_sys::Anum_pg_extension_extname as _,
            pg_sys::BTEqualStrategyNumber as _,
            pg_sys::Oid::from(pg_sys::F_NAMEEQ),
            pg_sys::Datum::from(name.as_ptr()),
        );
        let scan = pg_sys::systable_beginscan(
            rel,
            pg_sys::Oid::from(pg_sys::ExtensionNameIndexId),
            true,
            std::ptr::null_mut(),
            1,
            &mut key,
        );
        let tuple = pg_sys::systable_getnext(scan);
        let nsp = (!tuple.is_null()).then(|| {
            (*pg_sys::heap_tuple_get_struct::<pg_sys::FormData_pg_extension>(tuple)).extnamespace
        });
        pg_sys::systable_endscan(scan);
        pg_sys::table_close(rel, pg_sys::AccessShareLock as _);
        let schema = nsp.and_then(|nsp| {
            let n = pg_sys::get_namespace_name(nsp);
            (!n.is_null())
                .then(|| pgrx::spi::quote_identifier(std::ffi::CStr::from_ptr(n).to_string_lossy()))
        });
        (generation, schema)
    };
    if GENERATION.with(|g| g.get()) == generation {
        CACHE.with(|c| c.borrow_mut().insert(ext_name.to_string(), schema.clone()));
    }
    schema
}

// ---------------------------------------------------------------------------
// Helpers for type-specific table queries
// ---------------------------------------------------------------------------

/// Generate a subquery that resolves a schema name to a store_id.
///
/// Schema "mentat" maps to store_name "default"; "mentat_foo" maps to "foo".
fn store_id_subquery(schema: &str) -> String {
    let schema = schema.trim_matches('"');
    let store_name = if schema == "mentat" {
        "default".to_string()
    } else if let Some(name) = schema.strip_prefix("mentat_") {
        name.to_string()
    } else {
        // Fallback: treat the whole schema name as the store name
        schema.to_string()
    };
    format!(
        "(SELECT store_id FROM mentat.stores WHERE store_name = '{}')",
        store_name.replace('\'', "''")
    )
}

/// Generate a UNION ALL query across all 9 type-specific tables that projects
/// columns in a unified format compatible with the old wide-row layout.
///
/// Projected columns: e, a, value_type_tag, v_text (value as text), tx
///
/// `live`: the current_<type> projection (the datoms entities have now);
/// otherwise every assertion in the log, including replaced and retracted
/// ones (history).
fn all_datoms_union_sql(schema: &str, store_id_expr: &str, live: bool) -> String {
    let legs = [
        ("ref", 0, "v::text"),
        ("boolean", 1, "v::text"),
        ("long", 2, "v::text"),
        ("double", 3, "v::text"),
        ("instant", 4, "v::text"),
        ("text", 7, "v"),
        ("keyword", 8, "v"),
        ("uuid", 10, "v::text"),
        ("bytes", 11, "encode(v, 'hex')"),
    ];

    legs.iter()
        .map(|(ty, tag, v_expr)| {
            let (table, added) = if live {
                (format!("current_{ty}"), "")
            } else {
                (format!("datoms_{ty}_new"), " AND added = true")
            };
            format!(
                "SELECT e, a, {tag}::SMALLINT AS value_type_tag, {v_expr} AS v_text, tx \
                 FROM {schema}.{table} \
                 WHERE store_id = {store_id_expr}{added}"
            )
        })
        .collect::<Vec<_>>()
        .join("\nUNION ALL\n")
}

// ---------------------------------------------------------------------------
// View SQL generators
// ---------------------------------------------------------------------------

/// Generate SQL for the `entities` view.
///
/// Shows the entities that have a datom now (the current_<type>
/// projection). first_tx / first_ts are when the entity was first asserted
/// (the earliest assertion in the log, as on 1.18), so replacing its values
/// doesn't move them; last_tx / last_ts and attribute_count are over its
/// current datoms. The log is read in one grouped pass, joined on e: a
/// correlated MIN per log table and entity made a whole-store read ~10x
/// slower than 1.18's.
///
/// Columns: entity_id, first_tx, last_tx, first_ts, last_ts, attribute_count
fn entities_view_sql(schema: &str) -> String {
    let sid = store_id_subquery(schema);
    let union = all_datoms_union_sql(schema, &sid, true);
    let log = [
        "ref", "boolean", "long", "double", "instant", "text", "keyword", "uuid", "bytes",
    ]
    .iter()
    .map(|ty| {
        format!("SELECT e, tx FROM {schema}.datoms_{ty}_new WHERE store_id = {sid} AND added")
    })
    .collect::<Vec<_>>()
    .join("\nUNION ALL\n");
    format!(
        r"CREATE OR REPLACE VIEW {schema}.entities AS
SELECT
    c.entity_id,
    COALESCE(f.first_tx, c.min_tx) AS first_tx,
    c.last_tx,
    COALESCE(f.first_ts, c.min_ts) AS first_ts,
    c.last_ts,
    c.attribute_count
FROM (
SELECT
    d.e AS entity_id,
    MIN(d.tx) AS min_tx,
    MAX(d.tx) AS last_tx,
    MIN(t.tx_instant) AS min_ts,
    MAX(t.tx_instant) AS last_ts,
    COUNT(DISTINCT d.a) AS attribute_count
FROM (
{union}
) d
JOIN {schema}.transactions t ON t.tx = d.tx
GROUP BY d.e
) c
LEFT JOIN (
SELECT l.e, MIN(l.tx) AS first_tx, MIN(t.tx_instant) AS first_ts
FROM (
{log}
) l
JOIN {schema}.transactions t ON t.tx = l.tx
GROUP BY l.e
) f ON f.e = c.entity_id
ORDER BY c.entity_id",
    )
}

/// Generate SQL for the `attributes` view.
///
/// Shows schema information with human-readable attribute names.
/// This view does not reference datoms tables so no changes are needed.
///
/// Columns: entid, ident, value_type, cardinality, unique_constraint, indexed,
///          fulltext, component, no_history
fn attributes_view_sql(schema: &str) -> String {
    format!(
        r"CREATE OR REPLACE VIEW {schema}.attributes AS
SELECT
    s.entid,
    s.ident,
    s.value_type::TEXT AS value_type,
    s.cardinality::TEXT AS cardinality,
    s.unique_constraint::TEXT AS unique_constraint,
    s.indexed,
    s.fulltext,
    s.component,
    s.no_history
FROM {schema}.schema s
ORDER BY s.entid",
        schema = schema
    )
}

/// Generate SQL for the `facts` view.
///
/// Shows the store's current datoms (the current_<type> projection) as a
/// human-readable EAVT representation where entity IDs, attribute IDs,
/// and value type tags are resolved to readable forms.  Each UNION leg directly
/// produces the display value and type name, avoiding the old CASE expression
/// over value_type_tag.
///
/// Columns: entity_id, attribute, value, value_type, tx, tx_time
fn facts_view_sql(schema: &str) -> String {
    let sid = store_id_subquery(schema);

    // Each leg produces: e, a, value (text), value_type (text), tx
    let legs = [
        (
            "current_ref",
            "ref",
            format!(
                "COALESCE((SELECT ri.ident FROM {schema}.idents ri WHERE ri.entid = d.v), d.v::TEXT)",
                schema = schema
            ),
        ),
        (
            "current_boolean",
            "boolean",
            "d.v::TEXT".to_string(),
        ),
        (
            "current_long",
            "long",
            "d.v::TEXT".to_string(),
        ),
        (
            "current_double",
            "double",
            "d.v::TEXT".to_string(),
        ),
        (
            "current_instant",
            "instant",
            "d.v::TEXT".to_string(),
        ),
        (
            "current_text",
            "string",
            "d.v".to_string(),
        ),
        (
            "current_keyword",
            "keyword",
            "':' || d.v".to_string(),
        ),
        (
            "current_uuid",
            "uuid",
            "d.v::TEXT".to_string(),
        ),
        (
            "current_bytes",
            "bytes",
            "encode(d.v, 'hex')".to_string(),
        ),
    ];

    let union = legs
        .iter()
        .map(|(table, type_name, v_expr)| {
            format!(
                "SELECT d.e, d.a, {v_expr} AS value, '{type_name}' AS value_type, d.tx \
                 FROM {schema}.{table} d \
                 WHERE d.store_id = {sid}",
                v_expr = v_expr,
                type_name = type_name,
                table = table,
                sid = sid,
            )
        })
        .collect::<Vec<_>>()
        .join("\nUNION ALL\n");

    format!(
        r"CREATE OR REPLACE VIEW {schema}.facts AS
SELECT
    d.e AS entity_id,
    COALESCE(s.ident, 'entid:' || d.a::TEXT) AS attribute,
    d.value,
    d.value_type,
    d.tx,
    t.tx_instant AS tx_time
FROM (
{union}
) d
LEFT JOIN {schema}.schema s ON s.entid = d.a
LEFT JOIN {schema}.transactions t ON t.tx = d.tx
ORDER BY d.e, d.a",
        schema = schema,
        union = union,
    )
}

/// Generate SQL for type-specific value views: (view name, CREATE statement).
///
/// Each view reads its type's current_<type> projection, so it shows the
/// values entities have now.
fn type_specific_views_sql(schema: &str) -> Vec<(&'static str, String)> {
    let sid = store_id_subquery(schema);

    // (view_name, table)
    let types = [
        ("text_values", "current_text"),
        ("numeric_values", "current_long"),
        ("double_values", "current_double"),
        ("boolean_values", "current_boolean"),
        ("references", "current_ref"),
        ("instant_values", "current_instant"),
        ("uuid_values", "current_uuid"),
        ("keyword_values", "current_keyword"),
        ("bytes_values", "current_bytes"),
    ];

    types
        .iter()
        .map(|(view_name, table)| {
            (
                *view_name,
                format!(
                    r"CREATE OR REPLACE VIEW {schema}.{view_name} AS
SELECT
    d.e AS entity_id,
    COALESCE(s.ident, 'entid:' || d.a::TEXT) AS attribute,
    d.v AS value,
    d.tx
FROM {schema}.{table} d
LEFT JOIN {schema}.schema s ON s.entid = d.a
WHERE d.store_id = {sid}
ORDER BY d.e, d.a"
                ),
            )
        })
        .collect()
}

/// Generate SQL for the `searchable_text` view.
///
/// Provides a view with `to_tsvector` applied to the current text values
/// (`<schema>.current_text`) for full-text search.
///
/// Columns: entity_id, attribute, value, search_vector, tx
fn searchable_text_view_sql(schema: &str) -> String {
    let sid = store_id_subquery(schema);
    format!(
        r"CREATE OR REPLACE VIEW {schema}.searchable_text AS
SELECT
    d.e AS entity_id,
    COALESCE(s.ident, 'entid:' || d.a::TEXT) AS attribute,
    d.v AS value,
    to_tsvector('english', d.v) AS search_vector,
    d.tx
FROM {schema}.current_text d
LEFT JOIN {schema}.schema s ON s.entid = d.a
WHERE d.store_id = {sid} AND d.v IS NOT NULL
ORDER BY d.e, d.a",
        schema = schema,
        sid = sid,
    )
}

/// Generate SQL for the `entities_with_attribute()` helper function.
///
/// Returns the entities that have a value of the given attribute now, from
/// the nine current_<type> projections.
fn entities_with_attribute_fn_sql(schema: &str) -> String {
    let sid = store_id_subquery(schema);

    let tables = [
        "current_ref",
        "current_boolean",
        "current_long",
        "current_double",
        "current_instant",
        "current_text",
        "current_keyword",
        "current_uuid",
        "current_bytes",
    ];

    let union = tables
        .iter()
        .map(|table| {
            format!(
                "SELECT DISTINCT d.e, d.tx FROM {schema}.{table} d \
                 WHERE d.store_id = {sid} AND d.a = attr_entid",
                table = table,
                sid = sid,
            )
        })
        .collect::<Vec<_>>()
        .join("\n        UNION ALL\n        ");

    format!(
        r"CREATE OR REPLACE FUNCTION {schema}.entities_with_attribute(attr_ident TEXT)
RETURNS TABLE(entity_id BIGINT, tx BIGINT)
AS $$
DECLARE
    attr_entid BIGINT;
BEGIN
    SELECT entid INTO attr_entid FROM {schema}.schema WHERE ident = attr_ident;
    IF attr_entid IS NULL THEN
        RAISE EXCEPTION 'Unknown attribute ident: %', attr_ident;
    END IF;

    RETURN QUERY
    SELECT DISTINCT sub.e, sub.tx FROM (
        {union}
    ) sub
    ORDER BY sub.e;
END;
$$ LANGUAGE plpgsql STABLE",
        schema = schema,
        union = union,
    )
}

// ---------------------------------------------------------------------------
// Relationship navigation views
// ---------------------------------------------------------------------------

/// Generate SQL for the `entity_references` view.
///
/// Shows the current reference datoms (current_ref) between entities with
/// human-readable attribute names. This is the core view for navigating entity graphs.
///
/// Columns: source_entity, attribute, target_entity, target_ident, tx
fn entity_references_view_sql(schema: &str) -> String {
    let sid = store_id_subquery(schema);
    format!(
        r"CREATE OR REPLACE VIEW {schema}.entity_references AS
SELECT
    d.e AS source_entity,
    COALESCE(s.ident, 'entid:' || d.a::TEXT) AS attribute,
    d.v AS target_entity,
    i.ident AS target_ident,
    d.tx
FROM {schema}.current_ref d
LEFT JOIN {schema}.schema s ON s.entid = d.a
LEFT JOIN {schema}.idents i ON i.entid = d.v
WHERE d.store_id = {sid}
ORDER BY d.e, d.a",
        schema = schema,
        sid = sid,
    )
}

/// Generate SQL for the `reverse_references` view.
///
/// Shows which entities reference a given entity now (reverse navigation).
/// Useful for answering "who references entity X?" questions.
///
/// Columns: target_entity, attribute, source_entity, tx
fn reverse_references_view_sql(schema: &str) -> String {
    let sid = store_id_subquery(schema);
    format!(
        r"CREATE OR REPLACE VIEW {schema}.reverse_references AS
SELECT
    d.v AS target_entity,
    COALESCE(s.ident, 'entid:' || d.a::TEXT) AS attribute,
    d.e AS source_entity,
    d.tx
FROM {schema}.current_ref d
LEFT JOIN {schema}.schema s ON s.entid = d.a
WHERE d.store_id = {sid}
ORDER BY d.v, d.a",
        schema = schema,
        sid = sid,
    )
}

/// Generate SQL for the `graph_edges` view.
///
/// Treats the current reference datoms (current_ref) as directed edges in a
/// graph. Useful for
/// graph traversal queries using recursive CTEs.
///
/// Columns: source, edge_type, target, tx
fn graph_edges_view_sql(schema: &str) -> String {
    let sid = store_id_subquery(schema);
    format!(
        r"CREATE OR REPLACE VIEW {schema}.graph_edges AS
SELECT
    d.e AS source,
    COALESCE(s.ident, 'entid:' || d.a::TEXT) AS edge_type,
    d.v AS target,
    d.tx
FROM {schema}.current_ref d
LEFT JOIN {schema}.schema s ON s.entid = d.a
WHERE d.store_id = {sid}",
        schema = schema,
        sid = sid,
    )
}

// ---------------------------------------------------------------------------
// Transaction history and temporal views
// ---------------------------------------------------------------------------

/// Generate SQL for the `tx_log` view.
///
/// Shows the transaction log with timestamps, datom counts per transaction,
/// and the types of changes made. Gives SQL users a chronological audit trail.
///
/// Columns: tx, tx_time, datom_count
fn tx_log_view_sql(schema: &str) -> String {
    let sid = store_id_subquery(schema);
    // History: the assertions each transaction wrote, replaced ones too.
    let union = all_datoms_union_sql(schema, &sid, false);
    format!(
        r"CREATE OR REPLACE VIEW {schema}.tx_log AS
SELECT
    t.tx,
    t.tx_instant AS tx_time,
    COALESCE(d.datom_count, 0) AS datom_count
FROM {schema}.transactions t
LEFT JOIN (
    SELECT tx, COUNT(*) AS datom_count
    FROM (
{union}
    ) sub
    GROUP BY tx
) d ON d.tx = t.tx
ORDER BY t.tx DESC",
        schema = schema,
        union = union,
    )
}

/// Generate SQL for the `entity_history` view.
///
/// Shows how entity attributes changed over time by including both
/// assertions (added=true) and retractions (added=false) from all
/// type-specific tables.
///
/// Columns: entity_id, attribute, value, value_type, tx, tx_time, operation
fn entity_history_view_sql(schema: &str) -> String {
    let sid = store_id_subquery(schema);

    let legs = [
        (
            "datoms_ref_new",
            "ref",
            format!(
             "COALESCE((SELECT ri.ident FROM {schema}.idents ri WHERE ri.entid = d.v), d.v::TEXT)",
             schema = schema
         ),
        ),
        ("datoms_boolean_new", "boolean", "d.v::TEXT".to_string()),
        ("datoms_long_new", "long", "d.v::TEXT".to_string()),
        ("datoms_double_new", "double", "d.v::TEXT".to_string()),
        ("datoms_instant_new", "instant", "d.v::TEXT".to_string()),
        ("datoms_text_new", "string", "d.v".to_string()),
        ("datoms_keyword_new", "keyword", "':' || d.v".to_string()),
        ("datoms_uuid_new", "uuid", "d.v::TEXT".to_string()),
        (
            "datoms_bytes_new",
            "bytes",
            "encode(d.v, 'hex')".to_string(),
        ),
    ];

    let union = legs
        .iter()
        .map(|(table, type_name, v_expr)| {
            format!(
                "SELECT d.e, d.a, {v_expr} AS value, '{type_name}' AS value_type, d.tx, d.added \
                 FROM {schema}.{table} d \
                 WHERE d.store_id = {sid}",
                v_expr = v_expr,
                type_name = type_name,
                table = table,
                sid = sid,
            )
        })
        .collect::<Vec<_>>()
        .join("\nUNION ALL\n");

    format!(
        r"CREATE OR REPLACE VIEW {schema}.entity_history AS
SELECT
    d.e AS entity_id,
    COALESCE(s.ident, 'entid:' || d.a::TEXT) AS attribute,
    d.value,
    d.value_type,
    d.tx,
    t.tx_instant AS tx_time,
    CASE WHEN d.added THEN 'assert' ELSE 'retract' END AS operation
FROM (
{union}
) d
LEFT JOIN {schema}.schema s ON s.entid = d.a
LEFT JOIN {schema}.transactions t ON t.tx = d.tx
ORDER BY d.tx DESC, d.e, d.a",
        schema = schema,
        union = union,
    )
}

/// Generate SQL for the `recent_changes` view.
///
/// Shows the most recent assertions, limited to the last 100 transactions.
/// Useful for monitoring and audit queries without scanning all history.
/// History: it reads the log, so it keeps assertions replaced or retracted
/// since.
///
/// Columns: entity_id, attribute, value, value_type, tx, tx_time
fn recent_changes_view_sql(schema: &str) -> String {
    let sid = store_id_subquery(schema);

    let legs = [
        (
            "datoms_ref_new",
            "ref",
            format!(
             "COALESCE((SELECT ri.ident FROM {schema}.idents ri WHERE ri.entid = d.v), d.v::TEXT)",
             schema = schema
         ),
        ),
        ("datoms_boolean_new", "boolean", "d.v::TEXT".to_string()),
        ("datoms_long_new", "long", "d.v::TEXT".to_string()),
        ("datoms_double_new", "double", "d.v::TEXT".to_string()),
        ("datoms_instant_new", "instant", "d.v::TEXT".to_string()),
        ("datoms_text_new", "string", "d.v".to_string()),
        ("datoms_keyword_new", "keyword", "':' || d.v".to_string()),
        ("datoms_uuid_new", "uuid", "d.v::TEXT".to_string()),
        (
            "datoms_bytes_new",
            "bytes",
            "encode(d.v, 'hex')".to_string(),
        ),
    ];

    let union = legs
        .iter()
        .map(|(table, type_name, v_expr)| {
            format!(
                "SELECT d.e, d.a, {v_expr} AS value, '{type_name}' AS value_type, d.tx \
                 FROM {schema}.{table} d \
                 WHERE d.store_id = {sid} AND d.added = true \
                 AND d.tx >= (SELECT COALESCE(MAX(tx) - 100, 0) FROM {schema}.transactions)",
                v_expr = v_expr,
                type_name = type_name,
                table = table,
                sid = sid,
                schema = schema,
            )
        })
        .collect::<Vec<_>>()
        .join("\nUNION ALL\n");

    format!(
        r"CREATE OR REPLACE VIEW {schema}.recent_changes AS
SELECT
    d.e AS entity_id,
    COALESCE(s.ident, 'entid:' || d.a::TEXT) AS attribute,
    d.value,
    d.value_type,
    d.tx,
    t.tx_instant AS tx_time
FROM (
{union}
) d
LEFT JOIN {schema}.schema s ON s.entid = d.a
LEFT JOIN {schema}.transactions t ON t.tx = d.tx
ORDER BY d.tx DESC, d.e, d.a",
        schema = schema,
        union = union,
    )
}

// ---------------------------------------------------------------------------
// Schema summary and statistics views
// ---------------------------------------------------------------------------

/// Generate SQL for the `schema_summary` view.
///
/// Shows each user-defined attribute with usage statistics: how many entities
/// have a value of it now and how many current datoms it has.
/// Excludes system attributes (entid < 100) by default.
///
/// Columns: ident, value_type, cardinality, unique_constraint, indexed,
///          entity_count, datom_count
fn schema_summary_view_sql(schema: &str) -> String {
    let sid = store_id_subquery(schema);
    let union = all_datoms_union_sql(schema, &sid, true);
    format!(
        r"CREATE OR REPLACE VIEW {schema}.schema_summary AS
SELECT
    s.ident,
    s.value_type::TEXT AS value_type,
    s.cardinality::TEXT AS cardinality,
    s.unique_constraint::TEXT AS unique_constraint,
    s.indexed,
    s.fulltext,
    COALESCE(u.entity_count, 0) AS entity_count,
    COALESCE(u.datom_count, 0) AS datom_count
FROM {schema}.schema s
LEFT JOIN (
    SELECT a, COUNT(DISTINCT e) AS entity_count, COUNT(*) AS datom_count
    FROM (
{union}
    ) sub
    GROUP BY a
) u ON u.a = s.entid
WHERE s.entid >= 100
ORDER BY s.ident",
        schema = schema,
        union = union,
    )
}

// ---------------------------------------------------------------------------
// Convenience SQL functions
// ---------------------------------------------------------------------------

/// Generate SQL for the `lookup_entity()` function.
///
/// Finds entities by their current attribute value (current_<type>). This is
/// the SQL equivalent of Datomic's
/// entity lookup: given an attribute ident and a text representation of a
/// value, returns all matching entity IDs.
///
/// Usage: SELECT * FROM {schema}.lookup_entity(':person/name', 'Alice');
fn lookup_entity_fn_sql(schema: &str) -> String {
    let sid = store_id_subquery(schema);

    // Each type-specific table requires matching the text value against
    // the native column with appropriate casting.
    format!(
        r"CREATE OR REPLACE FUNCTION {schema}.lookup_entity(attr_ident TEXT, search_value TEXT)
RETURNS TABLE(entity_id BIGINT, tx BIGINT)
AS $$
DECLARE
    attr_entid BIGINT;
    attr_type TEXT;
BEGIN
    SELECT entid, value_type::TEXT INTO attr_entid, attr_type
    FROM {schema}.schema WHERE ident = attr_ident;
    IF attr_entid IS NULL THEN
        RAISE EXCEPTION 'Unknown attribute ident: %', attr_ident;
    END IF;

    -- One RETURN QUERY per type: a CASE *expression* can't yield a set (the
    -- old form here made every create_store() fail with a syntax error).
    IF attr_type = 'string' THEN
        RETURN QUERY SELECT d.e, d.tx FROM {schema}.current_text d
             WHERE d.store_id = {sid} AND d.a = attr_entid
             AND d.v = search_value;
    ELSIF attr_type = 'keyword' THEN
        RETURN QUERY SELECT d.e, d.tx FROM {schema}.current_keyword d
             WHERE d.store_id = {sid} AND d.a = attr_entid
             AND d.v = search_value;
    ELSIF attr_type = 'long' THEN
        RETURN QUERY SELECT d.e, d.tx FROM {schema}.current_long d
             WHERE d.store_id = {sid} AND d.a = attr_entid
             AND d.v = search_value::BIGINT;
    ELSIF attr_type = 'ref' THEN
        RETURN QUERY SELECT d.e, d.tx FROM {schema}.current_ref d
             WHERE d.store_id = {sid} AND d.a = attr_entid
             AND d.v = search_value::BIGINT;
    ELSIF attr_type = 'boolean' THEN
        RETURN QUERY SELECT d.e, d.tx FROM {schema}.current_boolean d
             WHERE d.store_id = {sid} AND d.a = attr_entid
             AND d.v = search_value::BOOLEAN;
    ELSIF attr_type = 'double' THEN
        RETURN QUERY SELECT d.e, d.tx FROM {schema}.current_double d
             WHERE d.store_id = {sid} AND d.a = attr_entid
             AND d.v = search_value::DOUBLE PRECISION;
    ELSIF attr_type = 'instant' THEN
        RETURN QUERY SELECT d.e, d.tx FROM {schema}.current_instant d
             WHERE d.store_id = {sid} AND d.a = attr_entid
             AND d.v = search_value::TIMESTAMPTZ;
    ELSIF attr_type = 'uuid' THEN
        RETURN QUERY SELECT d.e, d.tx FROM {schema}.current_uuid d
             WHERE d.store_id = {sid} AND d.a = attr_entid
             AND d.v = search_value::UUID;
    END IF;
END;
$$ LANGUAGE plpgsql STABLE",
        schema = schema,
        sid = sid,
    )
}

/// Generate SQL for the `entity_value()` function.
///
/// Gets the current value (current_<type>) of a single attribute on an
/// entity, returned as text; for a cardinality-many attribute, the one
/// asserted last.
/// This is the simplest way for SQL users to look up a specific fact.
///
/// Usage: SELECT {schema}.entity_value(123, ':person/name');
fn entity_value_fn_sql(schema: &str) -> String {
    let sid = store_id_subquery(schema);
    format!(
        r"CREATE OR REPLACE FUNCTION {schema}.entity_value(eid BIGINT, attr_ident TEXT)
RETURNS TEXT
AS $$
DECLARE
    attr_entid BIGINT;
    attr_type TEXT;
    result TEXT;
BEGIN
    SELECT entid, value_type::TEXT INTO attr_entid, attr_type
    FROM {schema}.schema WHERE ident = attr_ident;
    IF attr_entid IS NULL THEN
        RAISE EXCEPTION 'Unknown attribute ident: %', attr_ident;
    END IF;

    CASE attr_type
        WHEN 'string' THEN
            SELECT d.v INTO result FROM {schema}.current_text d
            WHERE d.store_id = {sid} AND d.e = eid AND d.a = attr_entid
            ORDER BY d.tx DESC LIMIT 1;
        WHEN 'keyword' THEN
            SELECT ':' || d.v INTO result FROM {schema}.current_keyword d
            WHERE d.store_id = {sid} AND d.e = eid AND d.a = attr_entid
            ORDER BY d.tx DESC LIMIT 1;
        WHEN 'long' THEN
            SELECT d.v::TEXT INTO result FROM {schema}.current_long d
            WHERE d.store_id = {sid} AND d.e = eid AND d.a = attr_entid
            ORDER BY d.tx DESC LIMIT 1;
        WHEN 'ref' THEN
            SELECT COALESCE(
                (SELECT ri.ident FROM {schema}.idents ri WHERE ri.entid = d.v),
                d.v::TEXT
            ) INTO result FROM {schema}.current_ref d
            WHERE d.store_id = {sid} AND d.e = eid AND d.a = attr_entid
            ORDER BY d.tx DESC LIMIT 1;
        WHEN 'boolean' THEN
            SELECT d.v::TEXT INTO result FROM {schema}.current_boolean d
            WHERE d.store_id = {sid} AND d.e = eid AND d.a = attr_entid
            ORDER BY d.tx DESC LIMIT 1;
        WHEN 'double' THEN
            SELECT d.v::TEXT INTO result FROM {schema}.current_double d
            WHERE d.store_id = {sid} AND d.e = eid AND d.a = attr_entid
            ORDER BY d.tx DESC LIMIT 1;
        WHEN 'instant' THEN
            SELECT d.v::TEXT INTO result FROM {schema}.current_instant d
            WHERE d.store_id = {sid} AND d.e = eid AND d.a = attr_entid
            ORDER BY d.tx DESC LIMIT 1;
        WHEN 'uuid' THEN
            SELECT d.v::TEXT INTO result FROM {schema}.current_uuid d
            WHERE d.store_id = {sid} AND d.e = eid AND d.a = attr_entid
            ORDER BY d.tx DESC LIMIT 1;
        WHEN 'bytes' THEN
            SELECT encode(d.v, 'hex') INTO result FROM {schema}.current_bytes d
            WHERE d.store_id = {sid} AND d.e = eid AND d.a = attr_entid
            ORDER BY d.tx DESC LIMIT 1;
        ELSE
            result := NULL;
    END CASE;

    RETURN result;
END;
$$ LANGUAGE plpgsql STABLE",
        schema = schema,
        sid = sid,
    )
}

/// Generate SQL for the `count_by_attribute()` function.
///
/// Returns per-attribute counts of the current datoms: how many distinct
/// entities have each attribute, and the datom count. Useful for analytics and understanding
/// data distribution.
///
/// Usage: SELECT * FROM {schema}.count_by_attribute();
fn count_by_attribute_fn_sql(schema: &str) -> String {
    let sid = store_id_subquery(schema);
    let union = all_datoms_union_sql(schema, &sid, true);
    format!(
        r"CREATE OR REPLACE FUNCTION {schema}.count_by_attribute()
RETURNS TABLE(attribute TEXT, entity_count BIGINT, datom_count BIGINT)
AS $$
BEGIN
    RETURN QUERY
    SELECT
        COALESCE(s.ident, 'entid:' || d.a::TEXT) AS attribute,
        COUNT(DISTINCT d.e) AS entity_count,
        COUNT(*) AS datom_count
    FROM (
{union}
    ) d
    LEFT JOIN {schema}.schema s ON s.entid = d.a
    GROUP BY d.a, s.ident
    ORDER BY datom_count DESC;
END;
$$ LANGUAGE plpgsql STABLE",
        schema = schema,
        union = union,
    )
}

/// Generate SQL for the `find_text()` function.
///
/// Full-text search over the current text values (current_text): returns
/// the entities whose value matches a text search query.
/// Uses PostgreSQL's built-in tsquery parsing so SQL users can use natural
/// language or boolean search operators.
///
/// Usage: SELECT * FROM {schema}.find_text('alice & engineer');
fn find_text_fn_sql(schema: &str) -> String {
    let sid = store_id_subquery(schema);
    format!(
        r"CREATE OR REPLACE FUNCTION {schema}.find_text(search_query TEXT)
RETURNS TABLE(entity_id BIGINT, attribute TEXT, value TEXT, rank REAL)
AS $$
BEGIN
    RETURN QUERY
    SELECT
        d.e,
        COALESCE(s.ident, 'entid:' || d.a::TEXT),
        d.v,
        ts_rank_cd(to_tsvector('english', d.v), plainto_tsquery('english', search_query))
    FROM {schema}.current_text d
    LEFT JOIN {schema}.schema s ON s.entid = d.a
    WHERE d.store_id = {sid}
      AND to_tsvector('english', d.v) @@ plainto_tsquery('english', search_query)
    ORDER BY ts_rank_cd(to_tsvector('english', d.v), plainto_tsquery('english', search_query)) DESC;
END;
$$ LANGUAGE plpgsql STABLE",
        schema = schema,
        sid = sid,
    )
}

// ---------------------------------------------------------------------------
// Optional extension-dependent indexes
// ---------------------------------------------------------------------------

/// SQL for an optional GIN trigram index (requires pg_trgm) on one of the
/// store's current_text / current_keyword projections, which the views
/// read, for fast LIKE/ILIKE and similarity searches. `trgm`
/// is pg_trgm's schema, quoted (extension_schema).
///
/// Named for the table, not the store: the index is in the store's own
/// schema. A store name in it truncated to the same 63 characters for both
/// tables once the name reached 53, and each rename_store added two more.
fn trigram_index_sql(schema: &str, table: &str, trgm: &str) -> String {
    format!(
        "CREATE INDEX IF NOT EXISTS idx_{table}_trgm ON {schema}.{table} \
         USING GIN (v {trgm}.gin_trgm_ops)"
    )
}

/// Whether `table` already has a gin_trgm_ops index, under any name.
fn has_trigram_index(table: &str) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    Ok(Spi::get_one_with_args::<bool>(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_index i \
           JOIN pg_catalog.pg_opclass o ON o.oid = i.indclass[0] \
          WHERE i.indrelid = $1::pg_catalog.regclass AND o.opcname = 'gin_trgm_ops')",
        &[DatumWithOid::from(table)],
    )?
    .unwrap_or(false))
}

/// One of a store's views or helper functions: (kind, name, identity
/// argument types, CREATE OR REPLACE statement). `kind` is "VIEW" or
/// "FUNCTION", as ALTER EXTENSION names them.
type StoreObject = (&'static str, &'static str, &'static str, String);

/// The store's views and helper functions, in creation order. Every
/// generator names the store's own tables under `schema` (#337): nothing is
/// rewritten afterwards.
fn views_and_functions_sql(schema: &str) -> Vec<StoreObject> {
    let view = |name, sql| ("VIEW", name, "", sql);
    let function = |name, args, sql| ("FUNCTION", name, args, sql);
    let mut out = vec![
        // Core views
        view("entities", entities_view_sql(schema)),
        view("attributes", attributes_view_sql(schema)),
        view("facts", facts_view_sql(schema)),
    ];
    // Type-specific views
    out.extend(
        type_specific_views_sql(schema)
            .into_iter()
            .map(|(name, sql)| view(name, sql)),
    );
    out.extend([
        // Full-text search view
        view("searchable_text", searchable_text_view_sql(schema)),
        // Relationship navigation views
        view("entity_references", entity_references_view_sql(schema)),
        view("reverse_references", reverse_references_view_sql(schema)),
        view("graph_edges", graph_edges_view_sql(schema)),
        // Transaction history and temporal views
        view("tx_log", tx_log_view_sql(schema)),
        view("entity_history", entity_history_view_sql(schema)),
        view("recent_changes", recent_changes_view_sql(schema)),
        // Schema summary view
        view("schema_summary", schema_summary_view_sql(schema)),
        // Helper functions
        function(
            "entities_with_attribute",
            "text",
            entities_with_attribute_fn_sql(schema),
        ),
        function("lookup_entity", "text, text", lookup_entity_fn_sql(schema)),
        function("entity_value", "bigint, text", entity_value_fn_sql(schema)),
        function("count_by_attribute", "", count_by_attribute_fn_sql(schema)),
        function("find_text", "text", find_text_fn_sql(schema)),
    ]);
    out
}

/// Whether the current role owns `table` (CREATE INDEX on it needs that).
fn owns_table(table: &str) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    Ok(Spi::get_one_with_args::<bool>(
        "SELECT pg_has_role(current_user, c.relowner, 'USAGE') FROM pg_class c WHERE c.oid = $1::regclass",
        &[DatumWithOid::from(table)],
    )?
    .unwrap_or(false))
}

// ---------------------------------------------------------------------------
// Public API: create all views for a store
// ---------------------------------------------------------------------------

/// Create all virtual table views for a given store schema.
///
/// This is called internally by `mentat_create_store()` and can also be
/// invoked directly to regenerate views for an existing store.
pub fn create_virtual_tables_for_schema(
    schema: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // The views embed the store's id by name: refresh them after a rename.
    for (_, _, _, sql) in views_and_functions_sql(schema) {
        Spi::run(&sql)?;
    }

    // Optional trigram indexes on the store's narrow text tables. They are
    // tuning, not structure (the store layout already carries the shipped
    // full-text GIN index). CREATE INDEX needs table ownership, which a role
    // regenerating the default store's views on a trusted install doesn't
    // have, so skip them for a non-owner rather than failing.
    if let Some(trgm) = extension_schema("pg_trgm") {
        if owns_table(&format!("{schema}.current_text"))? {
            for table in ["current_text", "current_keyword"] {
                // One is enough: a rename, or one the user made, keeps its own.
                if !has_trigram_index(&format!("{schema}.{table}"))? {
                    Spi::run(&trigram_index_sql(schema, table, &trgm))?;
                }
            }
        }
    }

    // NOTE: pg_vector support is planned for future implementation.
    // When available, this will create HNSW or IVFFlat indexes on embedding
    // columns for semantic similarity search over entity attributes.

    Ok(())
}

// ---------------------------------------------------------------------------
// SQL-callable function
// ---------------------------------------------------------------------------

/// Regenerate all virtual table views for a named store.
///
/// This recreates the standard views (entities, attributes, facts, type-specific
/// views, searchable_text) and the `entities_with_attribute()` helper function
/// in the store's schema. It also creates extension-dependent indexes if the
/// relevant extensions (pg_trgm, etc.) are installed.
///
/// # Example
/// ```sql
/// SELECT mentat_create_virtual_tables('my_store');
/// ```
#[pg_extern]
pub fn create_virtual_tables(
    store_name: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    validate_store_name(store_name)?;

    let schema_name = get_schema_for_store(store_name);
    let quoted_schema = quote_ident(&schema_name);

    create_virtual_tables_for_schema(&quoted_schema)?;

    Ok(format!(
        "Virtual tables created for store '{}'.",
        store_name
    ))
}

/// The CREATE OR REPLACE statements of a named store's views and helper
/// functions, one row per object, as create_store makes them. The upgrade
/// scripts use it to replace the ones a store already has (1.19.0: they
/// read the current_<type> projections, #395); it creates nothing itself.
#[pg_extern(schema = "mentat", name = "_store_virtual_tables")]
fn store_virtual_tables(
    store_name: &str,
) -> Result<
    TableIterator<
        'static,
        (
            name!(kind, String),
            name!(name, String),
            name!(args, String),
            name!(sql, String),
        ),
    >,
    Box<dyn std::error::Error + Send + Sync>,
> {
    validate_store_name(store_name)?;
    let schema = quote_ident(&get_schema_for_store(store_name));
    Ok(TableIterator::new(
        views_and_functions_sql(&schema)
            .into_iter()
            .map(|(kind, name, args, sql)| (kind.into(), name.into(), args.into(), sql))
            .collect::<Vec<_>>(),
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_store_id_subquery_default() {
        let sq = store_id_subquery("mentat");
        assert!(sq.contains("store_name = 'default'"));
    }

    #[test]
    fn test_store_id_subquery_custom() {
        let sq = store_id_subquery("mentat_my_store");
        assert!(sq.contains("store_name = 'my_store'"));
    }

    #[test]
    fn test_all_datoms_union_sql_covers_all_tables() {
        for (live, prefix, suffix) in [(true, "current_", ""), (false, "datoms_", "_new")] {
            let sql = all_datoms_union_sql("mentat", "42", live);
            for ty in [
                "ref", "boolean", "long", "double", "instant", "text", "keyword", "uuid", "bytes",
            ] {
                assert!(
                    sql.contains(&format!("mentat.{prefix}{ty}{suffix} ")),
                    "{sql}"
                );
            }
            assert!(sql.contains("UNION ALL"));
            assert!(sql.contains("store_id = 42"));
            assert_eq!(sql.contains("added"), !live, "{sql}");
        }
    }

    // Since 1.5.0 the log is append-only: a replaced or retracted value
    // keeps its added = true row. What reads it with only `added = true`
    // shows values entities no longer have (#395). Only the views that are
    // history read the log.
    #[test]
    fn current_state_objects_read_the_projection() {
        let history = ["tx_log", "entity_history", "recent_changes"];
        for (_, name, _, sql) in views_and_functions_sql("mentat_s") {
            if history.contains(&name) {
                assert!(sql.contains("mentat_s.datoms_"), "{name}: {sql}");
            } else if name == "entities" {
                // Its rows are current; only first_tx / first_ts look in
                // the log, in the LEFT JOIN.
                let start = sql.find("FROM (\nSELECT\n    d.e").unwrap();
                let rows = &sql[start..sql.find("LEFT JOIN").unwrap()];
                assert!(!rows.contains("datoms_"), "{rows}");
                assert!(sql[start..].contains("datoms_text_new"), "{sql}");
            } else {
                assert!(!sql.contains("datoms_"), "{name} reads the log: {sql}");
                assert!(!sql.contains("added"), "{name}: {sql}");
            }
        }
    }

    #[test]
    fn test_entities_view_sql_uses_union() {
        let sql = entities_view_sql("mentat");
        assert!(sql.contains("UNION ALL"));
        assert!(sql.contains("mentat.current_ref"));
        assert!(sql.contains("GROUP BY d.e"));
        assert!(sql.contains("mentat.transactions"));
        assert!(!sql.contains("mentat.datoms d"));
    }

    #[test]
    fn test_attributes_view_sql_references_schema_table() {
        let sql = attributes_view_sql("mentat");
        assert!(sql.contains("mentat.schema"));
        assert!(sql.contains("value_type"));
        assert!(sql.contains("cardinality"));
    }

    #[test]
    fn test_facts_view_sql_uses_type_specific_tables() {
        let sql = facts_view_sql("mentat");
        assert!(sql.contains("mentat.current_ref"));
        assert!(sql.contains("mentat.current_boolean"));
        assert!(sql.contains("mentat.current_long"));
        assert!(sql.contains("mentat.current_double"));
        assert!(sql.contains("mentat.current_instant"));
        assert!(sql.contains("mentat.current_text"));
        assert!(sql.contains("mentat.current_keyword"));
        assert!(sql.contains("mentat.current_uuid"));
        assert!(sql.contains("mentat.current_bytes"));
        assert!(sql.contains("'ref'"));
        assert!(sql.contains("'boolean'"));
        assert!(sql.contains("'long'"));
        assert!(sql.contains("'double'"));
        assert!(sql.contains("'instant'"));
        assert!(sql.contains("'string'"));
        assert!(sql.contains("'keyword'"));
        assert!(sql.contains("'uuid'"));
        assert!(sql.contains("'bytes'"));
        assert!(sql.contains("UNION ALL"));
    }

    #[test]
    fn test_type_specific_views_sql_creates_all_views() {
        let sql = type_specific_views_sql("mentat_test")
            .into_iter()
            .map(|(_, sql)| sql)
            .collect::<Vec<_>>()
            .join(";\n");
        assert!(sql.contains("mentat_test.text_values"));
        assert!(sql.contains("mentat_test.numeric_values"));
        assert!(sql.contains("mentat_test.double_values"));
        assert!(sql.contains("mentat_test.boolean_values"));
        assert!(sql.contains("mentat_test.references"));
        assert!(sql.contains("mentat_test.instant_values"));
        assert!(sql.contains("mentat_test.uuid_values"));
        assert!(sql.contains("mentat_test.keyword_values"));
        assert!(sql.contains("mentat_test.bytes_values"));
        // The store's current-state projections, not the log
        assert!(sql.contains("mentat_test.current_text"));
        assert!(sql.contains("mentat_test.current_long"));
        assert!(sql.contains("mentat_test.current_ref"));
        assert!(!sql.contains("datoms"));
        assert!(!sql.contains("value_type_tag"));
    }

    #[test]
    fn test_searchable_text_view_sql_uses_text_table() {
        let sql = searchable_text_view_sql("mentat");
        assert!(sql.contains("to_tsvector"));
        assert!(sql.contains("mentat.current_text"));
        assert!(!sql.contains("value_type_tag"));
    }

    #[test]
    fn test_entities_with_attribute_fn_sql() {
        let sql = entities_with_attribute_fn_sql("mentat");
        assert!(sql.contains("FUNCTION mentat.entities_with_attribute"));
        assert!(sql.contains("attr_ident TEXT"));
        assert!(sql.contains("RETURN QUERY"));
        assert!(sql.contains("UNION ALL"));
        assert!(sql.contains("mentat.current_ref"));
        assert!(sql.contains("mentat.current_text"));
    }

    #[test]
    fn test_trigram_index_sql_uses_type_specific_tables() {
        let sql = trigram_index_sql("mentat_s", "current_text", "public");
        assert!(sql.contains("v public.gin_trgm_ops"));
        assert!(!sql.contains("added"), "{sql}");
        let sql = trigram_index_sql("mentat_s", "current_keyword", "\"Ext Schema\"");
        assert!(sql.contains("v \"Ext Schema\".gin_trgm_ops"));
        assert!(sql.contains("idx_current_keyword_trgm ON mentat_s.current_keyword"));
    }

    // The generators used to name mentat.datoms_* and a str::replace moved
    // them to the store's schema; for a schema ending in "mentat" it also
    // matched inside the schema name (mentat_app_mentat.datoms_text_new ->
    // mentat_app_mentat_app_mentat.datoms_text_new).
    #[test]
    fn store_tables_are_named_in_the_store_schema() {
        for schema in [
            "mentat",
            "mentat_app_mentat",
            "mentat_mentat",
            "mentat_mentat_x",
            "\"mentat_A_mentat\"",
        ] {
            let mut sql = views_and_functions_sql(schema)
                .into_iter()
                .map(|(_, _, _, sql)| sql)
                .collect::<Vec<_>>()
                .join(";\n");
            sql.push_str(&trigram_index_sql(schema, "current_text", "public"));
            let mut n = 0;
            for t in ["datoms_", "current_", "transactions", "schema s", "idents"] {
                for (at, _) in sql.match_indices(&format!(".{t}")) {
                    let before = &sql[..at];
                    assert!(
                        before.ends_with(schema),
                        "{schema}: {}",
                        &sql[at.saturating_sub(40)..at + 20]
                    );
                    let start = before.len() - schema.len();
                    let prev = before[..start].chars().last().unwrap_or(' ');
                    assert!(
                        !prev.is_alphanumeric() && prev != '_' && prev != '"',
                        "{schema}: {}",
                        &sql[start.saturating_sub(20)..at]
                    );
                    n += 1;
                }
            }
            assert!(n > 50, "{schema}: {n}");
        }
    }

    #[test]
    fn test_custom_schema_name() {
        let sql = entities_view_sql("mentat_my_store");
        assert!(sql.contains("UNION ALL"));
        assert!(sql.contains("mentat_my_store.entities"));
        assert!(sql.contains("store_name = 'my_store'"));
    }

    // --- Relationship navigation views ---

    #[test]
    fn test_entity_references_view_sql() {
        let sql = entity_references_view_sql("mentat");
        assert!(sql.contains("mentat.entity_references"));
        assert!(sql.contains("mentat.current_ref"));
        assert!(sql.contains("source_entity"));
        assert!(sql.contains("target_entity"));
        assert!(sql.contains("target_ident"));
        assert!(sql.contains("mentat.idents"));
    }

    #[test]
    fn test_reverse_references_view_sql() {
        let sql = reverse_references_view_sql("mentat");
        assert!(sql.contains("mentat.reverse_references"));
        assert!(sql.contains("mentat.current_ref"));
        assert!(sql.contains("d.v AS target_entity"));
        assert!(sql.contains("d.e AS source_entity"));
    }

    #[test]
    fn test_graph_edges_view_sql() {
        let sql = graph_edges_view_sql("mentat");
        assert!(sql.contains("mentat.graph_edges"));
        assert!(sql.contains("d.e AS source"));
        assert!(sql.contains("d.v AS target"));
        assert!(sql.contains("edge_type"));
    }

    #[test]
    fn test_entity_references_custom_schema() {
        let sql = entity_references_view_sql("mentat_test");
        assert!(sql.contains("mentat_test.entity_references"));
        assert!(sql.contains("store_name = 'test'"));
    }

    // --- Transaction history views ---

    #[test]
    fn test_tx_log_view_sql() {
        let sql = tx_log_view_sql("mentat");
        assert!(sql.contains("mentat.tx_log"));
        assert!(sql.contains("mentat.transactions"));
        assert!(sql.contains("datom_count"));
        assert!(sql.contains("tx_time"));
        assert!(sql.contains("ORDER BY t.tx DESC"));
    }

    #[test]
    fn test_entity_history_view_sql() {
        let sql = entity_history_view_sql("mentat");
        assert!(sql.contains("mentat.entity_history"));
        assert!(sql.contains("UNION ALL"));
        assert!(sql.contains("mentat.datoms_ref_new"));
        assert!(sql.contains("mentat.datoms_text_new"));
        assert!(sql.contains("'assert'"));
        assert!(sql.contains("'retract'"));
        assert!(sql.contains("operation"));
        // Should include retractions (no added=true filter)
        assert!(!sql.contains("AND d.added = true"));
    }

    #[test]
    fn test_recent_changes_view_sql() {
        let sql = recent_changes_view_sql("mentat");
        assert!(sql.contains("mentat.recent_changes"));
        assert!(sql.contains("UNION ALL"));
        assert!(sql.contains("MAX(tx) - 100"));
        assert!(sql.contains("ORDER BY d.tx DESC"));
    }

    // --- Schema summary view ---

    #[test]
    fn test_schema_summary_view_sql() {
        let sql = schema_summary_view_sql("mentat");
        assert!(sql.contains("mentat.schema_summary"));
        assert!(sql.contains("entity_count"));
        assert!(sql.contains("datom_count"));
        assert!(sql.contains("s.entid >= 100"));
        assert!(sql.contains("UNION ALL"));
    }

    // --- Convenience functions ---

    #[test]
    fn test_lookup_entity_fn_sql() {
        let sql = lookup_entity_fn_sql("mentat");
        assert!(sql.contains("FUNCTION mentat.lookup_entity"));
        assert!(sql.contains("attr_ident TEXT"));
        assert!(sql.contains("search_value TEXT"));
        assert!(sql.contains("RETURN QUERY"));
        assert!(sql.contains("mentat.current_text"));
        assert!(sql.contains("mentat.current_long"));
        assert!(sql.contains("mentat.current_ref"));
        assert!(sql.contains("mentat.current_boolean"));
        assert!(sql.contains("STABLE"));
    }

    #[test]
    fn test_entity_value_fn_sql() {
        let sql = entity_value_fn_sql("mentat");
        assert!(sql.contains("FUNCTION mentat.entity_value"));
        assert!(sql.contains("eid BIGINT"));
        assert!(sql.contains("attr_ident TEXT"));
        assert!(sql.contains("RETURNS TEXT"));
        assert!(sql.contains("mentat.current_text"));
        assert!(sql.contains("mentat.current_long"));
        assert!(sql.contains("mentat.current_ref"));
        assert!(sql.contains("ORDER BY d.tx DESC LIMIT 1"));
    }

    #[test]
    fn test_count_by_attribute_fn_sql() {
        let sql = count_by_attribute_fn_sql("mentat");
        assert!(sql.contains("FUNCTION mentat.count_by_attribute"));
        assert!(sql.contains("entity_count"));
        assert!(sql.contains("datom_count"));
        assert!(sql.contains("UNION ALL"));
        assert!(sql.contains("GROUP BY"));
    }

    #[test]
    fn test_find_text_fn_sql() {
        let sql = find_text_fn_sql("mentat");
        assert!(sql.contains("FUNCTION mentat.find_text"));
        assert!(sql.contains("search_query TEXT"));
        assert!(sql.contains("ts_rank"));
        assert!(sql.contains("plainto_tsquery"));
        assert!(sql.contains("mentat.current_text"));
        assert!(sql.contains("rank REAL"));
    }

    #[test]
    fn test_lookup_entity_custom_schema() {
        let sql = lookup_entity_fn_sql("mentat_test");
        assert!(sql.contains("FUNCTION mentat_test.lookup_entity"));
        assert!(sql.contains("store_name = 'test'"));
    }

    #[test]
    fn test_entity_value_custom_schema() {
        let sql = entity_value_fn_sql("mentat_test");
        assert!(sql.contains("FUNCTION mentat_test.entity_value"));
        assert!(sql.contains("mentat_test.schema"));
        assert!(sql.contains("mentat_test.idents"));
    }
}
