//! Engine unit tests.

use super::*;
use camel_core::value::ObjectMap;
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

fn conn() -> EmbeddedConnection {
    EmbeddedConnection::open(&format!("enginetest{}", N.fetch_add(1, Ordering::SeqCst)))
}

fn run(c: &mut EmbeddedConnection, sql: &str) -> QueryResult {
    c.execute(sql, &[]).unwrap_or_else(|e| panic!("{}: {}", sql, e))
}

fn runp(c: &mut EmbeddedConnection, sql: &str, p: &[Value]) -> QueryResult {
    c.execute(sql, p).unwrap_or_else(|e| panic!("{}: {}", sql, e))
}

fn err(c: &mut EmbeddedConnection, sql: &str) -> CamelError {
    match c.execute(sql, &[]) {
        Ok(r) => panic!("expected error for {}, got {:?}", sql, r),
        Err(e) => e,
    }
}

fn rows(c: &mut EmbeddedConnection, sql: &str) -> Vec<Vec<Value>> {
    run(c, sql).rows
}

fn scalar(c: &mut EmbeddedConnection, sql: &str) -> Value {
    run(c, sql).rows.first().and_then(|r| r.first()).cloned().unwrap_or(Value::Null)
}

fn i(v: i64) -> Value {
    Value::Int(v)
}
fn s(v: &str) -> Value {
    Value::from(v)
}

fn setup() -> EmbeddedConnection {
    let mut c = conn();
    run(
        &mut c,
        "CREATE TABLE customers (id INTEGER PRIMARY KEY, name VARCHAR(50) NOT NULL, city VARCHAR(30), vip BOOLEAN DEFAULT false);
         CREATE TABLE orders (id SERIAL PRIMARY KEY, customer_id INT, item TEXT NOT NULL, amount DECIMAL(10,2), qty INTEGER DEFAULT 1, status VARCHAR(20) DEFAULT 'NEW', created TIMESTAMP DEFAULT CURRENT_TIMESTAMP);
         INSERT INTO customers (id, name, city, vip) VALUES (1, 'Alice', 'Paris', true), (2, 'Bob', 'London', false), (3, 'Carol', NULL, false), (4, 'Dave', 'Paris', true);
         INSERT INTO orders (customer_id, item, amount, qty) VALUES (1, 'apple', 1.5, 10), (1, 'pear', 2.25, 4), (2, 'apple', 1.5, 1), (3, 'kiwi', 0.5, 20), (NULL, 'melon', 3.0, 2);",
    );
    c
}

