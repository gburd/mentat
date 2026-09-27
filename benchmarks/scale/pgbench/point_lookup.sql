-- q1: unique-attr lookup, random user.
\set u random(0, :n_users - 1)
SELECT edn_q('[:find ?e ?name :in $ ?email :where [?e :user/email ?email] [?e :user/name ?name]]', jsonb_build_object('inputs', jsonb_build_array('user' || :u::int || '@example.com')));
