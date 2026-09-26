# Changes to the vendored rust-gc v0.5.1

Every modification to the files in `gc/` and `gc_derive/` relative to upstream
tag `v0.5.1`. Those files remain under the MPL-2.0 (`LICENSE`).

## Iterative marking (`gc/src/gc.rs`)

Upstream `GcBox::trace_inner` marked a box and then traced its data, which
called `trace_inner` on each child `Gc`: one Rust frame per `Gc` edge. A
collection while a long chain was alive (every mino list is a chain of cons
cells) overflowed the stack: ~6,000 links on a 2 MB stack in a debug build,
~200,000 nested levels in a release build.

- `GcBoxHeader` gains `this: Cell<Option<NonNull<GcBox<dyn Trace>>>>`, set by
  `insert_gcbox` when the box joins the chain, so a box can be queued without
  knowing its concrete type.
- A thread-local `MARK_STACK` worklist exists only while a mark pass runs.
  `collect_garbage`'s mark phase calls the new `mark_from(root)` for each
  rooted box; `mark_from` traces queued boxes in a loop.
- During a pass, `trace_inner` marks the box and pushes it on the worklist
  instead of tracing into it. Outside a pass it behaves as upstream.

Stack use during a collection is now bounded by the nesting inside a single
box's data, not by the length of the object graph.

## Tests (`gc/tests/deep_chains.rs`, new)

Collect with a 1,000,000-link live chain, collect a dead one, a 500,000-link
chain through `GcCell`s, and a cycle is still freed; all on a 256 KB stack.
Against unmodified v0.5.1 these abort with a stack overflow.
