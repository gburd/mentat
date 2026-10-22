# Worked example: a manually ordered list

Datoms are a set: Mentat keeps no order between the values of a
cardinality-many attribute. When the order is the user's (a playlist, a
to-do list someone drags items around in), store it: give each item a
position, and keep the positions consistent with a transaction function, the
way Datomic users do.

A transaction function runs inside the transaction, against the database as
of the start of that transaction, so two clients moving items at once can't
interleave their reads and writes. The functions below work on every backend
with the scripting interpreter: the embedded store built with the `mino`
feature, pg_mentat built with `script`, and the SQLite and DuckDB extensions.
See [Transaction functions](./datomic-compat.md#transaction-functions) for
the sandbox and limits.

This page is tested: `crates/sqlite/mentat/tests/ordered_list_doc.rs` runs
each `edn` block below, in order, on the embedded engine, and checks the
results shown after the queries and the failing transaction.

## Schema

A list has items; an item has a name and a position (0, 1, 2, ...).

```edn
[{:db/ident :list/name     :db/valueType :db.type/string :db/cardinality :db.cardinality/one
  :db/unique :db.unique/identity :db/index true}
 {:db/ident :list/items    :db/valueType :db.type/ref    :db/cardinality :db.cardinality/many}
 {:db/ident :item/name     :db/valueType :db.type/string :db/cardinality :db.cardinality/one
  :db/unique :db.unique/identity :db/index true}
 {:db/ident :item/position :db/valueType :db.type/long   :db/cardinality :db.cardinality/one}]
```

## The transaction functions

`:list/insert` adds a new item, by name, at a position in a list, moving the
items at that position and after it down by one. `:list/move` moves an item
already in the list. Both read the list's items in their current order and
assert a position only for items whose position changes, so a move near the
end of a long list writes a few datoms, not the whole list.

```edn
[{:db/ident :list/insert
  :db/fn "(fn [db list name pos]
            (let [le   (ffirst (mentat.store/q db
                         '[:find ?l :in $ ?n :where [?l :list/name ?n]] list))
                  rows (sort-by second (mentat.store/q db
                         '[:find ?i ?p :in $ ?n
                           :where [?l :list/name ?n] [?l :list/items ?i] [?i :item/position ?p]]
                         list))
                  ids  (mapv first rows)
                  old  (into {} rows)
                  pos  (max 0 (min pos (count ids)))
                  item (str \"new item \" name)]
              (concat [{:db/id item :item/name name}
                       [:db/add le :list/items item]]
                      (keep-indexed (fn [i e]
                                      (when (not= (get old e) i)
                                        [:db/add e :item/position i]))
                                    (concat (take pos ids) [item] (drop pos ids))))))"}
 {:db/ident :list/move
  :db/fn "(fn [db list name pos]
            (let [rows (sort-by second (mentat.store/q db
                         '[:find ?i ?p ?n :in $ ?l
                           :where [?le :list/name ?l] [?le :list/items ?i]
                                  [?i :item/position ?p] [?i :item/name ?n]]
                         list))
                  item (ffirst (filter (fn [r] (= (nth r 2) name)) rows))
                  _    (when (nil? item)
                         (throw (ex-info (str name \" is not in \" list) {})))
                  old  (into {} (map (fn [r] [(first r) (second r)]) rows))
                  ids  (vec (remove (fn [e] (= e item)) (map first rows)))
                  pos  (max 0 (min pos (count ids)))]
              (keep-indexed (fn [i e]
                              (when (not= (get old e) i)
                                [:db/add e :item/position i]))
                            (concat (take pos ids) [item] (drop pos ids)))))"}]
```

Each function reads the list with a query that takes the list's name as an
`:in` input. `:list/insert` names the new item with a tempid it makes up
(`"new item milk"`); the transactor resolves it like any other tempid, and
because `:item/name` is `:db.unique/identity`, inserting a name that already
exists reuses that item. A function that throws aborts the whole
transaction.

## Using them

Make a list and add two items. Each call is a transaction of its own:

```edn
[{:list/name "groceries"}]
```

```edn
[[:list/insert "groceries" "milk" 0]]
```

```edn
[[:list/insert "groceries" "eggs" 1]]
```

Insert at the front: `bread` goes to position 0, and the function moves
`milk` and `eggs` to 1 and 2.

```edn
[[:list/insert "groceries" "bread" 0]]
```

Read the list in order:

```edn
[:find ?n ?p
 :order ?p
 :where [?l :list/name "groceries"] [?l :list/items ?i]
        [?i :item/name ?n] [?i :item/position ?p]]
```

```
"bread" 0
"milk" 1
"eggs" 2
```

Move `eggs` to the front. The function writes the three positions that
change (and the transactor retracts the three old ones):

```edn
[[:list/move "groceries" "eggs" 0]]
```

The list now reads:

```edn
[:find ?n ?p
 :order ?p
 :where [?l :list/name "groceries"] [?l :list/items ?i]
        [?i :item/name ?n] [?i :item/position ?p]]
```

```
"eggs" 0
"bread" 1
"milk" 2
```

Moving an item that isn't in the list aborts the transaction:

```edn
[[:list/move "groceries" "butter" 0]]
```

```
error: butter is not in groceries
```

## Notes

- The function reads the database as of the start of its own transaction,
  and the transaction applies its writes atomically, so two clients moving
  items at once can't mix their reads and writes. If a client instead
  computes positions itself and transacts them, guard each write with
  `:db/cas`.
- Positions are dense (0..n-1). That keeps the reads simple, and a move
  rewrites the items between the old and new position. For long lists with
  frequent moves, sparse positions (1000, 2000, ...) let most inserts write
  one datom; the functions then need a branch that renumbers when a gap
  fills up.
- Removing an item is `[:db/retract list-entity :list/items item]` plus
  renumbering the rest, written the same way as `:list/move`.