#[test]
fn row_maps_keep_column_order() {
    let mut c = setup();
    let r = run(&mut c, "SELECT name, id, city AS b, vip AS a FROM customers WHERE id = 1");
    assert_eq!(r.rows_as_values()[0].to_json(), r#"{"name":"Alice","id":1,"b":"Paris","a":true}"#);
    let maps = r.rows_as_maps();
    assert_eq!(maps[0].keys().map(String::as_str).collect::<Vec<_>>(), ["name", "id", "b", "a"]);
}

#[test]
fn create_insert_select() {
    let mut c = setup();
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM customers"), i(4));
    let r = run(&mut c, "SELECT * FROM customers WHERE id = 1");
    assert_eq!(r.columns, vec!["id", "name", "city", "vip"]);
    assert_eq!(r.rows, vec![vec![i(1), s("Alice"), s("Paris"), Value::Bool(true)]]);
    assert!(r.is_query);
    assert_eq!(r.update_count, 1);
}

#[test]
fn serial_and_generated_keys() {
    let mut c = setup();
    let r = run(&mut c, "INSERT INTO orders (customer_id, item, amount) VALUES (4, 'fig', 9.99)");
    assert_eq!(r.update_count, 1);
    assert_eq!(r.generated_keys.len(), 1);
    assert_eq!(r.generated_keys[0].get("id"), Some(&i(6)));
    let r = run(&mut c, "INSERT INTO orders (item) VALUES ('a'), ('b') RETURNING id, item");
    assert_eq!(r.rows, vec![vec![i(7), s("a")], vec![i(8), s("b")]]);
    assert_eq!(r.update_count, 2);
    assert!(r.is_query);
    // defaults applied
    assert_eq!(rows(&mut c, "SELECT qty, status FROM orders WHERE id = 7"), vec![vec![i(1), s("NEW")]]);
    let created = scalar(&mut c, "SELECT created FROM orders WHERE id = 7");
    assert!(created.as_str().unwrap().starts_with("20"), "{:?}", created);
    // auto_increment spelling
    run(&mut c, "CREATE TABLE m (id INT AUTO_INCREMENT PRIMARY KEY, v TEXT)");
    run(&mut c, "INSERT INTO m (v) VALUES ('x')");
    run(&mut c, "INSERT INTO m (id, v) VALUES (10, 'y')");
    run(&mut c, "INSERT INTO m (v) VALUES ('z')");
    assert_eq!(rows(&mut c, "SELECT id FROM m ORDER BY id"), vec![vec![i(1)], vec![i(10)], vec![i(11)]]);
    run(&mut c, "CREATE TABLE g (id BIGINT GENERATED ALWAYS AS IDENTITY, v TEXT)");
    run(&mut c, "INSERT INTO g (v) VALUES ('x')");
    assert_eq!(scalar(&mut c, "SELECT id FROM g"), i(1));
}

#[test]
fn where_operators() {
    let mut c = setup();
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM customers WHERE city = 'Paris' AND vip"), i(2));
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM customers WHERE city = 'Paris' OR city = 'London'"), i(3));
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM customers WHERE NOT vip"), i(2));
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM customers WHERE city IS NULL"), i(1));
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM customers WHERE city IS NOT NULL"), i(3));
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM customers WHERE name LIKE 'A%'"), i(1));
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM customers WHERE name NOT LIKE '%o%'"), i(2));
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM customers WHERE name ILIKE 'alice'"), i(1));
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM customers WHERE id IN (1, 3, 99)"), i(2));
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM customers WHERE id NOT IN (1, 3)"), i(2));
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM customers WHERE id BETWEEN 2 AND 3"), i(2));
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM customers WHERE id NOT BETWEEN 2 AND 3"), i(2));
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM customers WHERE id <> 1 AND id != 2"), i(2));
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM orders WHERE amount >= 1.5 AND qty < 10"), i(3));
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM customers WHERE (city = 'Paris' OR city IS NULL) AND NOT (id = 4)"), i(2));
    // NULL comparison yields unknown
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM customers WHERE city <> 'Paris'"), i(1));
}

#[test]
fn expressions_and_functions() {
    let mut c = conn();
    assert_eq!(scalar(&mut c, "SELECT 1 + 2 * 3"), i(7));
    assert_eq!(scalar(&mut c, "SELECT (1 + 2) * 3"), i(9));
    assert_eq!(scalar(&mut c, "SELECT 7 / 2"), i(3));
    assert_eq!(scalar(&mut c, "SELECT 7 % 3"), i(1));
    assert_eq!(scalar(&mut c, "SELECT 7.0 / 2"), Value::Float(3.5));
    assert_eq!(scalar(&mut c, "SELECT -5 + 2"), i(-3));
    assert_eq!(scalar(&mut c, "SELECT 'a' || 'b' || 1"), s("ab1"));
    assert_eq!(scalar(&mut c, "SELECT 'a' || NULL"), Value::Null);
    assert_eq!(scalar(&mut c, "SELECT UPPER('abc')"), s("ABC"));
    assert_eq!(scalar(&mut c, "SELECT LOWER('ABC')"), s("abc"));
    assert_eq!(scalar(&mut c, "SELECT LENGTH('héllo')"), i(5));
    assert_eq!(scalar(&mut c, "SELECT COALESCE(NULL, NULL, 'x')"), s("x"));
    assert_eq!(scalar(&mut c, "SELECT NULLIF(1, 1)"), Value::Null);
    assert_eq!(scalar(&mut c, "SELECT ABS(-4)"), i(4));
    assert_eq!(scalar(&mut c, "SELECT ROUND(2.567, 2)"), Value::Float(2.57));
    assert_eq!(scalar(&mut c, "SELECT SUBSTRING('hello', 2, 3)"), s("ell"));
    assert_eq!(scalar(&mut c, "SELECT SUBSTRING('hello' FROM 2 FOR 2)"), s("el"));
    assert_eq!(scalar(&mut c, "SELECT TRIM('  x  ')"), s("x"));
    assert_eq!(scalar(&mut c, "SELECT REPLACE('aXbX', 'X', '-')"), s("a-b-"));
    assert_eq!(scalar(&mut c, "SELECT CONCAT('a', 1, 'b')"), s("a1b"));
    assert_eq!(scalar(&mut c, "SELECT CASE WHEN 1 > 2 THEN 'x' ELSE 'y' END"), s("y"));
    assert_eq!(scalar(&mut c, "SELECT CASE 2 WHEN 1 THEN 'one' WHEN 2 THEN 'two' END"), s("two"));
    assert_eq!(scalar(&mut c, "SELECT CAST('42' AS INTEGER) + 1"), i(43));
    assert_eq!(scalar(&mut c, "SELECT '7'::int * 2"), i(14));
    assert_eq!(scalar(&mut c, "SELECT CAST(3 AS VARCHAR)"), s("3"));
    assert_eq!(scalar(&mut c, "SELECT GREATEST(1, 5, 3)"), i(5));
    assert_eq!(scalar(&mut c, "SELECT POSITION('lo' IN 'hello')"), i(4));
    assert_eq!(scalar(&mut c, "SELECT 1 IS NOT DISTINCT FROM 1"), Value::Bool(true));
    let now = scalar(&mut c, "SELECT NOW()");
    assert_eq!(now.as_str().unwrap().len(), 23);
    assert_eq!(scalar(&mut c, "SELECT CURRENT_TIMESTAMP = NOW()"), Value::Bool(true));
    assert_eq!(scalar(&mut c, "SELECT TRUE AND NULL"), Value::Null);
    assert_eq!(scalar(&mut c, "SELECT FALSE AND NULL"), Value::Bool(false));
    assert_eq!(scalar(&mut c, "SELECT TRUE OR NULL"), Value::Bool(true));
}

#[test]
fn aliases_and_column_names() {
    let mut c = setup();
    let r = run(&mut c, "SELECT c.name AS customer, c.id + 100 total, UPPER(c.name), COUNT(*) OVER_ FROM customers c WHERE c.id = 1 GROUP BY c.name, c.id");
    assert_eq!(r.columns, vec!["customer", "total", "upper", "OVER_"]);
    assert_eq!(r.rows, vec![vec![s("Alice"), i(101), s("ALICE"), i(1)]]);
    let r = run(&mut c, "SELECT 1, 'x' AS \"Quoted Name\"");
    assert_eq!(r.columns, vec!["?column?", "Quoted Name"]);
}

#[test]
fn group_by_having_aggregates() {
    let mut c = setup();
    let r = rows(&mut c, "SELECT item, COUNT(*), SUM(qty), MIN(amount), MAX(amount) FROM orders GROUP BY item ORDER BY item");
    assert_eq!(r[0], vec![s("apple"), i(2), i(11), Value::Float(1.5), Value::Float(1.5)]);
    assert_eq!(r.len(), 4);
    let r = rows(&mut c, "SELECT customer_id, SUM(amount * qty) AS total FROM orders WHERE customer_id IS NOT NULL GROUP BY customer_id HAVING SUM(amount * qty) > 5 ORDER BY total DESC");
    assert_eq!(r, vec![vec![i(1), Value::Float(24.0)], vec![i(3), Value::Float(10.0)]]);
    assert_eq!(scalar(&mut c, "SELECT AVG(qty) FROM orders"), Value::Float(7.4));
    assert_eq!(scalar(&mut c, "SELECT COUNT(customer_id) FROM orders"), i(4));
    assert_eq!(scalar(&mut c, "SELECT COUNT(DISTINCT item) FROM orders"), i(4));
    assert_eq!(scalar(&mut c, "SELECT SUM(qty) FROM orders WHERE id > 100"), Value::Null);
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM orders WHERE id > 100"), i(0));
    assert_eq!(scalar(&mut c, "SELECT STRING_AGG(name, ';') FROM customers WHERE vip"), s("Alice;Dave"));
    let r = rows(&mut c, "SELECT city, COUNT(*) FROM customers GROUP BY city ORDER BY 2 DESC, 1");
    assert_eq!(r[0], vec![s("Paris"), i(2)]);
}

#[test]
fn order_limit_offset_distinct() {
    let mut c = setup();
    let r = rows(&mut c, "SELECT name FROM customers ORDER BY name DESC");
    assert_eq!(r, vec![vec![s("Dave")], vec![s("Carol")], vec![s("Bob")], vec![s("Alice")]]);
    let r = rows(&mut c, "SELECT name FROM customers ORDER BY id LIMIT 2 OFFSET 1");
    assert_eq!(r, vec![vec![s("Bob")], vec![s("Carol")]]);
    let r = rows(&mut c, "SELECT name FROM customers ORDER BY id OFFSET 3");
    assert_eq!(r, vec![vec![s("Dave")]]);
    let r = rows(&mut c, "SELECT DISTINCT city FROM customers ORDER BY city");
    assert_eq!(r, vec![vec![s("London")], vec![s("Paris")], vec![Value::Null]]);
    let r = rows(&mut c, "SELECT city FROM customers ORDER BY city DESC NULLS LAST, id LIMIT 1");
    assert_eq!(r, vec![vec![s("Paris")]]);
    let r = rows(&mut c, "SELECT id FROM customers ORDER BY vip DESC, id DESC");
    assert_eq!(r, vec![vec![i(4)], vec![i(1)], vec![i(3)], vec![i(2)]]);
    let r = rows(&mut c, "SELECT id FROM customers ORDER BY id FETCH FIRST 1 ROWS ONLY");
    assert_eq!(r, vec![vec![i(1)]]);
}

#[test]
fn joins() {
    let mut c = setup();
    let r = rows(&mut c, "SELECT c.name, o.item FROM customers c INNER JOIN orders o ON o.customer_id = c.id WHERE c.id = 1 ORDER BY o.item");
    assert_eq!(r, vec![vec![s("Alice"), s("apple")], vec![s("Alice"), s("pear")]]);
    let r = rows(&mut c, "SELECT c.name, COUNT(o.id) FROM customers c LEFT JOIN orders o ON o.customer_id = c.id GROUP BY c.name ORDER BY c.name");
    assert_eq!(r, vec![vec![s("Alice"), i(2)], vec![s("Bob"), i(1)], vec![s("Carol"), i(1)], vec![s("Dave"), i(0)]]);
    let r = rows(&mut c, "SELECT o.item, c.name FROM customers c RIGHT JOIN orders o ON o.customer_id = c.id WHERE c.name IS NULL");
    assert_eq!(r, vec![vec![s("melon"), Value::Null]]);
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM customers, orders"), i(20));
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM customers CROSS JOIN orders"), i(20));
    let r = run(&mut c, "SELECT o.* FROM orders o JOIN customers ON customers.id = o.customer_id WHERE customers.name = 'Bob'");
    assert_eq!(r.columns.len(), 7);
    assert_eq!(r.rows.len(), 1);
    let e = err(&mut c, "SELECT id FROM customers c JOIN orders o ON o.customer_id = c.id");
    assert!(e.message.contains("ambiguous"), "{}", e.message);
}

#[test]
fn subqueries_and_union() {
    let mut c = setup();
    let r = rows(&mut c, "SELECT name FROM customers WHERE id IN (SELECT customer_id FROM orders WHERE item = 'apple') ORDER BY name");
    assert_eq!(r, vec![vec![s("Alice")], vec![s("Bob")]]);
    let r = rows(&mut c, "SELECT name FROM customers c WHERE NOT EXISTS (SELECT 1 FROM orders o WHERE o.customer_id = c.id)");
    assert_eq!(r, vec![vec![s("Dave")]]);
    assert_eq!(scalar(&mut c, "SELECT (SELECT MAX(qty) FROM orders) + 1"), i(21));
    let r = rows(&mut c, "SELECT t.n FROM (SELECT name AS n FROM customers WHERE vip) t ORDER BY t.n DESC");
    assert_eq!(r, vec![vec![s("Dave")], vec![s("Alice")]]);
    let r = rows(&mut c, "SELECT city FROM customers WHERE city IS NOT NULL UNION SELECT 'Rome' ORDER BY city");
    assert_eq!(r, vec![vec![s("London")], vec![s("Paris")], vec![s("Rome")]]);
    assert_eq!(run(&mut c, "SELECT city FROM customers UNION ALL SELECT city FROM customers").rows.len(), 8);
}

#[test]
fn update_and_delete() {
    let mut c = setup();
    let r = run(&mut c, "UPDATE orders SET status = 'DONE', qty = qty + 1 WHERE item = 'apple'");
    assert_eq!(r.update_count, 2);
    assert!(!r.is_query);
    assert_eq!(rows(&mut c, "SELECT qty FROM orders WHERE item = 'apple' ORDER BY id"), vec![vec![i(11)], vec![i(2)]]);
    let r = run(&mut c, "UPDATE customers SET city = DEFAULT, vip = DEFAULT WHERE id = 1 RETURNING id, vip");
    assert_eq!(r.rows, vec![vec![i(1), Value::Bool(false)]]);
    assert_eq!(scalar(&mut c, "SELECT city FROM customers WHERE id = 1"), Value::Null);
    let r = run(&mut c, "DELETE FROM orders WHERE status = 'DONE'");
    assert_eq!(r.update_count, 2);
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM orders"), i(3));
    let r = run(&mut c, "DELETE FROM orders WHERE id = 4 RETURNING item");
    assert_eq!(r.rows, vec![vec![s("kiwi")]]);
    assert_eq!(run(&mut c, "UPDATE orders SET qty = 0 WHERE 1 = 0").update_count, 0);
    assert_eq!(run(&mut c, "DELETE FROM orders").update_count, 2);
    run(&mut c, "INSERT INTO orders (item) VALUES ('x')");
    assert_eq!(run(&mut c, "TRUNCATE TABLE orders").update_count, 1);
}

#[test]
fn constraints() {
    let mut c = setup();
    let e = err(&mut c, "INSERT INTO customers (id, name) VALUES (1, 'Dup')");
    assert_eq!(e.kind, "SQLIntegrityConstraintViolationException");
    assert!(e.message.contains("duplicate key value violates unique constraint \"customers_pkey\""), "{}", e.message);
    assert!(e.message.contains("[SQLState: 23505]"));
    assert!(e.message.contains("Key (id)=(1)"), "{}", e.message);
    let e = err(&mut c, "INSERT INTO customers (id) VALUES (9)");
    assert!(e.message.contains("null value in column \"name\""), "{}", e.message);
    let e = err(&mut c, "INSERT INTO customers (id, name) VALUES (9, 'This name is definitely longer than fifty characters in total')");
    assert!(e.message.contains("value too long for type character varying(50)"), "{}", e.message);
    // multi-row insert is atomic
    let e = err(&mut c, "INSERT INTO customers (id, name) VALUES (10, 'a'), (11, 'b'), (10, 'c')");
    assert!(e.message.contains("duplicate"));
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM customers WHERE id >= 10"), i(0));
    // update violating uniqueness is rolled back
    assert!(c.execute("UPDATE customers SET id = 1 WHERE id = 2", &[]).is_err());
    assert_eq!(scalar(&mut c, "SELECT name FROM customers WHERE id = 2"), s("Bob"));
    run(&mut c, "CREATE TABLE u (a INT, b INT, c TEXT UNIQUE, PRIMARY KEY (a, b))");
    run(&mut c, "INSERT INTO u VALUES (1, 1, 'x'), (1, 2, 'y'), (2, 1, NULL), (2, 2, NULL)");
    assert!(c.execute("INSERT INTO u VALUES (1, 2, 'z')", &[]).is_err());
    assert!(c.execute("INSERT INTO u VALUES (3, 3, 'x')", &[]).is_err());
    let e = err(&mut c, "INSERT INTO u (a, b) VALUES (NULL, 5)");
    assert!(e.message.contains("not-null"), "{}", e.message);
    let e = err(&mut c, "INSERT INTO customers (id, name) VALUES ('abc', 'x')");
    assert!(e.message.contains("invalid input syntax for type integer"), "{}", e.message);
    assert_eq!(e.kind, "SQLDataException");
}

#[test]
fn ddl() {
    let mut c = conn();
    run(&mut c, "CREATE TABLE IF NOT EXISTS t (id INT)");
    run(&mut c, "CREATE TABLE IF NOT EXISTS t (id INT)");
    let e = err(&mut c, "CREATE TABLE t (id INT)");
    assert!(e.message.contains("relation \"t\" already exists"));
    run(&mut c, "CREATE INDEX idx_t ON t (id)");
    run(&mut c, "DROP TABLE t");
    let e = err(&mut c, "DROP TABLE t");
    assert!(e.message.contains("table \"t\" does not exist"));
    run(&mut c, "DROP TABLE IF EXISTS t");
    let e = err(&mut c, "SELECT * FROM nope");
    assert!(e.message.contains("relation \"nope\" does not exist"));
    assert_eq!(e.kind, "SQLSyntaxErrorException");
    run(
        &mut c,
        "CREATE TABLE all_types (a INTEGER, b BIGINT, c SMALLINT, d VARCHAR(10), e TEXT, f BOOLEAN, g DOUBLE PRECISION, h DECIMAL(8,3), i TIMESTAMP, j DATE, k BYTEA, l REAL, m NUMERIC, n CHAR(2), o TIMESTAMP WITH TIME ZONE, p CLOB, CONSTRAINT pk PRIMARY KEY (a), FOREIGN KEY (b) REFERENCES other(id))",
    );
    runp(
        &mut c,
        "INSERT INTO all_types (a, b, c, d, e, f, g, h, i, j, k) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[i(1), s("2"), i(3), s("ten"), s("text"), s("yes"), s("1.5"), Value::Float(1.23456), s("2024-05-06T07:08:09"), s("2024-05-06 10:00"), s("bytes")],
    );
    let r = run(&mut c, "SELECT a, b, f, g, h, i, j, k FROM all_types");
    assert_eq!(r.rows[0], vec![i(1), i(2), Value::Bool(true), Value::Float(1.5), Value::Float(1.235), s("2024-05-06 07:08:09"), s("2024-05-06"), Value::Bytes(b"bytes".to_vec())]);
    let e = err(&mut c, "CREATE TABLE x (a INT, a INT)");
    assert!(e.message.contains("specified more than once"));
}

#[test]
fn parameters() {
    let mut c = setup();
    let r = runp(&mut c, "SELECT name FROM customers WHERE city = ? AND vip = ? ORDER BY id", &[s("Paris"), Value::Bool(true)]);
    assert_eq!(r.rows.len(), 2);
    let r = runp(&mut c, "SELECT name FROM customers WHERE id = $2 OR id = $1 ORDER BY id", &[i(1), i(3)]);
    assert_eq!(r.rows, vec![vec![s("Alice")], vec![s("Carol")]]);
    // text parameters are coerced
    let r = runp(&mut c, "SELECT name FROM customers WHERE id = $1 AND vip = $2", &[s("4"), s("t")]);
    assert_eq!(r.rows, vec![vec![s("Dave")]]);
    let named: ObjectMap = [("city".to_string(), s("London")), ("id".to_string(), i(2))].into_iter().collect();
    let r = c.execute_named("SELECT name FROM customers WHERE city = :#city AND id = :id", &named).unwrap();
    assert_eq!(r.rows, vec![vec![s("Bob")]]);
    let e = c.execute("SELECT * FROM customers WHERE id = ? AND name = ?", &[i(1)]).unwrap_err();
    assert!(e.message.contains("No value specified for parameter 2"), "{}", e.message);
    // array parameter expands in IN
    let r = runp(&mut c, "SELECT COUNT(*) FROM customers WHERE id IN (?)", &[Value::Array(vec![i(1), i(2)])]);
    assert_eq!(r.rows[0][0], i(2));
    let r = runp(&mut c, "SELECT name FROM customers ORDER BY id LIMIT ?", &[i(1)]);
    assert_eq!(r.rows.len(), 1);
}

#[test]
fn transactions() {
    let name = format!("txtest{}", N.fetch_add(1, Ordering::SeqCst));
    let mut a = EmbeddedConnection::open(&name);
    let mut b = EmbeddedConnection::open(&name);
    run(&mut a, "CREATE TABLE t (id INT PRIMARY KEY, v TEXT)");
    run(&mut a, "INSERT INTO t VALUES (1, 'one')");
    run(&mut a, "BEGIN");
    assert_eq!(a.transaction_status(), b'T');
    run(&mut a, "INSERT INTO t VALUES (2, 'two')");
    run(&mut a, "UPDATE t SET v = 'uno' WHERE id = 1");
    run(&mut a, "DELETE FROM t WHERE id = 1");
    run(&mut a, "CREATE TABLE t2 (x INT)");
    assert_eq!(scalar(&mut a, "SELECT COUNT(*) FROM t"), i(1));
    run(&mut a, "ROLLBACK");
    assert_eq!(rows(&mut b, "SELECT id, v FROM t"), vec![vec![i(1), s("one")]]);
    assert!(b.execute("SELECT * FROM t2", &[]).is_err());
    // commit
    run(&mut a, "START TRANSACTION");
    run(&mut a, "INSERT INTO t VALUES (3, 'three')");
    run(&mut a, "COMMIT");
    assert_eq!(scalar(&mut b, "SELECT COUNT(*) FROM t"), i(2));
    // error aborts the transaction
    a.begin().unwrap();
    run(&mut a, "INSERT INTO t VALUES (4, 'four')");
    assert!(a.execute("INSERT INTO t VALUES (1, 'dup')", &[]).is_err());
    assert_eq!(a.transaction_status(), b'E');
    let e = a.execute("SELECT 1", &[]).unwrap_err();
    assert!(e.message.contains("current transaction is aborted"));
    assert!(a.commit().is_err());
    assert_eq!(scalar(&mut b, "SELECT COUNT(*) FROM t"), i(2));
    // dropping a connection with an open transaction rolls back
    {
        let mut c = EmbeddedConnection::open(&name);
        c.begin().unwrap();
        run(&mut c, "DELETE FROM t");
    }
    assert_eq!(scalar(&mut b, "SELECT COUNT(*) FROM t"), i(2));
    // trait API
    a.begin().unwrap();
    assert!(a.in_transaction());
    run(&mut a, "DROP TABLE t");
    a.rollback().unwrap();
    assert!(!a.in_transaction());
    assert_eq!(scalar(&mut b, "SELECT COUNT(*) FROM t"), i(2));
}

#[test]
fn syntax_errors() {
    let mut c = setup();
    let e = err(&mut c, "SELECT * FORM customers");
    assert!(e.message.contains("syntax error at or near \"FORM\""), "{}", e.message);
    assert!(e.message.contains("line 1, column 10"), "{}", e.message);
    assert_eq!(e.kind, "SQLSyntaxErrorException");
    let e = err(&mut c, "SELECT nope FROM customers");
    assert!(e.message.contains("column \"nope\" does not exist"), "{}", e.message);
    let e = err(&mut c, "SELECT x.id FROM customers");
    assert!(e.message.contains("missing FROM-clause entry for table \"x\""), "{}", e.message);
    let e = err(&mut c, "INSERT INTO customers (id, nope) VALUES (1, 2)");
    assert!(e.message.contains("column \"nope\" of relation \"customers\" does not exist"), "{}", e.message);
    let e = err(&mut c, "SELECT 1 / 0");
    assert!(e.message.contains("division by zero"));
    let e = err(&mut c, "SELECT UNKNOWNFN(1)");
    assert!(e.message.contains("function unknownfn() does not exist"));
    let e = err(&mut c, "UPDATE customers SET");
    assert!(e.message.contains("end of input"), "{}", e.message);
    let e = err(&mut c, "SELECT 'abc");
    assert!(e.message.contains("unterminated quoted string"));
    let e = err(&mut c, "SELECT (SELECT id FROM customers)");
    assert!(e.message.contains("more than one row"));
    let e = err(&mut c, "INSERT INTO customers VALUES (1, 2, 3, 4, 5)");
    assert!(e.message.contains("more expressions than target columns"));
    let e = err(&mut c, "SELECT name FROM customers WHERE\n  id = = 1");
    assert!(e.message.contains("line 2, column 8"), "{}", e.message);
}

#[test]
fn scripts_and_comments() {
    let mut c = conn();
    let r = c
        .execute_script(
            "-- create\nCREATE TABLE k (v INT); /* two rows */ INSERT INTO k VALUES (1); INSERT INTO k VALUES (2);; SELECT SUM(v) FROM k;",
            &[],
            None,
            &ExecuteOptions::default(),
        )
        .unwrap();
    assert_eq!(r.len(), 4);
    assert_eq!(r[3].rows[0][0], i(3));
    assert_eq!(r[1].command, "INSERT");
    let last = c.execute("SELECT v FROM k ORDER BY v; SELECT 42", &[]).unwrap();
    assert_eq!(last.rows[0][0], i(42));
    let r = c.execute_with("SELECT v FROM k", &[], &ExecuteOptions { max_rows: 1, ..Default::default() }).unwrap();
    assert_eq!(r.rows.len(), 1);
}

#[test]
fn shared_between_datasources_and_threads() {
    let name = format!("shared{}", N.fetch_add(1, Ordering::SeqCst));
    let ds = EmbeddedDataSource::new(&name);
    ds.get_connection().unwrap().execute("CREATE TABLE c (n INT)", &[]).unwrap();
    let mut handles = Vec::new();
    for t in 0..4 {
        let ds = ds.clone();
        handles.push(std::thread::spawn(move || {
            let mut c = ds.get_connection().unwrap();
            for k in 0..25 {
                c.execute("INSERT INTO c VALUES (?)", &[Value::Int(t * 100 + k)]).unwrap();
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let other = crate::datasource::datasource_from_url(&format!("jdbc:rustsql:mem:{}", name)).unwrap();
    let r = other.get_connection().unwrap().execute("SELECT COUNT(*), COUNT(DISTINCT n) FROM c", &[]).unwrap();
    assert_eq!(r.rows[0], vec![i(100), i(100)]);
    assert!(database(&name).table_names().contains(&"c".to_string()));
    assert!(drop_database(&name));
}

#[test]
fn insert_select_and_misc() {
    let mut c = setup();
    run(&mut c, "CREATE TABLE archive (id INT, item TEXT)");
    let r = run(&mut c, "INSERT INTO archive (id, item) SELECT id, item FROM orders WHERE qty > 3");
    assert_eq!(r.update_count, 3);
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM archive"), i(3));
    run(&mut c, "CREATE TABLE d (id SERIAL, note TEXT DEFAULT 'none')");
    run(&mut c, "INSERT INTO d DEFAULT VALUES");
    run(&mut c, "INSERT INTO d (note) VALUES (DEFAULT)");
    assert_eq!(rows(&mut c, "SELECT id, note FROM d ORDER BY id"), vec![vec![i(1), s("none")], vec![i(2), s("none")]]);
    // update using a subquery
    run(&mut c, "UPDATE customers SET vip = TRUE WHERE id IN (SELECT customer_id FROM orders WHERE qty >= 20)");
    assert_eq!(scalar(&mut c, "SELECT vip FROM customers WHERE id = 3"), Value::Bool(true));
    // schema-qualified names and FOR UPDATE
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM public.customers FOR UPDATE"), i(4));
    assert_eq!(rows(&mut c, "VALUES (1, 'a'), (2, 'b')").len(), 2);
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM customers WHERE vip IS TRUE"), i(3));
    // escaped LIKE
    run(&mut c, "INSERT INTO customers (id, name) VALUES (50, '50% off')");
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM customers WHERE name LIKE '50\\% %'"), i(1));
    assert_eq!(scalar(&mut c, "SELECT COUNT(*) FROM customers WHERE name LIKE '50!%%' ESCAPE '!'"), i(1));
}

#[test]
fn full_join_ctes_windows_alter() {
    let mut c = conn();
    run(&mut c, "create table a (id int, x text); create table b (id int, y text)");
    run(&mut c, "insert into a values (1,'a1'),(2,'a2'); insert into b values (2,'b2'),(3,'b3')");
    let r = rows(&mut c, "select a.id, b.id, x, y from a full outer join b on a.id = b.id order by coalesce(a.id, b.id)");
    assert_eq!(r, vec![vec![i(1), Value::Null, s("a1"), Value::Null], vec![i(2), i(2), s("a2"), s("b2")], vec![Value::Null, i(3), Value::Null, s("b3")]]);

    // CTEs (chained, column list, recursive)
    let r = rows(&mut c, "with t(n) as (select id from a), u as (select n * 10 as m from t) select m from u order by m desc");
    assert_eq!(r, vec![vec![i(20)], vec![i(10)]]);
    let r = rows(&mut c, "with recursive cnt(n) as (select 1 union all select n + 1 from cnt where n < 5) select sum(n), count(*) from cnt");
    assert_eq!(r, vec![vec![i(15), i(5)]]);
    run(&mut c, "create table emp (id int, boss int, name text, dept text, sal int)");
    run(&mut c, "insert into emp values (1,null,'ceo','x',300),(2,1,'vp','x',200),(3,2,'dev','y',100),(4,2,'ops','y',100),(5,3,'intern','y',50)");
    let r = rows(&mut c, "with recursive chain as (select id, name, 0 as depth from emp where boss is null union all select e.id, e.name, chain.depth + 1 from emp e join chain on e.boss = chain.id) select name, depth from chain order by depth, name");
    assert_eq!(r.iter().map(|r| r[1].clone()).collect::<Vec<_>>(), vec![i(0), i(1), i(2), i(2), i(3)]);
    assert_eq!(scalar(&mut c, "select (with q as (select max(sal) as m from emp) select m from q)"), i(300));

    // window functions
    let r = rows(&mut c, "select name, row_number() over (order by sal desc, id), rank() over (order by sal desc), dense_rank() over (order by sal desc) from emp order by id");
    assert_eq!(r.iter().map(|r| (r[1].clone(), r[2].clone(), r[3].clone())).collect::<Vec<_>>(), vec![(i(1), i(1), i(1)), (i(2), i(2), i(2)), (i(3), i(3), i(3)), (i(4), i(3), i(3)), (i(5), i(5), i(4))]);
    let r = rows(&mut c, "select id, sum(sal) over (partition by dept order by id), sum(sal) over (partition by dept), lag(name) over (order by id), lead(name, 2, 'none') over (order by id) from emp order by id");
    assert_eq!(r[1], vec![i(2), i(500), i(500), s("ceo"), s("ops")]);
    assert_eq!(r[4], vec![i(5), i(250), i(250), s("ops"), s("none")]);
    let r = rows(&mut c, "select id, ntile(2) over (order by id), first_value(name) over (partition by dept order by sal), count(*) over () from emp order by id");
    assert_eq!(r.iter().map(|r| r[1].clone()).collect::<Vec<_>>(), vec![i(1), i(1), i(1), i(2), i(2)]);
    assert_eq!(r[2][2], s("intern"));
    assert_eq!(r[0][3], i(5));
    // windows over grouped rows and ordering by a window
    let r = rows(&mut c, "select dept, sum(sal), rank() over (order by sum(sal) desc) from emp group by dept order by 3");
    assert_eq!(r, vec![vec![s("x"), i(500), i(1)], vec![s("y"), i(250), i(2)]]);
    let r = rows(&mut c, "select name from emp order by row_number() over (order by name desc) limit 2");
    assert_eq!(r, vec![vec![s("vp")], vec![s("ops")]]);
    assert!(err(&mut c, "select id from emp where row_number() over () > 1").message.contains("window"));

    // ALTER TABLE
    run(&mut c, "alter table a add column z int default 7");
    assert_eq!(rows(&mut c, "select z from a order by id"), vec![vec![i(7)], vec![i(7)]]);
    run(&mut c, "alter table a add column if not exists z int");
    run(&mut c, "alter table a rename column x to label");
    assert_eq!(scalar(&mut c, "select label from a where id = 2"), s("a2"));
    run(&mut c, "alter table a alter column z set not null");
    assert!(err(&mut c, "insert into a (id, label, z) values (9, 'q', null)").message.contains("null"));
    run(&mut c, "alter table a alter column z drop not null; alter table a alter column z type text");
    assert_eq!(scalar(&mut c, "select z from a where id = 1"), s("7"));
    run(&mut c, "alter table a add constraint a_pk primary key (id)");
    assert!(err(&mut c, "insert into a (id) values (1)").message.contains("a_pk"));
    run(&mut c, "alter table a drop constraint a_pk; alter table a drop column z");
    run(&mut c, "insert into a (id) values (1)");
    assert_eq!(run(&mut c, "select * from a").columns, vec!["id".to_string(), "label".to_string()]);
    run(&mut c, "alter table a rename to a2");
    assert_eq!(scalar(&mut c, "select count(*) from a2"), i(3));
    assert!(err(&mut c, "select * from a").message.contains("does not exist"));
    run(&mut c, "alter table if exists nope add column q int");
    // DDL inside a rolled back transaction is undone
    run(&mut c, "begin; alter table a2 add column w int; rollback");
    assert_eq!(run(&mut c, "select * from a2").columns.len(), 2);
}
