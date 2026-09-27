-- issues whose state was asserted after T_SINCE (the final history file).
SELECT edn_q('[:find ?i :where [?i :issue/state _]]', jsonb_build_object('since', :t_since::bigint));
