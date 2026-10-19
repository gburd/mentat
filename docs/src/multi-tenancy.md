# Multi-Tenancy with Per-Store Row-Level Security

pg_mentat ships with optional per-store Row-Level Security (RLS) for the
nine narrow datom tables (`mentat.datoms_<type>_new`). When armed, every
SELECT, INSERT, UPDATE, and DELETE against those tables is automatically
filtered by `store_id = mentat.current_store_id()`, where
`mentat.current_store_id()` reads the session GUC
`mentat.current_store_id`. This gives multi-tenant Postgres deployments
per-tenant isolation enforced by the database itself, not by application
code.

This is one of the genuine differentiators pg_mentat has over Datomic:
isolation lives in the storage layer, not at the application boundary.

## The model

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

## Opting in

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

## Per-session usage

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

## Threat model

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

## Installing as a non-superuser

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

## A worked example

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

## Caveats

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
* `mentat.enable_multi_tenant_rls(true)` does not (and cannot) attach
  RLS retroactively to per-store schemas created by
  `mentat_create_store()`. Per-store schemas predate the narrow-table
  multi-tenancy model and are gradually being subsumed by it.
