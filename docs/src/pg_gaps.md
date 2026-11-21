# Probabilistic Inference over Missing Values via pg_gaps

`pg_mentat` integrates with [`pg_gaps`][pggaps], a PostgreSQL extension that
brings BayesStore-style probabilistic graphical models to ordinary tables.
Some values are missing; a first-order Bayesian network stored in pg_gaps'
catalog says how the values are distributed and correlated; inference returns
the distribution of each missing value, or its most likely completion,
conditioned on the data you have.

[pggaps]: https://codeberg.org/gregburd/pg_gaps

Mentat's data fits this directly. An entity whose cardinality-one attribute has
no current value is exactly what pg_gaps calls a missing value: the patient
whose smoking status was never recorded, the order with no shipping region,
the sensor reading that never arrived. pg_mentat builds the table pg_gaps
models, declares mentat attributes into a pg_gaps model by their idents, and
reads the posteriors back in Datalog, joined to the rest of your data.

pg_gaps is an **optional** dependency and needs **PostgreSQL 17 or later**.
Detect it with `mentat.has_pg_gaps()`. `mentat.create_gaps_view` works without
it (on PostgreSQL 15 or later: the view is `security_invoker`);
`mentat.gaps_declare` and the where-fns need it, and say so.

pg_gaps is in early development (0.1.0). This integration uses only its
documented SQL surface: `gaps.create_model`, `gaps.declare_attribute`,
`gaps.add_stripe`, `gaps.add_factor`, `gaps.learn_cpts`, `gaps.validate`,
`gaps.marginals` and `gaps.impute`.

## Quick start

```sh
# Build pg_gaps against your server (see its README for PGXN and Nix).
git clone https://codeberg.org/gregburd/pg_gaps
cd pg_gaps
make PG_CONFIG=/path/to/pg_config && sudo make install PG_CONFIG=/path/to/pg_config
```

```sql
CREATE EXTENSION pg_mentat;
CREATE EXTENSION pg_gaps;          -- installs into schema "gaps"
SET search_path = mentat, public;

SELECT edn_t('[{:db/ident :patient/name   :db/valueType :db.type/string
                :db/cardinality :db.cardinality/one :db/unique :db.unique/identity}
               {:db/ident :patient/smoker :db/valueType :db.type/ref
                :db/cardinality :db.cardinality/one}
               {:db/ident :patient/cancer :db/valueType :db.type/boolean
                :db/cardinality :db.cardinality/one}
               {:db/ident :yn/yes} {:db/ident :yn/no}]');

-- ... transact patients; some have no :patient/smoker or no :patient/cancer.

-- 1. The table pg_gaps models: one row per patient, NULL where a value is missing.
SELECT mentat.create_gaps_view('public.patients',
         ARRAY[':patient/smoker', ':patient/cancer'],
         'default', ':patient/name');

-- 2. A model: smoking status, and cancer given smoking status.
SELECT gaps.create_model('cancer');
SELECT mentat.gaps_declare('cancer', 'public.patients', ':patient/smoker');
SELECT mentat.gaps_declare('cancer', 'public.patients', ':patient/cancer');
SELECT gaps.add_factor('cancer', 'f_smoker', 'patient_smoker');
SELECT gaps.add_factor('cancer', 'f_cancer', 'patient_cancer', ARRAY['patient_smoker']);

-- 3. Learn the probabilities from the patients whose values are known.
SELECT * FROM gaps.learn_cpts('cancer');
SELECT * FROM gaps.validate('cancer');
```

```clojure
;; 4. The distribution of every missing smoking status, by patient name.
[:find ?name ?v ?p
 :where [(gaps-marginals "cancer" :patient/smoker) [[?e ?v ?p]]]
        [?e :patient/name ?name]]
;; => [["p14" ":yn/no" 0.80] ["p14" ":yn/yes" 0.20] ...]

;; The most likely value of each missing cancer diagnosis.
[:find ?e ?v ?p
 :where [(gaps-impute "cancer" :patient/cancer) [[?e ?v ?p]]]]
```

## mentat.create_gaps_view

```sql
mentat.create_gaps_view(view_name text, attrs text[],
                        store text DEFAULT 'default', entities text DEFAULT NULL)
  RETURNS text   -- the view's schema-qualified name
```

