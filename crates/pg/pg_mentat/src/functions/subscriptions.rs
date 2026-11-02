use crate::error::MentatError;
use crate::functions::materialized_views::validate_view_store;
use crate::functions::store_management::{get_schema_for_store, quote_ident};
use pgrx::datum::DatumWithOid;
use pgrx::prelude::*;
use pgrx::JsonB;
use serde_json::json;

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

/// Maximum length for a subscription name: its trigger, mentat_sub_trg_<name>,
/// fits the 63-character identifier limit. Longer names were cut, so two of
/// them on one store shared one trigger; a 57-character one made every
/// write fail (its channel, mentat_<name>, is 64).
const MAX_SUBSCRIPTION_NAME_LEN: usize = 63 - "mentat_sub_trg_".len();

/// Validate a subscription name.
///
/// Rules:
/// - Must not be empty
/// - Must be <= 48 characters (see [`MAX_SUBSCRIPTION_NAME_LEN`])
/// - Must start with a letter or underscore
/// - Must contain only letters, digits, and underscores
fn validate_subscription_name(name: &str) -> Result<(), MentatError> {
    if name.len() > MAX_SUBSCRIPTION_NAME_LEN {
        return Err(MentatError::InvalidQuery {
            message: format!(
                "Subscription name '{}' is {} characters; the limit is {}, so that its \
                 trigger name (mentat_sub_trg_<name>) fits PostgreSQL's 63.",
                name,
                name.len(),
                MAX_SUBSCRIPTION_NAME_LEN
            ),
            suggestion: Some("Choose a shorter subscription name.".to_string()),
        });
    }
    validate_subscription_name_chars(name)
}

/// The character rules alone. unsubscribe() uses these, so a subscription
/// made before 1.18.0 with a longer name can still be removed.
fn validate_subscription_name_chars(name: &str) -> Result<(), MentatError> {
    if name.is_empty() {
        return Err(MentatError::InvalidQuery {
            message: "Subscription name cannot be empty.".to_string(),
            suggestion: None,
        });
    }

    let first = name
        .chars()
        .next()
        .ok_or_else(|| MentatError::InvalidQuery {
            message: "Subscription name cannot be empty.".to_string(),
            suggestion: None,
        })?;

    if !first.is_ascii_alphabetic() && first != '_' {
        return Err(MentatError::InvalidQuery {
            message: "Subscription name must start with a letter or underscore.".to_string(),
            suggestion: None,
        });
    }

    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(MentatError::InvalidQuery {
            message: "Subscription name must contain only letters, digits, and underscores."
                .to_string(),
            suggestion: None,
        });
    }

    Ok(())
}

/// Derive the LISTEN/NOTIFY channel name for a subscription.
fn channel_name(subscription_name: &str) -> String {
    format!("mentat_{}", subscription_name)
}

// ---------------------------------------------------------------------------
// Helper: check if subscription exists
// ---------------------------------------------------------------------------

fn subscription_exists(
    store_name: &str,
    subscription_name: &str,
) -> Result<bool, pgrx::spi::SpiError> {
    Spi::connect(|client| {
        let exists = client
            .select(
                "SELECT 1 FROM mentat.subscriptions WHERE store_name = $1 AND name = $2",
                None,
                &[
                    DatumWithOid::from(store_name),
                    DatumWithOid::from(subscription_name),
                ],
            )?
            .next()
            .is_some();
        Ok(exists)
    })
}

