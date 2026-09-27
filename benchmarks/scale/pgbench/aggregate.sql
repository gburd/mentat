-- q3: issue count by state.
SELECT edn_q('[:find ?state (count ?i) :where [?i :issue/state ?state]]', '{}');
