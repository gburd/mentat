-- q2: issues assigned to a random user.
\set u random(0, :n_users - 1)
SELECT edn_q('[:find ?i ?title ?state :in $ ?email :where [?u :user/email ?email] [?i :issue/assignee ?u] [?i :issue/title ?title] [?i :issue/state ?state]]', jsonb_build_object('inputs', jsonb_build_array('user' || :u::int || '@example.com')));
