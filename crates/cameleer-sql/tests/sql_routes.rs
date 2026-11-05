//! Route tests for the sql / jdbc components against the embedded rustsql engine.

mod common;

use std::sync::Arc;
use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_sql::{datasource_from_url, transaction_policy, DataSource, JdbcAggregationRepository, JdbcIdempotentRepository};
use common::*;

fn embedded() -> Arc<dyn DataSource> {
    datasource_from_url(&format!("jdbc:rustsql:mem:{}", unique("routes"))).unwrap()
}

#[test]
fn producer_suite_embedded() {
    let ds = embedded();
    let ctx = context_with(ds.clone());
    producer_suite(&ctx, &ds);
}

#[test]
fn stored_procedure_suite_embedded() {
    let ds = embedded();
    let ctx = context_with(ds.clone());
    let dir = std::env::temp_dir().join(unique("stored"));
    std::fs::create_dir_all(&dir).unwrap();
    stored_suite(&ctx, &ds, &dir, false);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn stored_procedure_in_a_transacted_route() {
    let ds = embedded();
    let ctx = context_with(ds.clone());
    exec(&ds, "CREATE TABLE log (msg TEXT)");
    exec(&ds, "CREATE PROCEDURE write_log(IN m TEXT) LANGUAGE plpgsql AS $$ BEGIN INSERT INTO log VALUES (m); END $$");
    cameleer_sql::bind_transaction_policy(&ctx, "tx", ds.clone());
    ctx.add_routes(|b| {
        b.from("direct:tx").policy_ref("tx").to("sql-stored:write_log(VARCHAR ${body})?dataSource=#ds").throw_exception("IllegalStateException", "boom");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    assert!(t.request_body("direct:tx", "hello").is_err());
    assert_eq!(count(&ds, "SELECT count(*) FROM log"), 0, "the procedure's insert was rolled back with the route transaction");
    ctx.stop().unwrap();
}

#[test]
fn timer_to_sql_insert() {
    let ds = embedded();
    exec(&ds, "CREATE TABLE ticks (id SERIAL PRIMARY KEY, counter INT, note TEXT)");
    let ctx = context_with(ds.clone());
    ctx.add_routes(|b| {
        b.from("timer:tick?period=1&repeatCount=3")
            .set_header("note", simple("tick ${exchangeProperty.CamelTimerCounter}"))
            .to("sql:insert into ticks (counter, note) values (:#${exchangeProperty.CamelTimerCounter}, :#note)?dataSource=#ds")
            .to("mock:done");
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:done").unwrap();
    mock.expected_message_count(3);
    ctx.start().unwrap();
    mock.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
    let r = exec(&ds, "SELECT counter, note FROM ticks ORDER BY id");
    assert_eq!(r.rows.len(), 3);
    assert_eq!(r.rows[0], vec![Value::Int(1), Value::from("tick 1")]);
    assert_eq!(r.rows[2], vec![Value::Int(3), Value::from("tick 3")]);
}

#[test]
fn consumer_with_on_consume_marks_rows_processed() {
    let ds = embedded();
    exec(&ds, "CREATE TABLE orders (id SERIAL PRIMARY KEY, item TEXT, processed BOOLEAN DEFAULT false)");
    exec(&ds, "INSERT INTO orders (item) VALUES ('a'), ('b'), ('c')");
    let ctx = context_with(ds.clone());
    ctx.add_routes(|b| {
        b.from("sql:select * from orders where processed = false order by id?dataSource=#ds&initialDelay=0&delay=10&onConsume=update orders set processed = true where id = :#id&onConsumeBatchComplete=insert into orders (item, processed) values ('batch-done', true)")
            .to("mock:orders");
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:orders").unwrap();
    mock.expected_message_count(3);
    ctx.start().unwrap();
    mock.assert_is_satisfied().unwrap();
    let bodies = mock.received_bodies();
    assert_eq!(bodies[0].get("item"), Some(&Value::from("a")));
    assert_eq!(bodies[2].get("id"), Some(&Value::Int(3)));
    let ex = &mock.received_exchanges()[1];
    assert_eq!(ex.property("CamelBatchIndex"), Some(&Value::Int(1)));
    assert_eq!(ex.property("CamelBatchSize"), Some(&Value::Int(3)));
    // more rows arrive later and are picked up exactly once
    exec(&ds, "INSERT INTO orders (item) VALUES ('d')");
    mock.expected_message_count(4);
    mock.assert_is_satisfied().unwrap();
    std::thread::sleep(Duration::from_millis(60));
    ctx.stop().unwrap();
    assert_eq!(mock.received_exchanges().len(), 4);
    assert_eq!(count(&ds, "SELECT COUNT(*) FROM orders WHERE processed = false"), 0);
    assert!(count(&ds, "SELECT COUNT(*) FROM orders WHERE item = 'batch-done'") >= 2);
}

#[test]
fn consumer_list_mode_empty_result_and_max_messages() {
    let ds = embedded();
    exec(&ds, "CREATE TABLE jobs (id INT PRIMARY KEY, state VARCHAR(10))");
    exec(&ds, "INSERT INTO jobs VALUES (1, 'new'), (2, 'new'), (3, 'new')");
    let ctx = context_with(ds.clone());
    ctx.add_routes(|b| {
        b.from("sql:select id from jobs where state = 'new' order by id?dataSource=#ds&useIterator=false&maxMessagesPerPoll=2&routeEmptyResultSet=true&initialDelay=0&delay=10&onConsume=update jobs set state = 'done' where id = :#id")
            .to("mock:jobs");
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:jobs").unwrap();
    mock.expected_minimum_message_count(3);
    ctx.start().unwrap();
    mock.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
    let ex = mock.received_exchanges();
    assert_eq!(ex[0].body().as_array().unwrap().len(), 2);
    assert_eq!(ex[0].header("CamelSqlRowCount"), Some(&Value::Int(2)));
    assert_eq!(ex[1].body().as_array().unwrap().len(), 1);
    assert_eq!(ex[2].body(), &Value::Array(vec![]), "empty result sets are routed");
    assert_eq!(count(&ds, "SELECT COUNT(*) FROM jobs WHERE state = 'done'"), 3);
}

#[test]
fn consumer_on_consume_failed_and_query_from_file() {
    let ds = embedded();
    exec(&ds, "CREATE TABLE inbox (id INT PRIMARY KEY, payload TEXT, status VARCHAR(10) DEFAULT 'NEW')");
    exec(&ds, "INSERT INTO inbox (id, payload) VALUES (1, 'good'), (2, 'bad')");
    let dir = std::env::temp_dir().join(unique("sqlfile"));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("select.sql");
    std::fs::write(&file, "-- pick new messages\nselect * from inbox\nwhere status = 'NEW' order by id;\n").unwrap();
    let ctx = context_with(ds.clone());
    let uri = format!(
        "sql:file:{}?dataSource=#ds&initialDelay=0&delay=10&onConsume=update inbox set status = 'OK' where id = :#id&onConsumeFailed=update inbox set status = 'FAILED' where id = :#id",
        file.display()
    );
    ctx.add_routes(|b| {
        b.from(&uri)
            .process(|ex: &mut Exchange| {
                if ex.body().get("payload") == Some(&Value::from("bad")) {
                    return Err(CamelError::illegal_argument("bad payload"));
                }
                Ok(())
            })
            .to("mock:inbox");
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:inbox").unwrap();
    mock.expected_message_count(1);
    ctx.start().unwrap();
    mock.assert_is_satisfied().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while count(&ds, "SELECT COUNT(*) FROM inbox WHERE status = 'NEW'") > 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    ctx.stop().unwrap();
    let r = exec(&ds, "SELECT status FROM inbox ORDER BY id");
    assert_eq!(r.rows, vec![vec![Value::from("OK")], vec![Value::from("FAILED")]]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn transacted_consumer_rolls_back_the_poll() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let ds = embedded();
    exec(&ds, "CREATE TABLE q (id INT PRIMARY KEY, done BOOLEAN DEFAULT false)");
    exec(&ds, "INSERT INTO q (id) VALUES (1), (2)");
    let ctx = context_with(ds.clone());
    let fail = Arc::new(AtomicBool::new(true));
    let calls = Arc::new(AtomicUsize::new(0));
    let (f2, c2) = (fail.clone(), calls.clone());
    ctx.add_routes(move |b| {
        let (f3, c3) = (f2.clone(), c2.clone());
        b.from("sql:select id from q where done = false order by id?dataSource=#ds&transacted=true&initialDelay=0&delay=5&onConsume=update q set done = true where id = :#id")
            .process(move |ex: &mut Exchange| {
                c3.fetch_add(1, Ordering::SeqCst);
                if ex.body().get("id") == Some(&Value::Int(2)) && f3.load(Ordering::SeqCst) {
                    return Err(CamelError::runtime("boom"));
                }
                Ok(())
            });
    })
    .unwrap();
    ctx.start().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while calls.load(Ordering::SeqCst) < 6 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    // every poll fails on row 2, so row 1's onConsume is rolled back each time
    let done_while_failing = count(&ds, "SELECT COUNT(*) FROM q WHERE done = true");
    fail.store(false, Ordering::SeqCst);
    while count(&ds, "SELECT COUNT(*) FROM q WHERE done = true") < 2 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    ctx.stop().unwrap();
    assert!(calls.load(Ordering::SeqCst) >= 6);
    assert_eq!(done_while_failing, 0);
    assert_eq!(count(&ds, "SELECT COUNT(*) FROM q WHERE done = true"), 2);
}

#[test]
fn transacted_consumer_failure_keeps_rows_unprocessed() {
    let ds = embedded();
    exec(&ds, "CREATE TABLE q2 (id INT PRIMARY KEY, done BOOLEAN DEFAULT false)");
    exec(&ds, "INSERT INTO q2 (id) VALUES (1), (2)");
    let ctx = context_with(ds.clone());
    ctx.add_routes(|b| {
        b.from("sql:select id from q2 where done = false order by id?dataSource=#ds&transacted=true&initialDelay=0&delay=5&onConsume=update q2 set done = true where id = :#id")
            .to("mock:q2")
            .process(|_ex: &mut Exchange| Err(CamelError::runtime("always fails")));
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:q2").unwrap();
    mock.expected_minimum_message_count(4);
    ctx.start().unwrap();
    mock.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
    // transacted: the batch stops at the first failure, so only row 1 is ever routed
    assert!(mock.received_bodies().iter().all(|b| b.get("id") == Some(&Value::Int(1))));
    assert_eq!(count(&ds, "SELECT COUNT(*) FROM q2 WHERE done = true"), 0);
}

#[test]
fn transaction_policy_commits_and_rolls_back() {
    let ds = embedded();
    exec(&ds, "CREATE TABLE ledger (id SERIAL PRIMARY KEY, account TEXT, amount INT)");
    let ctx = context_with(ds.clone());
    let policy = transaction_policy(ds.clone());
    ctx.add_routes(|b| {
        b.from("direct:transfer")
            .policy(policy.clone())
            .to("sql:insert into ledger (account, amount) values ('from', -:#amount)?dataSource=#ds")
            .to("jdbc:ds?useHeadersAsParameters=true")
            .process(|ex: &mut Exchange| {
                if ex.header("amount").and_then(|v| v.as_i64()).unwrap_or(0) > 100 {
                    return Err(CamelError::illegal_state("insufficient funds"));
                }
                Ok(())
            })
            .end()
            .to("mock:transferred");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let send = |amount: i64| {
        t.request("direct:transfer", |ex| {
            ex.set_header("amount", amount);
            ex.set_body("insert into ledger (account, amount) values ('to', :?amount)");
        })
    };
    assert!(send(50).is_ok());
    assert_eq!(count(&ds, "SELECT COUNT(*) FROM ledger"), 2);
    assert_eq!(count(&ds, "SELECT SUM(amount) FROM ledger"), 0);
    let e = send(500).err().unwrap();
    assert!(format!("{:?}", e).contains("insufficient funds"));
    assert_eq!(count(&ds, "SELECT COUNT(*) FROM ledger"), 2, "both inserts rolled back");
    assert!(send(10).is_ok());
    assert_eq!(count(&ds, "SELECT COUNT(*) FROM ledger"), 4);
    ctx.stop().unwrap();
}

#[test]
fn idempotent_consumer_with_jdbc_repository() {
    let ds = embedded();
    let repo = Arc::new(JdbcIdempotentRepository::new(ds.clone(), "orderRoute").unwrap());
    let ctx = context_with(ds.clone());
    let r2 = repo.clone();
    ctx.add_routes(move |b| {
        b.from("direct:in").idempotent_consumer(header("messageId"), r2.clone()).to("mock:unique").end();
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:unique").unwrap();
    mock.expected_bodies_received(vec!["one", "two", "three"]);
    let t = ctx.create_producer_template();
    for (id, body) in [("1", "one"), ("2", "two"), ("1", "one-again"), ("3", "three"), ("2", "two-again")] {
        t.send_body_and_header("direct:in", body, "messageId", id).unwrap();
    }
    mock.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
    assert_eq!(count(&ds, "SELECT COUNT(*) FROM CAMEL_MESSAGEPROCESSED WHERE processorName = 'orderRoute'"), 3);
    assert!(exec(&ds, "SELECT createdAt FROM CAMEL_MESSAGEPROCESSED").rows[0][0].as_str().is_some());
    // a new repository instance sees the stored keys (persistence)
    use cameleer_core::processor::errors::IdempotentRepository;
    let again = JdbcIdempotentRepository::new(ds.clone(), "orderRoute").unwrap();
    assert!(again.contains("2"));
    assert!(!again.add("3"));
    assert!(again.remove("3"));
    assert!(again.add("3"));
    let other = JdbcIdempotentRepository::new(ds.clone(), "otherRoute").unwrap();
    assert!(!other.contains("1"));
    again.clear();
    assert_eq!(again.size().unwrap(), 0);
    assert!(other.add("1"));
}

#[test]
fn aggregation_with_jdbc_repository() {
    let ds = embedded();
    let ctx = context_with(ds.clone());
    let repo = Arc::new(JdbcAggregationRepository::new(&ctx, ds.clone(), "aggregation").unwrap());
    let r2 = repo.clone();
    ctx.add_routes(move |b| {
        b.from("direct:agg")
            .aggregate(header("group"), |old: Option<Exchange>, new: Exchange| -> Result<Exchange> {
                Ok(match old {
                    None => new,
                    Some(mut o) => {
                        let body = format!("{}+{}", o.body(), new.body());
                        o.set_body(body);
                        o
                    }
                })
            })
            .completion_size(3)
            .aggregation_repository(r2.clone())
            .to("mock:aggregated")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:aggregated").unwrap();
    mock.expected_message_count(2);
    let t = ctx.create_producer_template();
    for (g, b) in [("A", "a1"), ("B", "b1"), ("A", "a2"), ("B", "b2"), ("A", "a3"), ("C", "c1"), ("B", "b3")] {
        t.send_body_and_header("direct:agg", b, "group", g).unwrap();
    }
    mock.assert_is_satisfied().unwrap();
    let bodies: Vec<String> = mock.received_bodies().iter().map(|v| v.to_string()).collect();
    assert_eq!(bodies, vec!["a1+a2+a3", "b1+b2+b3"]);
    // group C is still in progress and stored in the table
    use cameleer_core::processor::routing::AggregationRepository;
    assert_eq!(repo.keys(), vec!["C".to_string()]);
    assert_eq!(count(&ds, "SELECT COUNT(*) FROM aggregation"), 1);
    let stored = repo.get("C").unwrap();
    assert_eq!(stored.body(), &Value::from("c1"));
    assert_eq!(stored.header("group"), Some(&Value::from("C")));
    assert_eq!(repo.scan().unwrap().len(), 0, "completed exchanges were confirmed");
    ctx.stop().unwrap();
    // recovery API
    let ex = repo.remove("C").unwrap();
    assert_eq!(repo.scan().unwrap(), vec![ex.exchange_id.to_string()]);
    assert_eq!(repo.recover(&ex.exchange_id).unwrap().unwrap().body(), &Value::from("c1"));
    repo.confirm(&ex.exchange_id);
    assert!(repo.scan().unwrap().is_empty());
}

#[test]
fn endpoint_configuration_errors() {
    let ds = embedded();
    let ctx = context_with(ds.clone());
    assert!(ctx.get_endpoint("sql-stored:SUB_NUMBERS(INTEGER ${headers.num1})").is_ok());
    let e = ctx.get_endpoint("sql-stored:SUB_NUMBERS(OUT INTEGER)").err().unwrap();
    assert!(format!("{:?}", e).contains("Missing name"), "{:?}", e);
    assert!(ctx.get_endpoint("sql-stored:P()?bogus=1").is_err());
    assert!(ctx.get_endpoint("sql:select 1?dataSource=#missing").is_err());
    assert!(ctx.get_endpoint("sql:select 1?dataSource=#ds&bogus=1").is_err());
    assert!(ctx.get_endpoint("sql:select 1?dataSource=#ds&outputType=Nope").is_err());
    assert!(ctx.get_endpoint("jdbc:missing").is_err());
    // autowiring the single data source and url data sources
    assert!(ctx.get_endpoint("sql:select 1").is_ok());
    assert!(ctx.get_endpoint("sql:select 2?dataSource=jdbc:rustsql:mem:inline").is_ok());
    let jdbc = ctx.endpoint::<cameleer_sql::jdbc::JdbcEndpoint>("jdbc:ds?statement.maxRows=5&readSize=10").unwrap();
    assert_eq!(jdbc.as_any().downcast_ref::<cameleer_sql::jdbc::JdbcEndpoint>().unwrap().config().read_size, 5);
}

#[test]
fn camel_main_style_properties() {
    let ctx = CamelContext::new();
    cameleer_sql::register(&ctx);
    let name = unique("props");
    ctx.properties().set("camel.datasource.url", format!("jdbc:rustsql:mem:{}", name));
    cameleer_sql::configure_datasources_from_properties(&ctx).unwrap();
    ctx.add_routes(|b| {
        b.from("direct:q").to("sql:select 40 + 2 as answer?dataSource=#dataSource&outputType=SelectOne");
    })
    .unwrap();
    ctx.start().unwrap();
    assert_eq!(ctx.create_producer_template().request_body("direct:q", Value::Null).unwrap(), Value::Int(42));
    ctx.stop().unwrap();
}
