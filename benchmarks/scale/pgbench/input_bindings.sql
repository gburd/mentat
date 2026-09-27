-- :in [?email ...] with 100 random emails.
\set s random(0, :n_users - 1)
SELECT edn_q('[:find ?e ?name :in $ [?email ...] :where [?e :user/email ?email] [?e :user/name ?name]]', jsonb_build_object('inputs', jsonb_build_array((SELECT jsonb_agg('user' || ((:s::int + g * 7919) % :n_users::int) || '@example.com') FROM generate_series(1, 100) g))));
