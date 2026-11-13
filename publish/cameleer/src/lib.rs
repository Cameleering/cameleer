//! `cameleer`: Apache Camel's integration model in Rust.
//!
//! This crate is a thin wrapper of [`cameleer-main`](https://crates.io/crates/cameleer-main), the
//! runner and `cameleer` command line tool. It exists so that `cargo install cameleer` installs
//! the `cameleer` command and `cargo add cameleer` gives the same API as `cameleer-main`, which
//! it re-exports.
//!
//! ```text
//! cargo install cameleer
//! cameleer run hello.camel.yaml
//! ```
//!
//! Documentation: <https://cameleer.ing/docs/>.

pub use cameleer_main::*;
