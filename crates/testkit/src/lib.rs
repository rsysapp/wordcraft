//! embed: test tooling. [`counting::Counting`] is a global allocator that counts the bytes held, so
//! a test can check that code which charges its memory to a budget never holds more than it
//! charged.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod counting;
