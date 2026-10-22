# Multi-Tenancy: Named Stores and Per-Store Row-Level Security

pg_mentat has two ways to keep several stores in one database:

| | Schema per store (`create_store`) | Rows in `mentat` keyed by `store_id` |
|---|---|---|
| Created by | `SELECT create_store('acme')` | `INSERT INTO mentat.stores (store_name, schema_name) VALUES ('acme', 'mentat')` |
| Data lives in | schema `mentat_acme`: its own narrow logs, projections, `schema`, `idents`, `partitions`, `transactions`, sequences | the shared `mentat.*` tables, rows tagged with the store's `store_id` |
| Attributes | per store: `acme` and `beta` can define `:p/name` differently | per store as rows, but the `mentat.schema` table is shared |
| Entity ids | per store (each store has its own sequences) | shared sequences |
| Isolation | by schema: grant or revoke `USAGE ON SCHEMA mentat_acme` | by Row-Level Security (below) |
| Drop | `drop_store('acme')` drops the schema | delete the rows yourself |
| Use with | `t`, `q`, `pull`, `pull_many`, `entity`, `schema`, `log`, `diff`, `"with"`, `mentat_excise(ids, 'acme')`, `recursive`, `materialize` | the same functions, plus `SET mentat.current_store_id` for RLS |

Choose schema per store unless you need the RLS model: it is the one
`create_store` / `drop_store` / `rename_store` / `list_stores` manage, and
each store's tables stay small.

## Named stores (one schema per store)

```sql
SELECT create_store('acme', 'Acme Corp');
SELECT t('acme', '[{:db/ident :p/name :db/valueType :db.type/string
                    :db/cardinality :db.cardinality/one}]');
SELECT t('acme', '[{:p/name "Ann"}]');
SELECT q('acme', '[:find ?n :where [_ :p/name ?n]]', '{}');
SELECT q('acme', '[:find ?n :where [_ :p/name ?n]]', '{"asOf": 1000000000005}');
SELECT pull('acme', '[*]', 1000001);
SELECT rename_store('acme', 'acme_corp');   -- schema becomes mentat_acme_corp
SELECT list_stores();
SELECT drop_store('acme_corp');             -- drops schema mentat_acme_corp
```

`create_store` builds `mentat_<name>` from the default store's own table
definitions (`mentat._create_store_layout`, sql/30_store_layout.sql), so a
named store has every table, column, constraint and shipped index the
default store has. The store also gets the read-only views
(`mentat_<name>.facts`, `.entities`, `.attributes`, ...) and helper
functions (`lookup_entity`, `entity_value`, `find_text`, ...). The default
store (`mentat`) cannot be dropped or renamed.

The extension-wide functions that have no store argument (`edn_t`, `edn_q`,
`edn_pull`, `edn_eval`, `mentat_tune_indexes`, the `mentat.attach_*`
per-attribute index helpers) work on the default store only.

**Permissions.** The role that calls `create_store` owns the new schema and
everything in it. Every role with a privilege on the default store's
`mentat` schema or its tables gets the same privilege on the new store's,
with the same grant option (`mentat._store_grants`). On a trusted install
that is the installing role, and anyone it granted to. PUBLIC gets nothing.
To give another role access to just one store:

```sql
GRANT USAGE ON SCHEMA mentat_acme TO acme_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA mentat_acme TO acme_app;
GRANT USAGE, UPDATE ON ALL SEQUENCES IN SCHEMA mentat_acme TO acme_app;
-- the functions also read mentat.stores and mentat.cache_generation:
GRANT USAGE ON SCHEMA mentat TO acme_app;
GRANT SELECT ON mentat.stores, mentat.cache_generation TO acme_app;
GRANT UPDATE ON mentat.cache_generation TO acme_app;   -- schema transactions bump it
```

### Upgrading from 1.17

`create_store` used to build the pre-1.5 wide layout, which no release
since 1.5.0 could write to (the first `t()` failed with `relation
"mentat_<name>.current_instant" does not exist`). So a 1.17 named store
holds only rows someone inserted by hand. `ALTER EXTENSION pg_mentat
UPDATE TO '1.18.0'` rebuilds each such store: the old schema is renamed
`mentat_<name>_pre118` and kept, and a new, empty `mentat_<name>` is built
with the store's owner and grants. A WARNING names each store rebuilt
and where its old tables are.

The upgrade copies nothing out of the old schema. It runs as a superuser,
and the old schema belongs to the store's owner, who could have replaced
any table in it with a view; reading it would run that view as superuser.
If the old store holds rows you want, copy them yourself, as the store's
owner (shown for a store named `acme`):

