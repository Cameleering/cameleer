//! # rust-camel SQL
//!
//! SQL support built on the Rust standard library only:
//!
//! | piece | module |
//! |---|---|
//! | DataSource / Connection SPI, registry binding, url factory | [`datasource`] |
//! | embedded in-memory SQL engine `rustsql` (`jdbc:rustsql:mem:<name>`) | [`engine`] |
//! | PostgreSQL wire protocol v3 client (`jdbc:postgresql://...`) | [`pg`] |
//! | `sql` component (producer + polling consumer), `sql-stored` (unsupported) | [`sql`] |
//! | `jdbc` component | [`jdbc`] |
//! | thread-bound transactions and the transaction policy | [`transaction`] |
//! | `JdbcIdempotentRepository`, `JdbcAggregationRepository` | [`repository`] |
//! | embedded PostgreSQL-protocol test server | [`testing`] |
//!
//! ```no_run
//! use camel_core::prelude::*;
//! use camel_sql::{bind_datasource, datasource_from_url};
//!
//! let ctx = CamelContext::new();
//! camel_sql::register(&ctx);
//! bind_datasource(&ctx, "ds", datasource_from_url("jdbc:rustsql:mem:shop").unwrap());
//! ctx.add_routes(|b| {
//!     b.from("direct:new").to("sql:insert into orders (item, qty) values (:#item, :#qty)?dataSource=#ds");
//!     b.from("sql:select * from orders where processed = false?dataSource=#ds&onConsume=update orders set processed = true where id = :#id&delay=1000")
//!         .log("${body}");
//! }).unwrap();
//! ctx.start().unwrap();
//! ```

pub mod datasource;
pub mod engine;
pub mod jdbc;
pub mod params;
pub mod pg;
pub mod repository;
pub mod sql;
pub mod testing;
pub mod transaction;

use std::sync::Arc;

use camel_core::context::CamelContext;
use camel_core::error::Result;
use camel_core::spi::Component;

pub use datasource::{
    bind_datasource, datasource_from_url, datasource_from_url_with, lookup_datasource, sql_error, ColumnType, Connection, DataSource, ExecuteOptions,
    QueryResult,
};
pub use engine::{EmbeddedConnection, EmbeddedDataSource};
pub use pg::{PgConfig, PgConnection, PgDataSource};
pub use repository::{JdbcAggregationRepository, JdbcIdempotentRepository};
pub use transaction::{bind_transaction_policy, run_in_transaction, transaction_policy, with_connection, JdbcTransactionPolicy};

fn add(ctx: &CamelContext, scheme: &'static str, f: impl Fn() -> Arc<dyn Component> + Send + Sync + 'static) {
    if !ctx.has_component(scheme) {
        ctx.add_component_factory(scheme, f);
    }
}

/// Register the `sql`, `jdbc` and `sql-stored` components (lazily created; existing
/// components are left untouched) and the `SQLException` kind hierarchy.
pub fn register(ctx: &CamelContext) {
    datasource::register_sql_kinds();
    add(ctx, "sql", || Arc::new(sql::SqlComponent::new()));
    add(ctx, "jdbc", || Arc::new(jdbc::JdbcComponent::new()));
    add(ctx, "sql-stored", || Arc::new(sql::SqlStoredComponent));
}

/// Create and bind data sources from properties:
///
/// * `camel.datasource.url` (+ `.username`/`.user`, `.password`, `.name` — default
///   `dataSource`) binds one data source,
/// * `camel.datasource.<name>.url` (+ `.username`/`.user`, `.password`) binds `<name>`.
///
/// Dashed and camel-case spellings are the same. Returns an error for an unsupported url.
pub fn configure_datasources_from_properties(ctx: &CamelContext) -> Result<()> {
    let props = ctx.properties().all();
    let get = |k: &str| props.get(k).cloned();
    let mut bound = Vec::new();
    if let Some(url) = get("camel.datasource.url") {
        let user = get("camel.datasource.username").or_else(|| get("camel.datasource.user"));
        let pass = get("camel.datasource.password");
        let name = get("camel.datasource.name").unwrap_or_else(|| "dataSource".to_string());
        let ds = datasource_from_url_with(&ctx.resolve_placeholders(&url)?, user.as_deref(), pass.as_deref())?;
        bind_datasource(ctx, &name, ds);
        bound.push(name);
    }
    for (k, url) in &props {
        let Some(rest) = k.strip_prefix("camel.datasource.") else { continue };
        let Some(name) = rest.strip_suffix(".url") else { continue };
        if name.is_empty() || name.contains('.') {
            continue;
        }
        let user = get(&format!("camel.datasource.{}.username", name)).or_else(|| get(&format!("camel.datasource.{}.user", name)));
        let pass = get(&format!("camel.datasource.{}.password", name));
        let ds = datasource_from_url_with(&ctx.resolve_placeholders(url)?, user.as_deref(), pass.as_deref())?;
        bind_datasource(ctx, name, ds);
        bound.push(name.to_string());
    }
    for n in bound {
        camel_core::camel_info!("camel-sql", "Bound DataSource '{}' from properties", n);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datasources_from_properties() {
        let ctx = CamelContext::new();
        ctx.properties().set("camel.datasource.url", "jdbc:rustsql:mem:propsdefault");
        ctx.properties().set("camel.datasource.orders.url", "jdbc:rustsql:mem:propsorders");
        ctx.properties().set("camel.datasource.pg.url", "jdbc:postgresql://localhost:5432/db");
        ctx.properties().set("camel.datasource.pg.username", "camel");
        configure_datasources_from_properties(&ctx).unwrap();
        assert_eq!(lookup_datasource(&ctx, "dataSource").unwrap().key(), "rustsql:mem:propsdefault");
        assert_eq!(lookup_datasource(&ctx, "#orders").unwrap().key(), "rustsql:mem:propsorders");
        assert!(lookup_datasource(&ctx, "pg").unwrap().description().contains("user=camel"));
        assert!(lookup_datasource(&ctx, "nope").is_err());
        ctx.properties().set("camel.datasource.bad.url", "jdbc:oracle:thin:@x");
        assert!(configure_datasources_from_properties(&ctx).is_err());
    }
}
