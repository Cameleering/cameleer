//! PostgreSQL support: the wire protocol v3 client ([`PgConnection`], [`PgDataSource`]),
//! protocol helpers and the cryptography needed for MD5 and SCRAM-SHA-256 authentication.
//!
//! Supported: startup, authentication (trust, cleartext, MD5, SCRAM-SHA-256 without channel
//! binding), simple query, extended query (Parse/Bind/Describe/Execute/Sync) with text-format
//! parameters, RowDescription/DataRow decoding by type OID, CommandComplete counts,
//! ErrorResponse → `SQLException` kinds by SQLState, transactions. Not supported: TLS,
//! binary formats, COPY, LISTEN/NOTIFY, cancel requests.

pub mod client;
pub mod crypto;
pub mod protocol;

pub use client::{PgConfig, PgConnection, PgDataSource, PooledConnection};