/// The error for a subscription name already in use (ignoring case) by this
/// store or another, or None if it is free.
fn name_taken(
    store_name: &str,
    subscription_name: &str,
) -> Result<Option<Box<dyn std::error::Error + Send + Sync>>, pgrx::spi::SpiError> {
    // One row always (LEFT JOIN): NULLs when the name is free. The exact
    // (store, name) row first, for the "already exists" message.
    let (store, name): (Option<String>, Option<String>) = Spi::get_two_with_args(
        "SELECT s.store_name, s.name FROM (SELECT 1) one LEFT JOIN LATERAL \
           (SELECT store_name, name FROM mentat.subscriptions \
             WHERE pg_catalog.lower(name) = pg_catalog.lower($1) \
             ORDER BY (store_name = $2 AND name = $1) DESC, id LIMIT 1) s ON true",
        &[
            DatumWithOid::from(subscription_name),
            DatumWithOid::from(store_name),
        ],
    )?;
    let (Some(store), Some(name)) = (store, name) else {
        return Ok(None);
    };
    if store == store_name && name == subscription_name {
        return Ok(Some(Box::new(MentatError::InvalidQuery {
            message: format!(
                "Subscription '{}' already exists for store '{}'.",
                subscription_name, store_name
            ),
            suggestion: Some(format!(
                "Use mentat_unsubscribe('{}', '{}') first to remove the existing subscription.",
                store_name, subscription_name
            )),
        })));
    }
    let as_name = if name == subscription_name {
        String::new()
    } else {
        format!(" as '{}'", name)
    };
    Ok(Some(Box::new(MentatError::InvalidQuery {
        message: format!(
            "Subscription name '{}' is already used by store '{}'{}. Subscription names are \
             shared by all stores and ignore case (the channel is {}).",
            subscription_name,
            store,
            as_name,
            channel_name(subscription_name)
        ),
        suggestion: Some("Choose another subscription name.".to_string()),
    })))
}

// ---------------------------------------------------------------------------
// Trigger function SQL generators
// ---------------------------------------------------------------------------

/// `s` as an SQL string literal, written as quote_literal() writes it: E''
/// when it has a backslash, so standard_conforming_strings doesn't matter.
fn sql_literal(s: &str) -> String {
    let doubled = s.replace('\'', "''");
    if s.contains('\\') {
        format!("E'{}'", doubled.replace('\\', "\\\\"))
    } else {
        format!("'{doubled}'")
    }
}

/// A dollar-quote tag that `body` doesn't contain, so no text in the body
/// (the subscribed query) can end it early.
fn dollar_tag(body: &str) -> String {
    (0..)
        .map(|n| format!("$mentat_sub{n}$"))
        .find(|tag| !body.contains(tag.as_str()))
        .expect("some tag is free")
}

/// Generate the SQL for the trigger function that re-evaluates a Datalog query
/// on datom changes and issues NOTIFY if results differ.
///
/// The trigger function:
/// 1. Runs the subscribed Datalog query against the subscribed store, via q()
/// 2. Computes an MD5 hash of the result
/// 3. Compares with the last known hash (stored in a session variable)
/// 4. Sends NOTIFY on the subscription channel if the hash changed
///
/// It runs as the writer, so its search_path is pinned and every call in it
/// is qualified: `q_fn` is pg_mentat's q, named through its schema (#383).
///
/// The payload is the query result, or with `token_only` a token,
/// `{"tx": T, "hash": H}`: the transaction that changed the result (its
/// store's current tx; null if this session hasn't allocated one) and the
/// result's md5. Anyone who can LISTEN gets the payload (#379); a token
/// tells them only that the result changed, and they re-run the query with
/// their own privileges.
fn trigger_function_sql(
    schema: &str,
    subscription_name: &str,
    channel: &str,
    query: &str,
    q_fn: &str,
    token_only: bool,
) -> String {
    let func_name = format!("{}.mentat_sub_{}", schema, subscription_name);

    // The session variable key stores the last known hash for this subscription.
    // We use a custom GUC-like approach with current_setting / set_config.
    let var_name = format!("mentat.sub_hash_{}", subscription_name);

    let token_only = if token_only { "true" } else { "false" };
    let body = format!(
        r"
DECLARE
    sub_store TEXT;
    new_hash TEXT;
    old_hash TEXT;
    result_json JSONB;
    payload TEXT;
    tx_id BIGINT;
BEGIN
    -- The store whose datoms view fired this trigger. Looked up from the
    -- view's schema on every call, not baked in, so rename_store doesn't
    -- break it.
    SELECT s.store_name INTO sub_store FROM mentat.stores s WHERE s.schema_name = TG_TABLE_SCHEMA;
    IF sub_store IS NULL THEN
        RAISE EXCEPTION 'subscription {subscription_name}: no store has schema %', TG_TABLE_SCHEMA;
    END IF;

    -- Re-evaluate the subscribed Datalog query
    SELECT {q_fn}(sub_store, {query_literal}, '{{}}'::jsonb) INTO result_json;

    -- Compute hash of current results
    new_hash := pg_catalog.md5(result_json::TEXT);

    -- Get last known hash from session state (returns empty string if unset)
    BEGIN
        old_hash := pg_catalog.current_setting('{var_name}', true);
    EXCEPTION WHEN OTHERS THEN
        old_hash := '';
    END;

    -- If results changed, notify and update the stored hash
    IF old_hash IS DISTINCT FROM new_hash THEN
        PERFORM pg_catalog.set_config('{var_name}', new_hash, false);
        payload := result_json::TEXT;
        IF {token_only} THEN
            BEGIN
                -- The sequence t() took this transaction's id from
                -- (allocate_tx_id), by the view's schema (rename_store).
                tx_id := pg_catalog.currval(
                    pg_catalog.format('%I.partition_tx_seq', TG_TABLE_SCHEMA)::pg_catalog.regclass);
            EXCEPTION WHEN OTHERS THEN
                tx_id := NULL;
            END;
            payload := pg_catalog.jsonb_build_object('tx', tx_id, 'hash', new_hash);
        END IF;
        PERFORM pg_catalog.pg_notify('{channel}', payload);
    END IF;

    RETURN NULL;
END;
",
        query_literal = sql_literal(query),
    );
    let tag = dollar_tag(&body);

    format!(
        r"
CREATE OR REPLACE FUNCTION {func_name}() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS {tag}{body}{tag};
"
    )
}

