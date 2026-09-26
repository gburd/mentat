-- pg_mentat regression: ground where-function (Phase 3 feature)

-- Ground integer: bind constant to variable
SELECT edn_q(
  '[:find ?name :where [(ground 30) ?age] [?e :person/age ?age] [?e :person/name ?name]]',
  '{}'::jsonb
);

-- Ground string: bind string constant
SELECT edn_q(
  '[:find ?e ?age :where [(ground "Alice") ?name] [?e :person/name ?name] [?e :person/age ?age]]',
  '{}'::jsonb
);

-- Ground in :find only (variable not in pattern value position)
SELECT edn_q(
  '[:find ?name ?label :where [(ground "senior") ?label] [?e :person/name ?name] [?e :person/age ?age] [(>= ?age 30)] :order (asc ?name)]',
  '{}'::jsonb
);
