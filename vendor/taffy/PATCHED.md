# rubrowser changes to taffy 0.15.1

This is [taffy](https://github.com/DioxusLabs/taffy) 0.15.1 (MIT, see
`LICENSE`) as published on crates.io, with one change:

- `src/compute/block.rs`: whether a cleared block's clearance is *forced*
  (a float adjoins the margin strut its top margin collapses into) is decided
  before the block is laid out. Before, floats inside the block itself were
  merged into the flags first, so a cleared block containing only floats got
  clearance against its own floats and was moved up to the last float's
  bottom above it.
