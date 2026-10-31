//! Shared producer test-suite, run against the embedded engine and the PostgreSQL server.
#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use camel_core::prelude::*;
use camel_sql::{bind_datasource, DataSource};

static N: AtomicUsize = AtomicUsize::new(0);

/// A unique name for a test database.
pub fn unique(prefix: &str) -> String {
    format!("{}_{}_{}", prefix, std::process::id(), N.fetch_add(1, Ordering::SeqCst))
}

/// A context with the sql components and `ds` bound.
pub fn context_with(ds: Arc<dyn DataSource>) -> CamelContext {
    let ctx = CamelContext::new();
    camel_sql::register(&ctx);
    bind_datasource(&ctx, "ds", ds);
    ctx
}

pub fn exec(ds: &Arc<dyn DataSource>, sql: &str) -> camel_sql::QueryResult {
    ds.get_connection().unwrap().execute(sql, &[]).unwrap_or_else(|e| panic!("{}: {}", sql, e))
}

pub fn count(ds: &Arc<dyn DataSource>, sql: &str) -> i64 {
    exec(ds, sql).scalar().and_then(|v| v.as_i64()).unwrap_or(-1)
}

pub fn obj(pairs: &[(&str, Value)]) -> Value {
    Value::object(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())))
}

/// Producer behaviour shared by both back ends. `ctx` must have `ds` bound and not be started.
pub fn producer_suite(ctx: &CamelContext, ds: &Arc<dyn DataSource>) {
    exec(ds, "CREATE TABLE products (id SERIAL PRIMARY KEY, name VARCHAR(50) NOT NULL UNIQUE, price DOUBLE PRECISION, stock INTEGER DEFAULT 0, active BOOLEAN DEFAULT true)");
    ctx.add_routes(|b| {
        b.from("direct:insert").to("sql:insert into products (name, price, stock) values (:#name, :#price, :#stock)?dataSource=#ds").to("mock:inserted");
        b.from("direct:positional").to("sql:insert into products (name, price) values (#, #)?dataSource=#ds");
        b.from("direct:list").to("sql:select name, price from products where price >= :#min order by price?dataSource=#ds");
        b.from("direct:one").to("sql:select name, stock from products where name = :#${header.wanted}?dataSource=#ds&outputType=SelectOne");
        b.from("direct:scalar").to("sql:select count(*) from products?dataSource=#ds&outputType=SelectOne");
        b.from("direct:stream").to("sql:select name from products order by id?dataSource=#ds&outputType=StreamList");
        b.from("direct:header").to("sql:select name from products order by id?dataSource=#ds&outputHeader=names");
        b.from("direct:noop").to("sql:select name from products?dataSource=#ds&noop=true");
        b.from("direct:in").to("sql:select name from products where id in (:#in:ids) order by id?dataSource=#ds");
        b.from("direct:update").to("sql:update products set stock = stock + :#delta where price < :#max?dataSource=#ds");
        b.from("direct:batch").to("sql:insert into products (name, price) values (:#name, :#price)?dataSource=#ds&batch=true");
        b.from("direct:body-sql").to("sql:ignored?dataSource=#ds&useMessageBodyForSql=true");
        b.from("direct:bad").to("sql:select * from no_such_table?dataSource=#ds");
        b.from("direct:catch")
            .do_try()
            .to("sql:insert into products (name) values (:#name)?dataSource=#ds")
            .set_body(constant("inserted"))
            .do_catch(&["SQLIntegrityConstraintViolationException"])
            .set_body(constant("duplicate"))
            .end();
        b.from("direct:jdbc").to("jdbc:ds");
        b.from("direct:jdbc-one").to("jdbc:ds?outputType=SelectOne");
        b.from("direct:jdbc-params").to("jdbc:ds?useHeadersAsParameters=true&readSize=2");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();

    // insert with named parameters from the body map
    let mock = ctx.mock_endpoint("mock:inserted").unwrap();
    mock.expected_message_count(3);
    for (n, p, s) in [("apple", 1.5, 10), ("pear", 2.25, 5), ("kiwi", 0.5, 0)] {
        t.send_body("direct:insert", obj(&[("name", Value::from(n)), ("price", Value::Float(p)), ("stock", Value::Int(s))])).unwrap();
    }
    mock.assert_is_satisfied().unwrap();
    let ex = &mock.received_exchanges()[0];
    assert_eq!(ex.header("CamelSqlUpdateCount"), Some(&Value::Int(1)));

    // named parameters from headers + generated keys
    let ex = t
        .request("direct:insert", |ex| {
            ex.set_header("name", "melon");
            ex.set_header("price", 3.0);
            ex.set_header("stock", 2);
            ex.set_header("CamelSqlRetrieveGeneratedKeys", true);
        })
        .unwrap();
    assert_eq!(ex.header("CamelSqlGeneratedKeysRowCount"), Some(&Value::Int(1)));
    let keys = ex.header("CamelSqlGeneratedKeysRows").unwrap().as_array().unwrap().clone();
    assert_eq!(keys[0].get("id"), Some(&Value::Int(4)), "{:?}", keys);

    // positional placeholders from a body array and a separated string
    t.send_body("direct:positional", Value::Array(vec![Value::from("fig"), Value::Float(4.0)])).unwrap();
    t.send_body("direct:positional", "plum,1.25").unwrap();
    assert_eq!(count(ds, "SELECT COUNT(*) FROM products"), 6);
    let e = t.request("direct:positional", |ex| ex.set_body(Value::Array(vec![Value::from("only-one")]))).err().unwrap();
    assert!(format!("{:?}", e).contains("Number of parameters mismatch"), "{:?}", e);

    // SelectList
    let ex = t.request("direct:list", |ex| ex.set_header("min", 1.5)).unwrap();
    let rows = ex.body().as_array().unwrap().clone();
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0], obj(&[("name", Value::from("apple")), ("price", Value::Float(1.5))]));
    assert_eq!(ex.header("CamelSqlRowCount"), Some(&Value::Int(4)));

    // SelectOne (simple expression parameter), single column and no row
    let ex = t.request("direct:one", |ex| ex.set_header("wanted", "pear")).unwrap();
    assert_eq!(ex.body(), &obj(&[("name", Value::from("pear")), ("stock", Value::Int(5))]));
    let ex = t
        .request("direct:one", |ex| {
            ex.set_header("wanted", "nothing");
            ex.set_body("unchanged");
        })
        .unwrap();
    assert_eq!(ex.body(), &Value::from("unchanged"));
    assert_eq!(ex.header("CamelSqlRowCount"), Some(&Value::Int(0)));
    assert_eq!(t.request_body("direct:scalar", Value::Null).unwrap(), Value::Int(6));

    // StreamList, outputHeader, noop
    let v = t.request_body("direct:stream", Value::Null).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 6);
    let ex = t.request("direct:header", |ex| ex.set_body("keep")).unwrap();
    assert_eq!(ex.body(), &Value::from("keep"));
    assert_eq!(ex.header("names").unwrap().as_array().unwrap().len(), 6);
    let ex = t.request("direct:noop", |ex| ex.set_body("same")).unwrap();
    assert_eq!(ex.body(), &Value::from("same"));
    assert_eq!(ex.header("CamelSqlRowCount"), Some(&Value::Int(6)));

    // IN list expansion
    let ex = t.request("direct:in", |ex| ex.set_header("ids", Value::Array(vec![Value::Int(1), Value::Int(3)]))).unwrap();
    assert_eq!(ex.body().as_array().unwrap().len(), 2);

    // UPDATE count
    let ex = t
        .request("direct:update", |ex| {
            ex.set_header("delta", 100);
            ex.set_header("max", 2.0);
        })
        .unwrap();
    assert_eq!(ex.header("CamelSqlUpdateCount"), Some(&Value::Int(3)));
    assert_eq!(count(ds, "SELECT stock FROM products WHERE name = 'kiwi'"), 100);

    // batch
    let items = Value::Array(vec![obj(&[("name", Value::from("b1")), ("price", Value::Float(1.0))]), obj(&[("name", Value::from("b2")), ("price", Value::Float(2.0))])]);
    let ex = t.request("direct:batch", |ex| ex.set_body(items.clone())).unwrap();
    assert_eq!(ex.header("CamelSqlUpdateCount"), Some(&Value::Int(2)));
    // a failing batch is atomic
    let bad = Value::Array(vec![obj(&[("name", Value::from("b3")), ("price", Value::Float(1.0))]), obj(&[("name", Value::from("b1")), ("price", Value::Float(2.0))])]);
    assert!(t.request("direct:batch", |ex| ex.set_body(bad.clone())).is_err());
    assert_eq!(count(ds, "SELECT COUNT(*) FROM products WHERE name = 'b3'"), 0);

    // useMessageBodyForSql and CamelSqlQuery header
    let ex = t
        .request("direct:body-sql", |ex| {
            ex.set_body("select count(*) as n from products where price > :#p");
            ex.set_header("p", 2.0);
        })
        .unwrap();
    assert_eq!(ex.body().as_array().unwrap()[0].get("n"), Some(&Value::Int(3)));
    let ex = t.request("direct:list", |ex| ex.set_header("CamelSqlQuery", "select name from products where name = 'fig'")).unwrap();
    assert_eq!(ex.body().as_array().unwrap().len(), 1);

    // errors
    let e = t.request("direct:bad", |_| {}).err().unwrap();
    let root = e.root_cause();
    assert_eq!(root.kind, "SQLSyntaxErrorException", "{:?}", e);
    assert!(root.message.contains("no_such_table"), "{}", root.message);
    assert!(root.is_a("SQLException"));
    assert_eq!(t.request_body("direct:catch", obj(&[("name", Value::from("unique-1"))])).unwrap(), Value::from("inserted"));
    assert_eq!(t.request_body("direct:catch", obj(&[("name", Value::from("unique-1"))])).unwrap(), Value::from("duplicate"));

    // jdbc component
    let ex = t.request("direct:jdbc", |ex| ex.set_body("select id, name from products where id <= 2 order by id")).unwrap();
    assert_eq!(ex.header("CamelJdbcRowCount"), Some(&Value::Int(2)));
    assert_eq!(ex.header("CamelJdbcColumnNames"), Some(&Value::Array(vec![Value::from("id"), Value::from("name")])));
    assert_eq!(ex.body().as_array().unwrap()[1], obj(&[("id", Value::Int(2)), ("name", Value::from("pear"))]));
    let ex = t.request("direct:jdbc", |ex| ex.set_body("update products set active = false where price > 3")).unwrap();
    assert_eq!(ex.header("CamelJdbcUpdateCount"), Some(&Value::Int(1)));
    let ex = t
        .request("direct:jdbc", |ex| {
            ex.set_body("insert into products (name, price) values ('jdbc-made', 9.5)");
            ex.set_header("CamelRetrieveGeneratedKeys", true);
        })
        .unwrap();
    assert_eq!(ex.header("CamelGeneratedKeysRowCount"), Some(&Value::Int(1)));
    assert!(ex.header("CamelGeneratedKeysRows").unwrap().as_array().unwrap()[0].get("id").is_some());
    assert_eq!(t.request_body("direct:jdbc-one", "select max(price) from products").unwrap(), Value::Float(9.5));
    let ex = t
        .request("direct:jdbc-params", |ex| {
            ex.set_body("select name from products where price < :?max order by name");
            ex.set_header("max", 2.0);
        })
        .unwrap();
    assert_eq!(ex.body().as_array().unwrap().len(), 2, "readSize limits the rows");
    assert_eq!(ex.body().as_array().unwrap()[0].get("name"), Some(&Value::from("apple")));
    ctx.stop().unwrap();
}