/// Generate the SQL for the trigger that fires the subscription function.
fn trigger_sql(schema: &str, subscription_name: &str) -> String {
    let trigger_name = format!("mentat_sub_trg_{}", subscription_name);
    let func_name = format!("{}.mentat_sub_{}", schema, subscription_name);

    format!(
        r"
CREATE OR REPLACE TRIGGER {trigger_name}
    AFTER INSERT OR UPDATE OR DELETE ON {schema}.datoms
    FOR EACH STATEMENT
    EXECUTE FUNCTION {func_name}();
",
        trigger_name = trigger_name,
        schema = schema,
        func_name = func_name,
    )
}

/// Generate SQL to drop the trigger and function for a subscription.
fn drop_trigger_sql(schema: &str, subscription_name: &str) -> String {
    let trigger_name = format!("mentat_sub_trg_{}", subscription_name);
    let func_name = format!("{}.mentat_sub_{}", schema, subscription_name);

    format!(
        r"
DROP TRIGGER IF EXISTS {trigger_name} ON {schema}.datoms;
DROP FUNCTION IF EXISTS {func_name}();
",
        trigger_name = trigger_name,
        schema = schema,
        func_name = func_name,
    )
}

// ---------------------------------------------------------------------------
// Public API functions
// ---------------------------------------------------------------------------

/// Create a subscription that monitors Datalog query results for changes.
///
/// When data in the store's datoms table changes (INSERT, UPDATE, DELETE),
/// the subscribed query is re-evaluated. If the results differ from the
/// previous evaluation, a NOTIFY is sent on channel `mentat_<subscription_name>`
/// with the new query results as the payload.
///
/// Clients can listen for changes using:
/// ```sql
/// LISTEN mentat_my_subscription;
/// ```
///
/// # Arguments
/// - `store_name`: Name of the store to monitor (use "default" for the default store)
/// - `subscription_name`: Unique name for this subscription
/// - `query`: Datalog query string to evaluate on changes
///
/// # Example
/// ```sql
/// SELECT mentat_subscribe('default', 'all_people',
///     '[:find ?name :where [?e :person/name ?name]]');
/// LISTEN mentat_all_people;
/// ```
#[pg_extern]
pub fn subscribe(
    store_name: &str,
    subscription_name: &str,
    query: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    subscribe_impl(store_name, subscription_name, query, false)
}

/// `subscribe(store, name, query, token_only)`: with `token_only` true the
/// NOTIFY payload is `{"tx": T, "hash": H}` instead of the query result, so
/// a listener learns only that the result changed and re-runs the query with
/// its own privileges (any role that can connect can LISTEN, #379). An
/// overload, not a default, so the three-argument form is unchanged.
#[pg_extern(name = "subscribe")]
pub fn subscribe_token_only(
    store_name: &str,
    subscription_name: &str,
    query: &str,
    token_only: bool,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    subscribe_impl(store_name, subscription_name, query, token_only)
}

