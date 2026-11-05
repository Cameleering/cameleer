//! Embedded test servers (in memory, ephemeral ports, optional TLS).
pub use crate::cassandra::server::{CassandraServer, CassandraServerConfig};
pub use crate::couchdb::server::{CouchDbServer, CouchDbServerConfig};
pub use crate::dns::{DnsRecord, DnsServer, SrvRecord};
pub use crate::elasticsearch::server::{ElasticsearchServer, ElasticsearchServerConfig};
pub use crate::mongodb::server::{MongoServer, MongoServerConfig};
pub use crate::redis::server::{RedisServer, RedisServerConfig};
