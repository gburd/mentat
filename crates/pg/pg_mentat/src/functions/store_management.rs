use crate::error::MentatError;
use pgrx::datum::DatumWithOid;
use pgrx::prelude::*;
use pgrx::JsonB;
use serde_json::json;

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

/// Maximum length for a store name.
const MAX_STORE_NAME_LEN: usize = 63;

/// Validate a store name.
///
/// Rules:
/// - Must not be empty
/// - Must be <= 63 characters (PostgreSQL identifier limit)
/// - Must start with a letter or underscore
/// - Must contain only letters, digits, and underscores
/// - Must not be a reserved name (default, pg_*, information_schema)
pub fn validate_store_name(name: &str) -> Result<(), MentatError> {
    if name.is_empty() {
        return Err(MentatError::InvalidStoreName {
            store_name: name.to_string(),
            reason: "Store name cannot be empty.".to_string(),
        });
    }

    if name.len() > MAX_STORE_NAME_LEN {
        return Err(MentatError::InvalidStoreName {
            store_name: name.to_string(),
            reason: format!(
                "Store name exceeds maximum length of {} characters.",
                MAX_STORE_NAME_LEN
            ),
        });
    }

    let first = name.chars().next().unwrap();
    if !first.is_ascii_alphabetic() && first != '_' {
        return Err(MentatError::InvalidStoreName {
            store_name: name.to_string(),
            reason: "Store name must start with a letter or underscore.".to_string(),
        });
    }

    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(MentatError::InvalidStoreName {
            store_name: name.to_string(),
            reason: "Store name must contain only letters, digits, and underscores.".to_string(),
        });
    }

    // Check reserved names
    let lower = name.to_lowercase();
    if lower == "default" || lower == "information_schema" || lower.starts_with("pg_") {
        return Err(MentatError::InvalidStoreName {
            store_name: name.to_string(),
            reason: format!("'{}' is a reserved name.", name),
        });
    }

    Ok(())
}

/// Quote a SQL identifier safely using PostgreSQL's quote_ident convention.
/// This prevents SQL injection in dynamic schema/table names.
pub fn quote_ident(ident: &str) -> String {
    let is_simple = !ident.is_empty()
        && (ident.chars().next().unwrap().is_ascii_lowercase() || ident.starts_with('_'))
        && ident
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');

    if is_simple {
        ident.to_string()
    } else {
        format!("\"{}\"", ident.replace('"', "\"\""))
    }
}

/// Derive the PostgreSQL schema name for a given store name.
/// The default store uses "mentat"; other stores use "mentat_<store_name>".
pub fn get_schema_for_store(store_name: &str) -> String {
    if store_name == "default" {
        "mentat".to_string()
    } else {
        format!("mentat_{}", store_name)
    }
}

// ---------------------------------------------------------------------------
// Helper: check if a store exists in the metadata table
// ---------------------------------------------------------------------------

fn store_exists(store_name: &str) -> Result<bool, pgrx::spi::SpiError> {
    Spi::connect(|client| {
        let exists = client
            .select(
                "SELECT 1 FROM mentat.stores WHERE store_name = $1",
                None,
                &[DatumWithOid::from(store_name)],
            )?
            .next()
            .is_some();
        Ok(exists)
    })
}

fn get_store_schema(store_name: &str) -> Result<Option<String>, pgrx::spi::SpiError> {
    Spi::connect(|client| {
        let mut rows = client.select(
            "SELECT schema_name FROM mentat.stores WHERE store_name = $1",
            None,
            &[DatumWithOid::from(store_name)],
        )?;
        match rows.next() {
            Some(row) => Ok(row.get::<String>(1)?),
            None => Ok(None),
        }
    })
}

// ---------------------------------------------------------------------------
// CRUD functions
// ---------------------------------------------------------------------------