fn subscribe_impl(
    store_name: &str,
    subscription_name: &str,
    query: &str,
    token_only: bool,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    validate_view_store(store_name)?;
    validate_subscription_name(subscription_name)?;

    if query.trim().is_empty() {
        return Err(Box::new(MentatError::InvalidQuery {
            message: "Subscription query cannot be empty.".to_string(),
            suggestion: None,
        }));
    }

    // The channel (mentat_<name>) and the hash setting carry no store, and
    // the trigger and function names fold to lower case, so a name is one per
    // database, ignoring case: a second store reusing it would NOTIFY the
    // first store's listeners with its results.
    if let Some(err) = name_taken(store_name, subscription_name)? {
        return Err(err);
    }
    // Register first, before any DDL. The unique index on lower(name) makes a
    // concurrent subscribe() of the same name wait for this one, then insert
    // nothing; the check above could not see its uncommitted row.
    let inserted: i64 = Spi::get_one_with_args(
        "WITH ins AS (INSERT INTO mentat.subscriptions (store_name, name, query) \
                      VALUES ($1, $2, $3) ON CONFLICT DO NOTHING RETURNING 1) \
         SELECT count(*) FROM ins",
        &[
            DatumWithOid::from(store_name),
            DatumWithOid::from(subscription_name),
            DatumWithOid::from(query),
        ],
    )?
    .unwrap_or(0);
    if inserted == 0 {
        return Err(
            name_taken(store_name, subscription_name)?.unwrap_or_else(|| {
                Box::new(MentatError::InvalidQuery {
                    message: format!(
                        "Subscription name '{}' was just taken by a concurrent transaction.",
                        subscription_name
                    ),
                    suggestion: Some("Choose another subscription name.".to_string()),
                })
            }),
        );
    }

    let schema = get_schema_for_store(store_name);
    let quoted_schema = quote_ident(&schema);
    let channel = channel_name(subscription_name);

    // Create the trigger function
    let q_fn = crate::functions::query::ext_qualified("pg_mentat", "q");
    let func_sql = trigger_function_sql(
        &quoted_schema,
        subscription_name,
        &channel,
        query,
        &q_fn,
        token_only,
    );
    Spi::run(&func_sql)?;

    // Create the trigger on the datoms table.
    // The datoms table may be partitioned; for the default store the trigger
    // goes on mentat.datoms (the parent). PostgreSQL propagates statement-level
    // triggers to partitions automatically when using AFTER ... FOR EACH STATEMENT.
    let trg_sql = trigger_sql(&quoted_schema, subscription_name);
    Spi::run(&trg_sql)?;

    // Quoted when it has capitals: an unquoted LISTEN folds to lower case,
    // and pg_notify's channel does not.
    Ok(format!(
        "Subscription '{}' created. LISTEN {} to receive notifications.",
        subscription_name,
        quote_ident(&channel)
    ))
}

/// Remove a subscription, dropping its trigger and function.
///
/// After unsubscribing, no further NOTIFY messages will be sent for this
/// subscription. Existing LISTEN connections on the channel are not affected
/// but will stop receiving new messages.
///
/// # Arguments
/// - `store_name`: Name of the store the subscription belongs to
/// - `subscription_name`: Name of the subscription to remove
///
/// # Example
/// ```sql
/// SELECT mentat_unsubscribe('default', 'all_people');
/// ```
#[pg_extern]
pub fn unsubscribe(
    store_name: &str,
    subscription_name: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    validate_view_store(store_name)?;
    validate_subscription_name_chars(subscription_name)?;

    // Verify subscription exists
    if !subscription_exists(store_name, subscription_name)? {
        return Err(Box::new(MentatError::InvalidQuery {
            message: format!(
                "Subscription '{}' not found for store '{}'.",
                subscription_name, store_name
            ),
            suggestion: None,
        }));
    }

    let schema = get_schema_for_store(store_name);
    let quoted_schema = quote_ident(&schema);

    // Drop trigger and function
    let drop_sql = drop_trigger_sql(&quoted_schema, subscription_name);
    Spi::run(&drop_sql)?;

    // Remove from metadata table
    Spi::run_with_args(
        "DELETE FROM mentat.subscriptions WHERE store_name = $1 AND name = $2",
        &[
            DatumWithOid::from(store_name),
            DatumWithOid::from(subscription_name),
        ],
    )?;

    Ok(format!(
        "Subscription '{}' removed from store '{}'.",
        subscription_name, store_name
    ))
}