Creates (or replaces) a view with column `e`, the entid, and one text column
per attribute in `attrs`. The column for `:patient/smoker` is `patient_smoker`
(`mentat.gaps_column` gives the name: ASCII letters lowered, anything other than
a letter, digit or underscore turned into an underscore; a name over
PostgreSQL's 63 bytes is refused). A column is NULL where the entity has no
current value for that attribute.

**Which entities.** With `entities` set to an attribute ident (such as
`:patient/name` or `:patient/id`), the rows are the entities that have a current
value for it, so an entity missing every one of `attrs` still appears and gets
a posterior for each. With `entities` NULL, the rows are the entities that have
a value for at least one of `attrs`.

**Values.** Columns are text, spelled the way you write the domain:

| Attribute type | Column value |
|---|---|
| `:db.type/ref` | the `:db/ident` of the entity it points to (an enum value, such as `:yn/yes`), or its entid if it has none |
| `:db.type/keyword` | `:ns/name` |
| `:db.type/boolean` | `true` / `false` |
| `:db.type/long` | the number |
| `:db.type/string` | the string |

Each attribute must be **cardinality one** (a pg_gaps attribute has one value
per entity) and of one of those discrete types; doubles, instants, uuids and
bytes are refused (pg_gaps models finite, discrete domains). A cardinality-many
attribute or an unknown ident is an error naming it.

**Always current.** The view reads mentat's current-state tables
(`mentat.current_<type>`), so a model sees each transaction as soon as it
commits. It is `security_invoker`: whoever queries it, including pg_gaps
reading it during inference, needs read access to the store's tables, and
row-level security on a multi-tenant store applies to them. Each column's
`COMMENT` is its ident, which `mentat.gaps_declare` reads; `pg_dump` keeps both.

**Where it goes.** A schema-qualified name puts it there; an unqualified one
goes in the first schema of your `search_path`, as `CREATE VIEW` would. The
name is used as written, case included (`'Public.MyView'` is
`"Public"."MyView"`), so write it in lower case unless you mean otherwise.

**Named stores.** `store` names a mentat store; the attributes must be defined
in it. A model should not mix views of different stores: entids are per store.

## mentat.gaps_declare

```sql
mentat.gaps_declare(model text, view regclass, attr_ident text,
                    domain text[] DEFAULT NULL, stripe text DEFAULT NULL)
  RETURNS text[]   -- the domain declared
```

Declares the view column that holds `attr_ident` as a probabilistic attribute
of the pg_gaps model (`gaps.declare_attribute`, keyed by `e`), and adds a
stripe over all its entities (`gaps.add_stripe`) named `stripe`, by default the
column name, ready for `gaps.add_factor`.

`domain` defaults to the column's distinct current values, sorted. Pass it when
a value may not occur yet, or to fix the order: pg_gaps refuses a value
outside the domain, and CPTs you write by hand (`gaps.set_cpt`) follow the
domain's order. For an enum ref, the domain is the enum idents:
`ARRAY[':yn/no', ':yn/yes']`.

The model must exist (`gaps.create_model`). Factors are yours to declare: which
attributes depend on which is knowledge about your domain, not something
pg_mentat can infer.

## The Datalog where-fns

```clojure
[(gaps-marginals "model" :attr) [[?e ?value ?p ?component]]]
[(gaps-impute    "model" :attr) [[?e ?value ?p ?component]]]
```

- `gaps-marginals` binds, for each missing value of `:attr`, one row per value
  of its domain: the entity `?e`, the value, its posterior probability, and the
  grounded component it belongs to. The probabilities of one entity's values
  sum to 1.
- `gaps-impute` binds one row per missing value: the value in the jointly most
  probable completion of the entity's component, and that completion's
  probability. For a component too large for exact inference that pg_gaps
  sampled (`gaps.samples` set), `?p` is nil and each value is its own most
  probable sampled value: the rows of that component are not one joint
  completion (see pg_gaps' DESIGN, "Gibbs sampling, as built").

`?component` is optional; bind it when you combine rows. `?e` joins to the
rest of the query like any entity variable. `?value` is the view's text
spelling (`":yn/yes"`); `edn_q` decodes results that look like booleans or
numbers, so a boolean attribute's value comes back as a JSON boolean, a long
as a number, and so does a *string* value such as `"007"` or `"true"` (keep
string domains non-numeric if that matters). An entity whose value is present
gets no rows: it is evidence, not a random variable.

`:attr` is the column whose `COMMENT` is the ident, in any view of the model
(`create_gaps_view` writes the comment). A model should hold one store's views
only: entids are per store, and `?e` is joined as an entity of the store the
query runs in. Each call runs pg_gaps inference over the whole model once.

`:find` returns a set, as in any Datalog: `[:find ?v ?p ...]` without `?e`
collapses equal rows of different entities. Keep `?e` in `:find`, or use
`:with ?e` with an aggregate.

**What may be combined.** Values in one grounded component are correlated.
Multiplying two marginals of one component (rows with the same `?component`)
is not their joint probability; pg_gaps' own documentation says which outputs
may be combined in SQL, and the same holds in Datalog. Filtering, sorting and
joining the rows back to entities is safe.

## Notes

- **Consistency.** pg_gaps reads the view in several statements while it
  grounds a model. For one consistent snapshot while others write, run the
  query in a `REPEATABLE READ` transaction.
- **Security.** All of these run as the caller. pg_gaps' own model catalog has
  row-level security per model owner; see pg_gaps' `doc/DESIGN.md`, "Security".
- **Learning publishes statistics.** `gaps.learn_cpts` writes frequencies of
  the rows the caller can read into the model, which anyone who can read the
  model can read.
- **Without pg_gaps**, `gaps_declare` and a Datalog query using `gaps-marginals`
  or `gaps-impute` raise `:db.error/missing-extension`.
