# Changelog — mino-rs

## Unreleased

### Fixed
- **Tail calls no longer grow the stack.** A call to a fn in tail position
  now returns an internal `TailCall` signal that the caller's apply loop runs,
  as upstream mino does (`MINO_TAIL_CALL`). Before, only `recur` did this, so
  `dotimes`, `while` (both expand to a named fn calling itself last) and any
  self- or mutually-recursive fn used one Rust stack frame and one depth level
  per iteration: `(dotimes [i 60] i)` already hit a depth limit of 300, and
  without limits large counts overflowed the stack and aborted the process.
  Now `(dotimes [i 1000000] ...)`, `(while ...)` over 100,000 iterations and a
  1,000,000-deep self tail call run at constant depth on a 1 MB stack. Tail
  calls never cross a `try` (the body is fully run inside the frame, so
  `catch` still sees the throw), and non-tail recursion such as
  `(inc (f (dec n)))` is still bounded by the depth limit.

- **Garbage collection no longer overflows the stack on long or deeply
  nested data.** mino now uses a vendored copy of rust-gc v0.5.1
  (`vendor/`, MPL-2.0) whose marking uses a worklist instead of recursing
  once per `Gc` edge; see `vendor/CHANGES.md`. Before, collecting while a
  list of ~6,000 elements was alive aborted a debug build on a 2 MB stack
  (so `(count (range 10000))` crashed), and live data nested ~200,000 deep
  aborted release builds. Now both run a million elements / 300,000 levels
  on a 2 MB stack.

### Added
- **`Interpreter::sandboxed()`**: an interpreter with no host access.
  - *Before:* every interpreter bound `slurp`, `spit`, `rm-rf`, `mkdir-p` and
    `file-exists?`, so any script could read, write or delete files as the
    host process.
  - *Now:* those five prims are unbound in a sandboxed interpreter; calling
    one gives `unbound symbol`. Durable `mino.store` ops (`store-open*` with a
    path, `store-read-snapshot*`, `store-read-wal*`, and
    commit/checkpoint/close on a pathed store) throw `:eval/contract`
    "... disabled in a sandboxed interpreter". In-memory `(mino.store/open)`
    still works. `Interpreter::new()` keeps full host access.
- **Print capture**: in a sandboxed interpreter `print`/`println`/`prn`
  append to a buffer (charged to the heap budget) instead of writing stdout.
  `take_output()` returns the buffer and clears it.
  - *Before:* script output went straight to the host's stdout (in pg_mentat
    that is the postmaster log).
- **`Limits { steps, heap_bytes, depth }` + `set_limits`**. All default to
  `None` (unlimited). The budget resets at each top-level eval.
  - `steps`: one per `eval`, plus one per element produced by a bulk prim.
    *Before:* `(loop [] (recur))` hung the host forever. *Now:* it fails with
    `:eval/limit {:limit :steps}`.
  - `heap_bytes`: bulk producers charge their output size *before*
    allocating. *Before:* `(range 100000000000)` tried to allocate ~3 TB and
    OOM-killed the process. *Now:* it fails in microseconds with
    `:eval/limit {:limit :heap}`. Charged: `range`, `repeat`, `vec`, `set`,
    `into`, `concat`, `map`, `mapv`, `filterv`, `apply` (the spread seq),
    `reverse`, `sort`, `sort-by`, `str`, `clojure.string/join`,
    `clojure.string/replace`/`replace-first` (string and regex), print
    capture.
  - `depth`: every `eval` and fn application is one level (a `recur` loop
    stays at one level). *Before:*
    `(defn f [n] (if (zero? n) 0 (inc (f (dec n))))) (f 1000000)` overflowed
    the Rust stack and aborted the whole process (SIGABRT, no unwinding).
    *Now:* it fails with `:eval/limit {:limit :depth}`. The `Limits` doc
    comment has measured stack cost per level and recommended values.
- **`set_check_hook`**: a host callback run every 4096 eval steps and each
  time depth crosses a multiple of 64. If it returns `Err`, the eval aborts
  with that message (for cancellation or wall-clock timeouts).
- A limit error is the diagnostic map
  `{:mino/kind :eval/limit, :mino/code "MLM001", :mino/message "step limit exceeded" | "heap limit exceeded" | "depth limit exceeded" | <hook message>, :mino/data {:limit :steps|:heap|:depth|:hook, :value N}}`
  (kind and code match upstream mino). It is **uncatchable**: once one trips,
  `try` re-raises it without running `catch` or `finally`, and every later
  eval step re-raises it until the top-level eval returns.

### Changed
- `repeat` is now a native prim. *Before:* core.clj's `repeat` recursed once
  per element through the eager `lazy-seq`, so `(repeat 20000 "a")`
  overflowed the stack. *Now:* it uses O(1) stack and is charged up front.
  Infinite `(repeat x)` still needs lazy seqs and throws.
- The recursive eval path was reshaped to use less stack. Special forms,
  macro expansion, collection literals and non-fn callables now run out of
  line. A debug-build mino call level dropped from ~25 KB to ~7.8 KB of
  stack, so 500 depth levels fit in a 1 MB debug thread stack.
- The mentat scripting layer (`src/script.rs`) now runs sandboxed with
  `steps 10_000_000`, `heap 64 MiB`, `depth 1000`.

### Known issues
- Deeply nested *data* still recurses without a depth check in the reader,
  printer, `=`/hash and GC drop, so a 100k-deep nested vector can still
  overflow the stack. This is a separate task.
- The heap budget counts cumulative bytes charged by the bulk prims listed
  above. It is not a live-heap measurement. Small allocations (one `conj`,
  one closure) count only as steps.