```sql
BEGIN;
INSERT INTO mentat_acme.schema
    SELECT entid, ident, value_type, cardinality, unique_constraint,
           indexed, fulltext, component, no_history
      FROM mentat_acme_pre118.schema ON CONFLICT DO NOTHING;
INSERT INTO mentat_acme.idents (ident, entid)
    SELECT ident, entid FROM mentat_acme_pre118.idents ON CONFLICT DO NOTHING;
INSERT INTO mentat_acme.transactions (tx, tx_instant)
    SELECT tx, tx_instant FROM mentat_acme_pre118.transactions ON CONFLICT DO NOTHING;
-- The store's datoms view routes each row to its narrow log table.
INSERT INTO mentat_acme.datoms (e, a, value_type_tag, v_ref, v_bool, v_long, v_double,
                                v_text, v_keyword, v_instant, v_uuid, v_bytes, tx, added)
    SELECT e, a, value_type_tag, v_ref, v_bool, v_long, v_double,
           v_text, v_keyword, v_instant, v_uuid, v_bytes, tx, added
      FROM mentat_acme_pre118.datoms
     WHERE num_nonnulls(v_ref, v_bool, v_long, v_double, v_text,
                        v_keyword, v_instant, v_uuid, v_bytes) = 1;
-- The current-state projection: per (e, a, v), the latest tx wins.
DO $$ DECLARE ty TEXT; BEGIN
  FOREACH ty IN ARRAY ARRAY['ref', 'boolean', 'long', 'double', 'instant',
                            'text', 'keyword', 'uuid', 'bytes'] LOOP
    EXECUTE format('INSERT INTO mentat_acme.current_%1$s (store_id, e, a, v, tx)
                    SELECT store_id, e, a, v, tx FROM (
                      SELECT DISTINCT ON (e, a, v) store_id, e, a, v, tx, added
                        FROM mentat_acme.datoms_%1$s_new ORDER BY e, a, v, tx DESC) l
                    WHERE added ON CONFLICT DO NOTHING', ty);
  END LOOP; END $$;
-- Never re-issue an id the copied rows use.
SELECT setval('mentat_acme.partition_db_seq', GREATEST(
    (SELECT last_value FROM mentat_acme.partition_db_seq),
    (SELECT max(entid) FROM mentat_acme.idents WHERE entid BETWEEN 100 AND 999999)));
SELECT setval('mentat_acme.partition_user_seq', GREATEST(
    (SELECT last_value FROM mentat_acme.partition_user_seq),
    (SELECT max(e) FROM mentat_acme_pre118.datoms WHERE e BETWEEN 1000001 AND 999999999999)));
SELECT setval('mentat_acme.partition_tx_seq', GREATEST(
    (SELECT last_value FROM mentat_acme.partition_tx_seq),
    (SELECT max(tx) FROM mentat_acme.transactions
      WHERE tx BETWEEN 1000000000000 AND 1999999999999)));
-- Other sessions reload the store's attributes.
UPDATE mentat.cache_generation SET gen = gen + 1 WHERE store_name = 'acme';
COMMIT;
```

Drop `mentat_<name>_pre118` once you have checked the result.

**A store the upgrade skipped.** The upgrade never aborts over a named
store; it leaves one alone, with a WARNING saying why, when its schema does
not hold the old layout, `mentat_<name>_pre118` already exists, or its name
is longer than 56 characters (the limit since 1.18.0: `mentat_` plus the
name must fit PostgreSQL's 63-character identifiers; 1.17 accepted 63 and
PostgreSQL cut the schema name short). It also
leaves alone any `mentat.stores` row that `create_store` would not have
made (its schema is not `mentat_<store_name>`). To rebuild a skipped store
after the upgrade, set its schema aside and create the store again, then
copy as above from the schema you set aside:

```sql
ALTER SCHEMA mentat_acme RENAME TO mentat_acme_old;
DELETE FROM mentat.stores WHERE store_name = 'acme';
SELECT create_store('acme');
```

For a name over 56 characters, create the store under a shorter name and
look up its truncated schema with
`SELECT nspname FROM pg_namespace WHERE nspname LIKE 'mentat\_<first letters>%'`.

## Row-Level Security over `store_id` (shared `mentat` tables)

