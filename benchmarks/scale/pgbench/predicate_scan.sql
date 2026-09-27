-- q4: open issues with priority >= 4.
SELECT edn_q('[:find ?i ?title ?priority :in $ ?min :where [?i :issue/state :state/open] [?i :issue/priority ?priority] [?i :issue/title ?title] [(>= ?priority ?min)]]', '{"inputs":[4]}');
