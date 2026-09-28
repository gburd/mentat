| scale | fresh process, in-process (LOAD mentat + open + query) ms | fresh process, over Quack (LOAD quack + quack_query) ms | fresh process, SELECT 1 only ms |
|---|---:|---:|---:|
├─────────┼─────────┤
│ 67013   │ User 7  │
└─────────┴─────────┘
├─────────┼─────────┤
│ 67013   │ User 7  │
└─────────┴─────────┘
| s | 17 | 40 | 15 |
├─────────┼─────────┤
│ 67213   │ User 7  │
└─────────┴─────────┘
├─────────┼─────────┤
│ 67213   │ User 7  │
└─────────┴─────────┘
| m | 17 | 40 | 16 |
LOAD quack only: 26 ms
LOAD mentat only: 16 ms