/// Create a new named store with its own PostgreSQL schema.
///
/// The store lives in schema `mentat_<name>`, with the same tables the
/// default store has in `mentat` (narrow datom logs, current-state
/// projections, schema / idents / partitions / transactions, partition
/// sequences), created by `mentat._create_store_layout`
/// (sql/30_store_layout.sql) from the default store's own definitions. The
/// calling role owns it; roles with access to the default store get the same
/// access to it (`mentat._store_grants`).
///
/// # Example
/// ```sql
/// SELECT create_store('my_store', 'A store for my project');
/// SELECT t('my_store', '[{:db/ident :m/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]');
/// ```
#[pg_extern]
pub fn create_store(
    store_name: &str,
    description: default!(Option<&str>, "NULL"),
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    validate_store_name(store_name)?;

    // Check if store already exists
    if store_exists(store_name)? {
        return Err(Box::new(MentatError::StoreExists {
            store_name: store_name.to_string(),
        }));
    }

    let schema_name = get_schema_for_store(store_name);
    let quoted_schema = quote_ident(&schema_name);

    // Register first: the layout needs the store_id.
    let store_id: i64 = Spi::get_one_with_args(
        "INSERT INTO mentat.stores (store_name, schema_name, description) \
         VALUES ($1, $2, $3) RETURNING store_id",
        &[
            DatumWithOid::from(store_name),
            DatumWithOid::from(schema_name.as_str()),
            DatumWithOid::from(description),
        ],
    )?
    .ok_or("INSERT INTO mentat.stores returned no store_id")?;

    Spi::run(&format!("CREATE SCHEMA {}", quoted_schema))?;
    Spi::run_with_args(
        "SELECT mentat._create_store_layout($1, $2)",
        &[
            DatumWithOid::from(schema_name.as_str()),
            DatumWithOid::from(store_id),
        ],
    )?;

    // Cross-backend schema-cache invalidation for this store.
    Spi::run_with_args(
        "INSERT INTO mentat.cache_generation (store_name, gen) VALUES ($1, 1) \
         ON CONFLICT (store_name) DO UPDATE SET gen = mentat.cache_generation.gen + 1",
        &[DatumWithOid::from(store_name)],
    )?;
    // A store of the same name may have existed earlier in this backend.
    crate::cache::invalidate_store_cache(store_name);

    // Create virtual table views (entities, attributes, facts, etc.)
    crate::functions::virtual_tables::create_virtual_tables_for_schema(&quoted_schema, store_name)?;

    Spi::run_with_args(
        "SELECT mentat._store_grants($1)",
        &[DatumWithOid::from(schema_name.as_str())],
    )?;

    Ok(format!("Store '{}' created successfully.", store_name))
}

/// Drop a named store, removing its schema and all data.
///
/// The default store cannot be dropped.
///
/// # Example
/// ```sql
/// SELECT mentat_drop_store('my_store');
/// ```
#[pg_extern]
pub fn drop_store(store_name: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    if store_name == "default" {
        return Err(Box::new(MentatError::CannotDropDefaultStore));
    }

    validate_store_name(store_name)?;

    // Verify store exists and get its schema name
    let schema_name = get_store_schema(store_name)?.ok_or_else(|| {
        Box::new(MentatError::StoreNotFound {
            store_name: store_name.to_string(),
        }) as Box<dyn std::error::Error + Send + Sync>
    })?;

    let quoted_schema = quote_ident(&schema_name);

    // Drop the schema with CASCADE
    Spi::run(&format!("DROP SCHEMA IF EXISTS {} CASCADE", quoted_schema))?;

    // Remove from metadata table
    Spi::run_with_args(
        "DELETE FROM mentat.stores WHERE store_name = $1",
        &[DatumWithOid::from(store_name)],
    )?;
    Spi::run_with_args(
        "DELETE FROM mentat.cache_generation WHERE store_name = $1",
        &[DatumWithOid::from(store_name)],
    )?;
    crate::cache::invalidate_store_cache(store_name);

    Ok(format!("Store '{}' dropped successfully.", store_name))
}

/// List all registered stores.
///
/// Returns a JSON array of store objects with name, schema, description, and creation time.
///
/// # Example
/// ```sql
/// SELECT mentat_list_stores();
/// ```
#[pg_extern]
pub fn list_stores() -> Result<JsonB, Box<dyn std::error::Error + Send + Sync>> {
    let stores = Spi::connect(|client| {
        let mut result = Vec::new();

        let rows = client.select(
            "SELECT store_name, schema_name, description, created_at::TEXT \
             FROM mentat.stores ORDER BY created_at",
            None,
            &[],
        )?;

        for row in rows {
            let name: String = row.get::<String>(1)?.unwrap_or_default();
            let schema: String = row.get::<String>(2)?.unwrap_or_default();
            let desc: Option<String> = row.get::<String>(3)?;
            let created: String = row.get::<String>(4)?.unwrap_or_default();

            result.push(json!({
                "store_name": name,
                "schema_name": schema,
                "description": desc,
                "created_at": created,
            }));
        }

        Ok::<_, pgrx::spi::SpiError>(result)
    })?;

    Ok(JsonB(json!(stores)))
}

