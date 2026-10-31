//! # camel-datastores
//!
//! Datastore connectors for rust-camel, written from scratch on the standard library
//! (wire protocols included) with embedded test servers:
//!
//! | scheme | module | protocol |
//! |---|---|---|
//! | `spring-redis` | [`redis`] | RESP2 / RESP3, AUTH, SELECT, pipelining, pub/sub, TLS |
//! | `mongodb` | [`mongodb`] | OP_MSG + BSON, SCRAM-SHA-256, TLS, tailable cursors, change streams |
//! | `elasticsearch`, `opensearch` | [`elasticsearch`] | REST/JSON over HTTP(S), basic auth / API key |
//! | `cql` | [`cassandra`] | CQL native protocol v4, PasswordAuthenticator, prepared statements, paging |
//! | `couchdb` | [`couchdb`] | CouchDB REST API, `_changes` feed consumer |
//!
//! Embedded servers (in memory, ephemeral ports) are in [`testing`].
//! Couchbase is not implemented (its binary memcached/KV protocol and cluster map are out of scope).

pub mod cassandra;
pub mod couchdb;
pub mod elasticsearch;
pub mod mongodb;
pub mod redis;
pub mod testing;
pub mod util;

use std::sync::Arc;

use camel_core::context::CamelContext;
use camel_core::spi::Component;

/// Exception kinds raised by the connectors (registered by [`register`]).
pub mod kinds {
    /// Spring `DataAccessException` (parent of the Redis kinds).
    pub const DATA_ACCESS: &str = "DataAccessException";
    pub const REDIS_SYSTEM: &str = "RedisSystemException";
    pub const REDIS_CONNECTION_FAILURE: &str = "RedisConnectionFailureException";
    pub const REDIS_COMMAND_TIMEOUT: &str = "RedisCommandTimeoutException";
    pub const MONGO: &str = "MongoException";
    pub const MONGO_COMMAND: &str = "MongoCommandException";
    pub const MONGO_WRITE: &str = "MongoWriteException";
    pub const MONGO_SECURITY: &str = "MongoSecurityException";
    pub const CAMEL_MONGODB: &str = "CamelMongoDbException";
    pub const ELASTICSEARCH: &str = "ElasticsearchException";
    pub const CASSANDRA_DRIVER: &str = "DriverException";
    pub const CASSANDRA_AUTHENTICATION: &str = "AuthenticationException";
    pub const CASSANDRA_INVALID_QUERY: &str = "InvalidQueryException";
    pub const CASSANDRA_SYNTAX: &str = "SyntaxError";
    pub const COUCHDB: &str = "CouchDbException";
    pub const COUCHDB_CONFLICT: &str = "DocumentConflictException";
    pub const COUCHDB_NOT_FOUND: &str = "NoDocumentException";
}

/// Registers the exception kind hierarchy.
pub fn register_kinds() {
    use camel_core::error::{kinds as core, register_kind_parent};
    use kinds::*;
    register_kind_parent(DATA_ACCESS, core::RUNTIME);
    register_kind_parent(REDIS_SYSTEM, DATA_ACCESS);
    register_kind_parent(REDIS_CONNECTION_FAILURE, DATA_ACCESS);
    register_kind_parent(REDIS_COMMAND_TIMEOUT, DATA_ACCESS);
    register_kind_parent(MONGO, core::RUNTIME);
    register_kind_parent(MONGO_COMMAND, MONGO);
    register_kind_parent(MONGO_WRITE, MONGO);
    register_kind_parent(MONGO_SECURITY, MONGO);
    register_kind_parent(CAMEL_MONGODB, core::CAMEL_EXCHANGE);
    register_kind_parent(ELASTICSEARCH, core::RUNTIME);
    register_kind_parent(CASSANDRA_DRIVER, core::RUNTIME);
    register_kind_parent(CASSANDRA_AUTHENTICATION, CASSANDRA_DRIVER);
    register_kind_parent(CASSANDRA_INVALID_QUERY, CASSANDRA_DRIVER);
    register_kind_parent(CASSANDRA_SYNTAX, CASSANDRA_DRIVER);
    register_kind_parent(COUCHDB, core::RUNTIME);
    register_kind_parent(COUCHDB_CONFLICT, COUCHDB);
    register_kind_parent(COUCHDB_NOT_FOUND, COUCHDB);
}

fn add(ctx: &CamelContext, scheme: &'static str, f: impl Fn() -> Arc<dyn Component> + Send + Sync + 'static) {
    if !ctx.has_component(scheme) {
        ctx.add_component_factory(scheme, f);
    }
}

/// Registers the `spring-redis`, `mongodb`, `elasticsearch`, `opensearch`, `cql` and
/// `couchdb` components (lazily created; existing components are left untouched).
pub fn register(ctx: &CamelContext) {
    register_kinds();
    add(ctx, "spring-redis", || Arc::new(redis::RedisComponent));
    add(ctx, "mongodb", || Arc::new(mongodb::MongoDbComponent));
    add(ctx, "elasticsearch", || Arc::new(elasticsearch::ElasticsearchComponent::new("elasticsearch")));
    add(ctx, "opensearch", || Arc::new(elasticsearch::ElasticsearchComponent::new("opensearch")));
    add(ctx, "cql", || Arc::new(cassandra::CqlComponent));
    add(ctx, "couchdb", || Arc::new(couchdb::CouchDbComponent));
}
