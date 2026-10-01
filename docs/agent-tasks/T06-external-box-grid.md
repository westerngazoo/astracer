# T06: Lay out the external box as a compact grid

Size: S · Area: layout · Touches: `crates/lcw-layout/src/grouped.rs`

## Why
In the crate-boxed layout, each crate is a force simulation of its own. External
nodes (std, other dependencies) have no edges among themselves, so their
simulation is pure repulsion against gravity. That spreads them into a disk about
`45·√n` world units across. On this repository that makes 556 externals a box
about 2,100 units tall, a quarter of the diagram, for code the repository does not
own.

## What to do
In `layout_grouped`, use a deterministic square grid instead of `force(...)` for
the group whose `groups.groups[g].external` is true:

- `cols = ceil(sqrt(n))`;
- spacing `params.ideal_length`;
- fill row by row in member order.

Everything else stays as it is: boxing, packing, and the external box placed last.

## Done when
- [ ] A new test in `grouped.rs` builds a graph with 400 external nodes. It asserts
      that the external box is at most `20 * k + 2 * PAD` wide, with
      `k = ideal_length`, and that all 400 positions are distinct. It also asserts
      that every node is still inside the box and below the label band (reuse the
      existing containment test's checks).
- [ ] That test fails if the grid is reverted to `force(...)`.
- [ ] All existing `lcw-layout` tests pass, and `crates_e2e.mjs` passes against
      this repository (21 assertions). Its header says how to run it.

## Verify
`cargo test -p lcw-layout --locked`, then the browser test above.

## Out of scope
Hiding externals; label changes; other groups' layout.