/// List all active subscriptions, optionally filtered by store.
///
/// Returns a JSON array of subscription objects with store name, subscription
/// name, query, and creation time.
///
/// # Example
/// ```sql
/// SELECT mentat_list_subscriptions();
/// SELECT mentat_list_subscriptions('default');
/// ```
#[pg_extern]
pub fn list_subscriptions(
    store_name: default!(Option<&str>, "NULL"),
) -> Result<JsonB, Box<dyn std::error::Error + Send + Sync>> {
    let subscriptions = Spi::connect(|client| {
        let mut result = Vec::new();

        let rows = match store_name {
            Some(name) => client.select(
                "SELECT store_name, name, query, created_at::TEXT \
                 FROM mentat.subscriptions WHERE store_name = $1 ORDER BY created_at",
                None,
                &[DatumWithOid::from(name)],
            )?,
            None => client.select(
                "SELECT store_name, name, query, created_at::TEXT \
                 FROM mentat.subscriptions ORDER BY created_at",
                None,
                &[],
            )?,
        };

        for row in rows {
            let sname: String = row.get::<String>(1)?.unwrap_or_default();
            let sub_name: String = row.get::<String>(2)?.unwrap_or_default();
            let query_str: String = row.get::<String>(3)?.unwrap_or_default();
            let created: String = row.get::<String>(4)?.unwrap_or_default();

            result.push(json!({
                "store_name": sname,
                "name": sub_name,
                "query": query_str,
                "channel": channel_name(&sub_name),
                "created_at": created,
            }));
        }

        Ok::<_, pgrx::spi::SpiError>(result)
    })?;

    Ok(JsonB(json!(subscriptions)))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_subscription_name_valid() {
        assert!(validate_subscription_name("my_sub").is_ok());
        assert!(validate_subscription_name("sub1").is_ok());
        assert!(validate_subscription_name("_private").is_ok());
        assert!(validate_subscription_name("a").is_ok());
    }

    #[test]
    fn test_validate_subscription_name_empty() {
        assert!(validate_subscription_name("").is_err());
    }

    #[test]
    fn test_validate_subscription_name_too_long() {
        assert_eq!(MAX_SUBSCRIPTION_NAME_LEN, 48);
        assert!(validate_subscription_name(&"a".repeat(48)).is_ok());
        assert_eq!(format!("mentat_sub_trg_{}", "a".repeat(48)).len(), 63);
        for n in [49, 57, 58, 59] {
            let e = validate_subscription_name(&"a".repeat(n)).unwrap_err();
            assert!(e.to_string().contains("the limit is 48"), "{n}: {e}");
        }
    }

    #[test]
    fn test_validate_subscription_name_starts_with_digit() {
        assert!(validate_subscription_name("1sub").is_err());
    }

    #[test]
    fn test_validate_subscription_name_invalid_chars() {
        assert!(validate_subscription_name("my-sub").is_err());
        assert!(validate_subscription_name("my sub").is_err());
        assert!(validate_subscription_name("sub!").is_err());
    }

    #[test]
    fn test_channel_name() {
        assert_eq!(channel_name("all_people"), "mentat_all_people");
        assert_eq!(channel_name("my_sub"), "mentat_my_sub");
    }

    #[test]
    fn test_trigger_function_sql_contains_key_elements() {
        let sql = trigger_function_sql(
            "mentat",
            "test_sub",
            "mentat_test_sub",
            "[:find ?e :where [?e :person/name]]",
            "public.q",
            false,
        );
        assert!(sql.contains("mentat.mentat_sub_test_sub"));
        assert!(sql.contains("q(sub_store, '[:find"));
        assert!(sql.contains("pg_notify"));
        assert!(sql.contains("mentat_test_sub"));
        assert!(sql.contains("[:find ?e :where [?e :person/name]]"));
    }

    #[test]
    fn test_trigger_function_sql_escapes_quotes() {
        let sql = trigger_function_sql(
            "mentat",
            "test_sub",
            "mentat_test_sub",
            "[:find ?name :where [?e :person/name ?name] [(= ?name \"O'Brien\")]]",
            "public.q",
            false,
        );
        // Single quotes are doubled once for the q('...') string
        // literal embedded inside the $$-quoted function body. The function
        // body is dollar-quoted, so the quotes inside it are literal -- there
        // is no second nesting level, hence one level of doubling (O''Brien),
        // not two (O''''Brien).
        assert!(sql.contains("O''Brien"));
    }

    // The trigger fires on the subscribed store's datoms view; it must query
    // that store, not the default one (edn_q), and find it by the view's
    // schema at run time: a store name baked in breaks on rename_store.
    #[test]
    fn test_trigger_function_sql_queries_its_store() {
        let sql = trigger_function_sql(
            "mentat_acme",
            "s",
            "mentat_s",
            "[:find ?n :where [_ :p/name ?n]]",
            "public.q",
            false,
        );
        assert!(sql.contains("q(sub_store, '[:find ?n"), "{sql}");
        assert!(sql.contains("schema_name = TG_TABLE_SCHEMA"), "{sql}");
        assert!(!sql.contains("acme'"), "{sql}");
        assert!(!sql.contains("edn_q"), "{sql}");
    }

    // The function runs as the writer: a pinned path, every call qualified,
    // so a function of the same name earlier on the writer's path isn't the
    // one called (#383).
    #[test]
    fn test_trigger_function_sql_pins_the_path() {
        let sql = trigger_function_sql(
            "mentat",
            "s",
            "mentat_s",
            "[:find ?e]",
            "\"my ext\".q",
            false,
        );
        assert!(
            sql.contains("SET search_path = pg_catalog, pg_temp"),
            "{sql}"
        );
        assert!(
            sql.contains("\"my ext\".q(sub_store, '[:find ?e]'"),
            "{sql}"
        );
        for f in ["md5", "current_setting", "set_config", "pg_notify"] {
            assert!(sql.contains(&format!("pg_catalog.{f}(")), "{f}: {sql}");
            assert!(!sql.contains(&format!(" {f}(")), "{f}: {sql}");
        }
    }

    // The body was $$-quoted with only ' doubled: a query containing $$
    // ended it early (#383).
    #[test]
    fn test_trigger_function_sql_quotes_any_query() {
        let query = "[:find ?e :where [?e :p/name \"$$ $mentat_sub0$ \\ '\"]]";
        let sql = trigger_function_sql("mentat", "s", "mentat_s", query, "public.q", false);
        assert!(sql.contains("AS $mentat_sub1$"), "{sql}");
        assert_eq!(sql.matches("$mentat_sub1$").count(), 2, "{sql}");
        assert!(
            sql.contains("E'[:find ?e :where [?e :p/name \"$$ $mentat_sub0$ \\\\ ''\"]]'"),
            "{sql}"
        );
        assert_eq!(sql_literal("it's"), "'it''s'");
        assert_eq!(sql_literal("a\\b"), "E'a\\\\b'");
    }

    // Token mode (#379).
    #[test]
    fn test_trigger_function_sql_token_payload() {
        let res = trigger_function_sql("mentat", "s", "mentat_s", "[:find ?e]", "public.q", false);
        let tok = trigger_function_sql("mentat", "s", "mentat_s", "[:find ?e]", "public.q", true);
        assert!(res.contains("IF false THEN"), "{res}");
        assert!(tok.contains("IF true THEN"), "{tok}");
        for sql in [&res, &tok] {
            assert!(
                sql.contains("jsonb_build_object('tx', tx_id, 'hash', new_hash)"),
                "{sql}"
            );
            assert!(
                sql.contains("pg_catalog.pg_notify('mentat_s', payload)"),
                "{sql}"
            );
        }
    }

    #[test]
    fn test_trigger_sql_format() {
        let sql = trigger_sql("mentat", "test_sub");
        assert!(sql.contains("mentat_sub_trg_test_sub"));
        assert!(sql.contains("AFTER INSERT OR UPDATE OR DELETE"));
        assert!(sql.contains("FOR EACH STATEMENT"));
        assert!(sql.contains("mentat.datoms"));
    }

    #[test]
    fn test_drop_trigger_sql_format() {
        let sql = drop_trigger_sql("mentat", "test_sub");
        assert!(sql.contains("DROP TRIGGER IF EXISTS mentat_sub_trg_test_sub"));
        assert!(sql.contains("DROP FUNCTION IF EXISTS mentat.mentat_sub_test_sub"));
    }
}
