//! Test entry point (house rule: no inline tests in logic files — every `#[cfg(test)]` unit test
//! lives in this dedicated in-crate module, per CLAUDE.md "Test layout").

mod batch;
mod combinator;
mod corpus;
mod diff;
mod emit;
mod gap;
mod invariant;
mod map;
mod mut_thread;
mod prim_map;
mod remap;
mod reserved;
mod slice_type;
mod symtab;
mod taxonomy;
mod transpile;
mod type_map;
mod valid_ident;
mod vet;
mod write_format;