pg_mentat also ships with optional per-store Row-Level Security (RLS) for the
nine narrow datom tables (`mentat.datoms_<type>_new`). When armed, every
SELECT, INSERT, UPDATE, and DELETE against those tables is automatically
filtered by `store_id = mentat.current_store_id()`, where
`mentat.current_store_id()` reads the session GUC
`mentat.current_store_id`. This gives multi-tenant Postgres deployments
per-tenant isolation enforced by the database itself, not by application
code.

This is one of the genuine differentiators pg_mentat has over Datomic:
isolation lives in the storage layer, not at the application boundary.

### The model

* The narrow datom tables are **shared** across all stores in a single
  Postgres database. Each row carries a `store_id BIGINT` column.
* `mentat.stores` maps `store_name` to `store_id` (`BIGSERIAL`). The
  `default` store is always `store_id = 0`.
* The session-level GUC `mentat.current_store_id` (a placeholder GUC,
  parsed as BIGINT by `mentat.current_store_id()`) tells RLS which
  store the connection is acting on behalf of. Unset, empty, or
  unparseable values fall back to `0` (the default store).
* A second GUC, `mentat.enable_multi_tenant_rls` (boolean, default
  `off`), records operator intent: it is the canonical user-visible
  switch that says "RLS is on for this deployment". Audit tooling and
  the docs treat the GUC as authoritative; the actual enforcement is
  per-table state (next section).

### Opting in

RLS is **off by default** so that single-store deployments pay zero
overhead. Two steps arm it:

1. Register the GUC for the deployment so audit tools can see operator
   intent:

   ```sql
   ALTER SYSTEM SET mentat.enable_multi_tenant_rls = on;
   SELECT pg_reload_conf();
   ```

   Or, for a single session: `SET mentat.enable_multi_tenant_rls = on;`.

2. Arm the per-table RLS state on every database where the extension
   is installed:

   ```sql
   SELECT mentat.enable_multi_tenant_rls(true);
   ```

   This calls `ALTER TABLE ... ENABLE ROW LEVEL SECURITY` on all nine
   narrow tables in one transaction and returns `9`.

To disarm:

```sql
SELECT mentat.enable_multi_tenant_rls(false);  -- DISABLE ROW LEVEL SECURITY on all nine tables
ALTER SYSTEM SET mentat.enable_multi_tenant_rls = off;
SELECT pg_reload_conf();
```

The two-step opt-in is intentional: arming RLS briefly takes
`AccessExclusiveLock` on each narrow table, and we do not want that to
happen implicitly on a session GUC change.

### Per-session usage

Once RLS is armed, every session that wants to read or write datoms
must set its store id:

```sql
SET mentat.current_store_id = '7';      -- pretend to be store_id=7 for this session
```

A common pattern is to bind the GUC to a per-tenant role:

```sql
ALTER ROLE tenant_alice SET mentat.current_store_id = '1';
ALTER ROLE tenant_bob   SET mentat.current_store_id = '2';
```

A connection authenticating as `tenant_alice` then sees only
`store_id = 1` rows, automatically. The application never needs a
`WHERE store_id = ...` clause; forgetting one used to leak data, with
RLS it cannot.

### Threat model

What this protects:

* Cross-tenant **reads** by regular (non-superuser, non-`BYPASSRLS`)
  roles. A query that omits a `WHERE store_id = ...` clause is
  silently filtered.
* Cross-tenant **writes**: the policies have a `WITH CHECK` predicate
  that rejects an `INSERT` or `UPDATE` whose `store_id` does not match
  the session value. A misconfigured ETL job that tags rows with the
  wrong tenant id fails closed instead of silently corrupting another
  tenant's data.

What this does **not** protect against:

* **Superusers** (`rolsuper = t`). PostgreSQL always bypasses RLS for
  superusers; `SET row_security = on` does not change this. Run
  application connections as a non-superuser role.
* **`BYPASSRLS` roles**. Same as superusers; never grant `BYPASSRLS`
  to an application role.
* **The table owner.** The narrow tables are owned by the role that
  owns the extension's objects. For a superuser install that is the superuser
  who ran `CREATE EXTENSION pg_mentat`; for a non-superuser (trusted) install
  it is the bootstrap superuser (see "Installing as a non-superuser" below).
  The owner bypasses RLS: if the application connection role is the owner,
  RLS is silently inert. Run application connections as separate roles with
  explicit grants, or call `ALTER TABLE mentat.datoms_<type>_new FORCE ROW
  LEVEL SECURITY` to subject the owner as well.
* **`SECURITY DEFINER` functions** owned by a privileged role.
  `SECURITY DEFINER` functions execute as their owner, so an unwary
  function created by the superuser is a hole in the wall.
* **Direct file-system access.** Anyone who can read the cluster's
  data directory can read every store's datoms.
