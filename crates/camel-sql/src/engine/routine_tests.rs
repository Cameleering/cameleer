//! Stored procedure / function tests for the embedded engine.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

fn conn() -> EmbeddedConnection {
    EmbeddedConnection::open(&format!("routinetest{}", N.fetch_add(1, Ordering::SeqCst)))
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

fn scalar(c: &mut EmbeddedConnection, sql: &str) -> Value {
    run(c, sql).rows.first().and_then(|r| r.first()).cloned().unwrap_or(Value::Null)
}

fn i(v: i64) -> Value {
    Value::Int(v)
}

fn s(v: &str) -> Value {
    Value::from(v)
}

#[test]
fn procedure_with_out_parameters() {
    let mut c = conn();
    run(&mut c, "CREATE PROCEDURE subnumbers(IN a INTEGER, IN b INTEGER, OUT res INTEGER) LANGUAGE plpgsql AS $$ BEGIN res := a - b; END $$");
    let r = run(&mut c, "CALL subnumbers(5, 3, NULL)");
    assert!(r.is_query);
    assert_eq!(r.command, "CALL");
    assert_eq!(r.columns, vec!["res"]);
    assert_eq!(r.rows, vec![vec![i(2)]]);
    // OUT arguments may be omitted; parameters bind with ?
    assert_eq!(runp(&mut c, "CALL subnumbers(?, ?)", &[i(10), s("4")]).rows, vec![vec![i(6)]]);
    // INOUT and several OUT parameters, Oracle-style `name IN type`
    run(&mut c, "CREATE OR REPLACE PROCEDURE stats(x IN INTEGER, INOUT total INTEGER, OUT doubled INTEGER, OUT label VARCHAR) AS $$ BEGIN total := total + x; doubled := x * 2; label := 'x=' || x; END $$ LANGUAGE plpgsql");
    let r = run(&mut c, "CALL stats(7, 100, NULL, NULL)");
    assert_eq!(r.columns, vec!["total", "doubled", "label"]);
    assert_eq!(r.rows, vec![vec![i(107), i(14), s("x=7")]]);
    // already exists / unknown / wrong arity / function via CALL
    assert!(err(&mut c, "CREATE PROCEDURE subnumbers(a int) AS $$ BEGIN END $$").message.contains("already exists"));
    assert!(err(&mut c, "CALL nope(1)").message.contains("does not exist"));
    assert!(err(&mut c, "CALL subnumbers(1, 2, 3, 4)").message.contains("argument"));
    run(&mut c, "DROP PROCEDURE subnumbers");
    assert!(err(&mut c, "CALL subnumbers(1, 2)").message.contains("does not exist"));
    run(&mut c, "DROP PROCEDURE IF EXISTS subnumbers");
}

#[test]
fn sql_and_plpgsql_functions() {
    let mut c = conn();
    run(&mut c, "CREATE FUNCTION add_nums(a integer, b integer) RETURNS integer AS $$ SELECT a + b $$ LANGUAGE sql");
    let r = run(&mut c, "SELECT add_nums(2, 3)");
    assert_eq!(r.columns, vec!["add_nums"]);
    assert_eq!(r.rows, vec![vec![i(5)]]);
    assert_eq!(runp(&mut c, "SELECT add_nums(?, ?) AS s", &[i(1), i(2)]).rows, vec![vec![i(3)]]);
    // positional $n in a SQL body, nested calls
    run(&mut c, "CREATE FUNCTION twice(integer) RETURNS integer LANGUAGE sql IMMUTABLE AS 'SELECT $1 * 2'");
    assert_eq!(scalar(&mut c, "SELECT twice(add_nums(1, twice(2)))"), i(10));
    assert_eq!(scalar(&mut c, "SELECT * FROM add_nums(4, 4)"), i(8));
    // procedural: DECLARE, WHILE, IF/ELSIF/ELSE, recursion
    run(
        &mut c,
        "CREATE FUNCTION fact(n integer) RETURNS bigint AS $$
         DECLARE result bigint := 1; i integer := 1;
         BEGIN
           IF n < 0 THEN RAISE EXCEPTION 'negative input %', n;
           ELSIF n = 0 THEN RETURN 1;
           ELSE
             WHILE i <= n LOOP result := result * i; i := i + 1; END LOOP;
           END IF;
           RETURN result;
         END; $$ LANGUAGE plpgsql",
    );
    assert_eq!(scalar(&mut c, "SELECT fact(10)"), i(3628800));
    assert_eq!(scalar(&mut c, "SELECT fact(0)"), i(1));
    let e = err(&mut c, "SELECT fact(-2)");
    assert!(e.message.contains("negative input -2"), "{}", e.message);
    assert!(format!("{:?}", e).contains("P0001"), "{:?}", e);
    run(&mut c, "CREATE FUNCTION fib(n int) RETURNS int AS $$ BEGIN IF n < 2 THEN RETURN n; END IF; RETURN fib(n - 1) + fib(n - 2); END $$ LANGUAGE plpgsql");
    assert_eq!(scalar(&mut c, "SELECT fib(15)"), i(610));
    // per-row evaluation over a table
    run(&mut c, "CREATE TABLE t (id INT PRIMARY KEY, qty INT)");
    run(&mut c, "INSERT INTO t VALUES (1, 5), (2, 7)");
    let r = run(&mut c, "SELECT id, twice(qty) AS d FROM t WHERE twice(qty) > 10 ORDER BY id");
    assert_eq!(r.rows, vec![vec![i(2), i(14)]]);
    // FOR range with REVERSE/BY, EXIT WHEN, CONTINUE, labels
    run(
        &mut c,
        "CREATE FUNCTION loops() RETURNS text AS $$
         DECLARE s text := '';
         BEGIN
           FOR i IN 1..10 BY 2 LOOP s := s || i; END LOOP;
           s := s || '|';
           FOR i IN REVERSE 3..1 LOOP s := s || i; END LOOP;
           s := s || '|';
           <<outer>> FOR i IN 1..3 LOOP
             FOR j IN 1..3 LOOP
               CONTINUE WHEN j = 2;
               EXIT outer WHEN i = 3;
               s := s || i || j || ',';
             END LOOP;
           END LOOP outer;
           LOOP s := s || '!'; EXIT; END LOOP;
           RETURN s;
         END $$ LANGUAGE plpgsql",
    );
    assert_eq!(scalar(&mut c, "SELECT loops()"), s("13579|321|11,13,21,23,!"));
    // defaults and STRICT
    run(&mut c, "CREATE FUNCTION greet(name text, greeting text DEFAULT 'Hello') RETURNS text AS $$ BEGIN RETURN greeting || ' ' || name; END $$ LANGUAGE plpgsql STRICT");
    assert_eq!(scalar(&mut c, "SELECT greet('Bob')"), s("Hello Bob"));
    assert_eq!(scalar(&mut c, "SELECT greet('Bob', 'Hi')"), s("Hi Bob"));
    assert_eq!(scalar(&mut c, "SELECT greet(NULL)"), Value::Null);
    // missing RETURN
    run(&mut c, "CREATE FUNCTION noret() RETURNS int AS $$ BEGIN NULL; END $$ LANGUAGE plpgsql");
    assert!(err(&mut c, "SELECT noret()").message.contains("without RETURN"));
    // function OUT parameters (record)
    run(&mut c, "CREATE FUNCTION divmod(a int, b int, OUT q int, OUT r int) AS $$ BEGIN q := a / b; r := a % b; END $$ LANGUAGE plpgsql");
    let r = run(&mut c, "SELECT * FROM divmod(17, 5)");
    assert_eq!(r.columns, vec!["q", "r"]);
    assert_eq!(r.rows, vec![vec![i(3), i(2)]]);
    assert!(err(&mut c, "CALL fact(3)").message.contains("not a procedure"));
}

#[test]
fn procedures_modify_data_with_queries_and_handlers() {
    let mut c = conn();
    run(&mut c, "CREATE TABLE accounts (id INT PRIMARY KEY, owner TEXT, balance INT NOT NULL)");
    run(&mut c, "CREATE TABLE audit (msg TEXT)");
    run(&mut c, "INSERT INTO accounts VALUES (1, 'ann', 100), (2, 'bob', 50)");
    run(
        &mut c,
        "CREATE PROCEDURE transfer(src INT, dst INT, amount INT, OUT src_balance INT, OUT status TEXT) LANGUAGE plpgsql AS $$
         DECLARE cur INT;
         BEGIN
           SELECT balance INTO STRICT cur FROM accounts WHERE id = src;
           IF cur < amount THEN
             status := 'insufficient';
             src_balance := cur;
             RETURN;
           END IF;
           UPDATE accounts SET balance = balance - amount WHERE id = src;
           UPDATE accounts SET balance = balance + amount WHERE id = dst;
           INSERT INTO audit VALUES ('moved ' || amount || ' from ' || src || ' to ' || dst);
           SELECT balance INTO src_balance FROM accounts WHERE id = src;
           status := 'ok';
         END $$",
    );
    let r = run(&mut c, "CALL transfer(1, 2, 30, NULL, NULL)");
    assert_eq!(r.rows, vec![vec![i(70), s("ok")]]);
    assert_eq!(r.update_count, 3);
    let r = run(&mut c, "CALL transfer(2, 1, 500)");
    assert_eq!(r.rows, vec![vec![i(80), s("insufficient")]]);
    assert_eq!(scalar(&mut c, "SELECT msg FROM audit"), s("moved 30 from 1 to 2"));
    // STRICT INTO without rows
    let e = err(&mut c, "CALL transfer(9, 1, 1)");
    assert!(e.message.contains("no rows"), "{}", e.message);
    // an error rolls back the whole CALL
    run(&mut c, "CREATE PROCEDURE bad_insert() AS $$ BEGIN INSERT INTO audit VALUES ('x'); INSERT INTO accounts VALUES (1, 'dup', 0); END $$ LANGUAGE plpgsql");
    assert!(err(&mut c, "CALL bad_insert()").message.contains("duplicate key"));
    assert_eq!(scalar(&mut c, "SELECT count(*) FROM audit"), i(1));
    // EXCEPTION handler: the block's changes are undone, the handler's kept
    run(
        &mut c,
        "CREATE PROCEDURE safe_insert(p_id INT, OUT outcome TEXT) AS $$
         BEGIN
           BEGIN
             INSERT INTO audit VALUES ('attempt ' || p_id);
             INSERT INTO accounts VALUES (p_id, 'new', 0);
             outcome := 'inserted';
           EXCEPTION WHEN unique_violation THEN
             outcome := 'duplicate: ' || SQLSTATE;
             INSERT INTO audit VALUES ('dup ' || p_id);
           END;
         END $$ LANGUAGE plpgsql",
    );
    assert_eq!(scalar(&mut c, "CALL safe_insert(1)"), s("duplicate: 23505"));
    assert_eq!(scalar(&mut c, "CALL safe_insert(3)"), s("inserted"));
    assert_eq!(scalar(&mut c, "SELECT string_agg(msg, ';') FROM audit"), s("moved 30 from 1 to 2;dup 1;attempt 3"));
    // FOR over a query with a record, GET DIAGNOSTICS, FOUND, custom ERRCODE
    run(
        &mut c,
        "CREATE FUNCTION total_balance(min_balance INT) RETURNS INT AS $$
         DECLARE r RECORD; total INT := 0; n INT;
         BEGIN
           FOR r IN SELECT id, balance FROM accounts WHERE balance >= min_balance ORDER BY id LOOP
             total := total + r.balance;
           END LOOP;
           IF NOT FOUND THEN RAISE EXCEPTION 'no account above %', min_balance USING ERRCODE = 'P0042'; END IF;
           PERFORM id FROM accounts;
           GET DIAGNOSTICS n = ROW_COUNT;
           RETURN total * 1000 + n;
         END $$ LANGUAGE plpgsql",
    );
    assert_eq!(scalar(&mut c, "SELECT total_balance(0)"), i(150003));
    let e = err(&mut c, "SELECT total_balance(1000)");
    assert!(format!("{:?}", e).contains("P0042"), "{:?}", e);
    // a function with DML: allowed with constant arguments, refused per row
    run(&mut c, "CREATE FUNCTION log_msg(m TEXT) RETURNS INT AS $$ BEGIN INSERT INTO audit VALUES (m); RETURN 1; END $$ LANGUAGE plpgsql");
    assert_eq!(scalar(&mut c, "SELECT log_msg('via select')"), i(1));
    assert_eq!(scalar(&mut c, "SELECT count(*) FROM audit WHERE msg = 'via select'"), i(1));
    assert!(err(&mut c, "SELECT log_msg(owner) FROM accounts").message.contains("data-modifying"));
    // transactions: CALL and CREATE are rolled back
    c.begin().unwrap();
    run(&mut c, "CALL transfer(1, 2, 10)");
    run(&mut c, "CREATE PROCEDURE temp_proc() AS $$ BEGIN NULL; END $$ LANGUAGE plpgsql");
    c.rollback().unwrap();
    assert_eq!(scalar(&mut c, "SELECT balance FROM accounts WHERE id = 1"), i(70));
    assert!(err(&mut c, "CALL temp_proc()").message.contains("does not exist"));
    // DO block
    run(&mut c, "DO $$ BEGIN FOR k IN 1..3 LOOP INSERT INTO audit VALUES ('do ' || k); END LOOP; END $$");
    assert_eq!(scalar(&mut c, "SELECT count(*) FROM audit WHERE msg LIKE 'do %'"), i(3));
}

#[test]
fn set_returning_functions_and_result_sets() {
    let mut c = conn();
    run(&mut c, "CREATE TABLE items (id INT, name TEXT, price DOUBLE)");
    run(&mut c, "INSERT INTO items VALUES (1, 'a', 1.5), (2, 'b', 20.0), (3, 'c', 30.0)");
    run(&mut c, "CREATE FUNCTION expensive(min_price DOUBLE) RETURNS TABLE (item_id INT, item_name TEXT) AS $$ BEGIN RETURN QUERY SELECT id, name FROM items WHERE price >= min_price ORDER BY id; END $$ LANGUAGE plpgsql");
    let r = run(&mut c, "SELECT * FROM expensive(10)");
    assert_eq!(r.columns, vec!["item_id", "item_name"]);
    assert_eq!(r.rows, vec![vec![i(2), s("b")], vec![i(3), s("c")]]);
    let r = run(&mut c, "SELECT e.item_name FROM expensive(25) e");
    assert_eq!(r.rows, vec![vec![s("c")]]);
    run(&mut c, "CREATE FUNCTION squares(n INT) RETURNS SETOF INT AS $$ BEGIN FOR k IN 1..n LOOP RETURN NEXT k * k; END LOOP; RETURN; END $$ LANGUAGE plpgsql");
    let r = run(&mut c, "SELECT * FROM squares(4)");
    assert_eq!(r.rows, vec![vec![i(1)], vec![i(4)], vec![i(9)], vec![i(16)]]);
    run(&mut c, "CREATE FUNCTION pairs() RETURNS TABLE (k INT, v TEXT) AS $$ BEGIN k := 1; v := 'one'; RETURN NEXT; k := 2; v := 'two'; RETURN NEXT; END $$ LANGUAGE plpgsql");
    assert_eq!(run(&mut c, "SELECT v FROM pairs() WHERE k = 2").rows, vec![vec![s("two")]]);
    run(&mut c, "CREATE FUNCTION all_items() RETURNS SETOF items AS $$ SELECT * FROM items ORDER BY id DESC $$ LANGUAGE sql");
    let r = run(&mut c, "SELECT name FROM all_items() LIMIT 1");
    assert_eq!(r.rows, vec![vec![s("c")]]);
    // a procedure returning a result set from a plain SELECT
    run(&mut c, "CREATE PROCEDURE list_items(max_id INT) AS $$ BEGIN SELECT id, name FROM items WHERE id <= max_id ORDER BY id; END $$ LANGUAGE plpgsql");
    let r = run(&mut c, "CALL list_items(2)");
    assert!(r.is_query);
    assert_eq!(r.columns, vec!["id", "name"]);
    assert_eq!(r.rows.len(), 2);
    // generate_series
    assert_eq!(run(&mut c, "SELECT * FROM generate_series(1, 7, 3)").rows, vec![vec![i(1)], vec![i(4)], vec![i(7)]]);
    // nesting limit
    run(&mut c, "CREATE FUNCTION forever(n INT) RETURNS INT AS $$ BEGIN RETURN forever(n + 1); END $$ LANGUAGE plpgsql");
    assert!(err(&mut c, "SELECT forever(1)").message.contains("stack depth"));
    // syntax errors in bodies are reported at CREATE time
    assert!(c.execute("CREATE FUNCTION broken() RETURNS INT AS $$ BEGIN IF THEN END $$ LANGUAGE plpgsql", &[]).is_err());
    assert!(c.execute("CREATE PROCEDURE p() RETURNS INT AS $$ BEGIN END $$", &[]).is_err());
}