/// Rename an existing store.
///
/// This renames the store in the metadata table and renames its backing PostgreSQL schema.
/// The default store cannot be renamed.
///
/// # Example
/// ```sql
/// SELECT mentat_rename_store('old_store', 'new_store');
/// ```
#[pg_extern]
pub fn rename_store(
    old_name: &str,
    new_name: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    if old_name == "default" {
        return Err(Box::new(MentatError::InvalidStoreName {
            store_name: old_name.to_string(),
            reason: "Cannot rename the default store.".to_string(),
        }));
    }

    validate_store_name(new_name)?;

    // Verify old store exists
    let old_schema = get_store_schema(old_name)?.ok_or_else(|| {
        Box::new(MentatError::StoreNotFound {
            store_name: old_name.to_string(),
        }) as Box<dyn std::error::Error + Send + Sync>
    })?;

    // Check new name doesn't already exist
    if store_exists(new_name)? {
        return Err(Box::new(MentatError::StoreExists {
            store_name: new_name.to_string(),
        }));
    }

    let new_schema = get_schema_for_store(new_name);
    let quoted_old = quote_ident(&old_schema);
    let quoted_new = quote_ident(&new_schema);

    // Rename the PostgreSQL schema
    Spi::run(&format!(
        "ALTER SCHEMA {} RENAME TO {}",
        quoted_old, quoted_new
    ))?;

    // Update metadata table
    Spi::run_with_args(
        "UPDATE mentat.stores SET store_name = $1, schema_name = $2 WHERE store_name = $3",
        &[
            DatumWithOid::from(new_name),
            DatumWithOid::from(new_schema.as_str()),
            DatumWithOid::from(old_name),
        ],
    )?;
    Spi::run_with_args(
        "DELETE FROM mentat.cache_generation WHERE store_name = $1",
        &[DatumWithOid::from(old_name)],
    )?;
    Spi::run_with_args(
        "INSERT INTO mentat.cache_generation (store_name, gen) VALUES ($1, 1) \
         ON CONFLICT (store_name) DO UPDATE SET gen = mentat.cache_generation.gen + 1",
        &[DatumWithOid::from(new_name)],
    )?;
    crate::cache::invalidate_store_cache(old_name);
    crate::cache::invalidate_store_cache(new_name);
    // The virtual-table views and functions find the store's id by name.
    crate::functions::virtual_tables::create_virtual_tables_for_schema(&quoted_new, new_name)?;

    Ok(format!("Store '{}' renamed to '{}'.", old_name, new_name))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_store_name_valid() {
        assert!(validate_store_name("my_store").is_ok());
        assert!(validate_store_name("store1").is_ok());
        assert!(validate_store_name("_private").is_ok());
        assert!(validate_store_name("a").is_ok());
    }

    #[test]
    fn test_validate_store_name_empty() {
        assert!(validate_store_name("").is_err());
    }

    #[test]
    fn test_validate_store_name_too_long() {
        let long_name = "a".repeat(64);
        assert!(validate_store_name(&long_name).is_err());
    }

    #[test]
    fn test_validate_store_name_starts_with_digit() {
        assert!(validate_store_name("1store").is_err());
    }

    #[test]
    fn test_validate_store_name_invalid_chars() {
        assert!(validate_store_name("my-store").is_err());
        assert!(validate_store_name("my store").is_err());
        assert!(validate_store_name("store!").is_err());
    }

    #[test]
    fn test_validate_store_name_reserved() {
        assert!(validate_store_name("default").is_err());
        assert!(validate_store_name("pg_stats").is_err());
        assert!(validate_store_name("information_schema").is_err());
    }

    #[test]
    fn test_quote_ident_simple() {
        assert_eq!(quote_ident("my_store"), "my_store");
        assert_eq!(quote_ident("store1"), "store1");
    }

    #[test]
    fn test_quote_ident_needs_quoting() {
        assert_eq!(quote_ident("My Store"), "\"My Store\"");
        assert_eq!(quote_ident("UPPER"), "\"UPPER\"");
    }

    #[test]
    fn test_quote_ident_escapes_quotes() {
        assert_eq!(quote_ident("has\"quote"), "\"has\"\"quote\"");
    }

    #[test]
    fn test_get_schema_for_store_default() {
        assert_eq!(get_schema_for_store("default"), "mentat");
    }

    #[test]
    fn test_get_schema_for_store_custom() {
        assert_eq!(get_schema_for_store("my_store"), "mentat_my_store");
    }
}