* **Tenant-id forgery.** Any role that can `SET
  mentat.current_store_id` can claim to be any tenant. The contract
  is that tenant id is assigned by trusted middleware (typically by
  authenticating to a per-tenant role and using `ALTER ROLE ... SET
  mentat.current_store_id = '<id>'`). A web tier that lets the client
  send the tenant id is its own bug, not one that pg_mentat will
  catch.

### Installing as a non-superuser

pg_mentat is a trusted extension (since 1.17.0; #331). A role with `CREATE`
on the database (for example the database owner) can install it without
being a superuser:

```sql
-- as postgres:
CREATE ROLE app LOGIN;
CREATE DATABASE appdb OWNER app;
-- as app:
CREATE EXTENSION pg_mentat;
```

The install script runs as the bootstrap superuser, which owns the objects.
The installing role gets `USAGE` on schema `mentat` and full privileges on
its tables and sequences, `WITH GRANT OPTION`, so it can grant access to
other roles. PUBLIC gets nothing:

```sql
-- as app: a read-only reporting role
GRANT USAGE ON SCHEMA mentat TO reporting;
GRANT SELECT ON ALL TABLES IN SCHEMA mentat TO reporting;
```

A role with no grants gets `permission denied for schema mentat`, as before.

Things that need the table owner (a superuser) whoever installed it:
`mentat.enable_multi_tenant_rls(...)` and
`mentat_tune_indexes(dry_run => false)`. Both use `ALTER TABLE` or
`CREATE INDEX` on the extension's tables.

**Backups.** The installing role's privileges are part of the extension, not
the dump. Restore a non-superuser's database *as that role*
(`pg_restore -U app -d appdb ...`, into a database it owns). Then
`CREATE EXTENSION` re-grants it, and the grants it gave to other roles
restore too. A superuser restoring it instead must re-run the `GRANT`s for
`app`.

### A worked example

```sql
-- One-time install and arming
CREATE EXTENSION pg_mentat;
SELECT mentat.enable_multi_tenant_rls(true);
ALTER SYSTEM SET mentat.enable_multi_tenant_rls = on;
SELECT pg_reload_conf();

-- Two tenants
INSERT INTO mentat.stores (store_name, schema_name)
VALUES ('alice', 'mentat'), ('bob', 'mentat');

-- One role per tenant, each pinned to its store_id via ALTER ROLE
CREATE ROLE tenant_alice LOGIN PASSWORD 'redacted' NOSUPERUSER NOBYPASSRLS;
CREATE ROLE tenant_bob   LOGIN PASSWORD 'redacted' NOSUPERUSER NOBYPASSRLS;
GRANT USAGE ON SCHEMA mentat TO tenant_alice, tenant_bob;
GRANT SELECT, INSERT, UPDATE, DELETE
   ON mentat.datoms_ref_new, mentat.datoms_long_new, mentat.datoms_text_new,
      mentat.datoms_double_new, mentat.datoms_instant_new,
      mentat.datoms_keyword_new, mentat.datoms_uuid_new,
      mentat.datoms_bytes_new, mentat.datoms_boolean_new
   TO tenant_alice, tenant_bob;

ALTER ROLE tenant_alice
    SET mentat.current_store_id = '1';   -- alice's store_id
ALTER ROLE tenant_bob
    SET mentat.current_store_id = '2';   -- bob's store_id

-- Now alice's session sees only alice's data, automatically:
\c - tenant_alice
SELECT count(*) FROM mentat.datoms_long_new;   -- only alice's rows
INSERT INTO mentat.datoms_long_new (store_id, e, a, v, tx, added)
VALUES (2, 1, 1, 1, 1, true);                  -- ERROR: WITH CHECK violation
```

### Caveats

* The `mentat.datoms` compatibility view is currently hard-coded to
  `store_id = 0`. Code that still reads from the view sees only the
  default store regardless of the session's `mentat.current_store_id`.
  Migrate to the narrow tables (or to `edn_q` / `edn_pull`)
  for multi-store access.
* Schema metadata (`mentat.schema`, `mentat.idents`,
  `mentat.partitions`, `mentat.transactions`) is not yet under RLS.
  In the present model all stores share one schema; tenants can see
  one another's attribute definitions. If your tenants must not even
  share schema, run them in separate databases.
* `mentat.enable_multi_tenant_rls(true)` arms RLS on the `mentat` schema's
  tables only. A store made by `create_store()` is isolated by its schema
  instead (see "Named stores" above); its tables are not under RLS.
