//! transacted(): transaction manager SPI, propagation, rollback rules; threads EIP options.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cameleer_core::executor::RejectedPolicy;
use cameleer_core::prelude::*;
use cameleer_core::processor::errors::ThreadsOptions;
use cameleer_core::transaction::{
    current_transaction, CompletionStatus, Propagation, TransactionDefinition, TransactionManager, TransactionPolicy, TransactionSynchronization,
    TxResource,
};

/// A manager recording begin/commit/rollback/savepoint events.
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<String>>>);

impl Recorder {
    fn events(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
    fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

impl TransactionManager for Recorder {
    fn begin(&self, d: &TransactionDefinition) -> Result<TxResource> {
        let mut e = self.0.lock().unwrap();
        let n = e.iter().filter(|x| x.starts_with("begin")).count() + 1;
        e.push(format!("begin{}", n));
        let _ = d;
        Ok(Box::new(n))
    }
    fn commit(&self, tx: &TxResource) -> Result<()> {
        self.0.lock().unwrap().push(format!("commit{}", tx.downcast_ref::<usize>().unwrap()));
        Ok(())
    }
    fn rollback(&self, tx: &TxResource) -> Result<()> {
        self.0.lock().unwrap().push(format!("rollback{}", tx.downcast_ref::<usize>().unwrap()));
        Ok(())
    }
    fn create_savepoint(&self, _tx: &TxResource) -> Result<TxResource> {
        self.0.lock().unwrap().push("savepoint".into());
        Ok(Box::new(()))
    }
    fn rollback_to_savepoint(&self, _tx: &TxResource, _sp: &TxResource) -> Result<()> {
        self.0.lock().unwrap().push("rollback-to-savepoint".into());
        Ok(())
    }
    fn release_savepoint(&self, _tx: &TxResource, _sp: &TxResource) -> Result<()> {
        self.0.lock().unwrap().push("release-savepoint".into());
        Ok(())
    }
    fn suspend(&self, tx: &TxResource) -> Result<()> {
        self.0.lock().unwrap().push(format!("suspend{}", tx.downcast_ref::<usize>().unwrap()));
        Ok(())
    }
    fn resume(&self, tx: &TxResource) -> Result<()> {
        self.0.lock().unwrap().push(format!("resume{}", tx.downcast_ref::<usize>().unwrap()));
        Ok(())
    }
}

fn setup() -> (CamelContext, Recorder) {
    let ctx = CamelContext::new();
    let rec = Recorder::default();
    ctx.registry().bind_transaction_manager("transactionManager", rec.clone());
    (ctx, rec)
}

#[test]
fn transacted_commits_and_rolls_back() {
    let (ctx, rec) = setup();
    ctx.add_routes(|b| {
        b.from("direct:tx").transacted().process(|ex: &mut Exchange| {
            assert!(current_transaction().is_some());
            if ex.body().to_string() == "fail" {
                return Err(CamelError::illegal_state("boom"));
            }
            Ok(())
        });
        b.from("direct:mark").transacted().mark_rollback_only().to("mock:never");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:tx", "ok").unwrap();
    assert_eq!(rec.events(), vec!["begin1", "commit1"]);
    rec.clear();
    let err = t.send_body("direct:tx", "fail").unwrap_err();
    assert!(err.to_string().contains("boom"));
    assert_eq!(rec.events(), vec!["begin1", "rollback1"]);
    rec.clear();
    let _ = t.send_body("direct:mark", "x");
    assert_eq!(rec.events(), vec!["begin1", "rollback1"]);
    assert_eq!(ctx.mock_endpoint("mock:never").unwrap().received_counter(), 0);
    assert!(current_transaction().is_none());
    ctx.stop().unwrap();
}

#[test]
fn propagation_behaviours() {
    let (ctx, rec) = setup();
    ctx.registry().bind("PROPAGATION_REQUIRES_NEW", TransactionPolicy::new(Arc::new(rec.clone()), Propagation::RequiresNew));
    ctx.registry().bind("PROPAGATION_REQUIRED", TransactionPolicy::new(Arc::new(rec.clone()), Propagation::Required));
    ctx.add_routes(|b| {
        b.from("direct:outer").transacted().to("direct:required").to("direct:new").to("direct:nested").to("direct:notsupported");
        b.from("direct:required").transacted().process(|_ex: &mut Exchange| Ok(()));
        b.from("direct:new").transacted_ref("PROPAGATION_REQUIRES_NEW").process(|_ex: &mut Exchange| Ok(()));
        b.from("direct:nested").transacted_ref("PROPAGATION_NESTED").process(|_ex: &mut Exchange| Ok(()));
        b.from("direct:notsupported").transacted_ref("PROPAGATION_NOT_SUPPORTED").process(|_ex: &mut Exchange| {
            assert!(current_transaction().is_none());
            Ok(())
        });
        b.from("direct:mandatory").transacted_ref("PROPAGATION_MANDATORY").to("mock:m");
        b.from("direct:never").transacted().to("direct:never2");
        b.from("direct:never2").transacted_ref("PROPAGATION_NEVER").to("mock:n");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:outer", "x").unwrap();
    assert_eq!(
        rec.events(),
        vec!["begin1", "suspend1", "begin2", "commit2", "resume1", "savepoint", "release-savepoint", "suspend1", "resume1", "commit1"]
    );
    rec.clear();
    let e = t.send_body("direct:mandatory", "x").unwrap_err();
    assert!(e.to_string().contains("mandatory"), "{}", e);
    let e = t.send_body("direct:never", "x").unwrap_err();
    assert!(e.to_string().contains("never"), "{}", e);
    assert_eq!(rec.events(), vec!["begin1", "rollback1"]);
    ctx.stop().unwrap();
}

#[test]
fn participating_failure_rolls_back_outer_and_rollback_only_last() {
    let (ctx, rec) = setup();
    ctx.add_routes(|b| {
        b.from("direct:outer").transacted().to("direct:inner").to("mock:after");
        b.from("direct:inner").transacted().throw_exception("IllegalStateException", "inner failed");
        b.from("direct:outer2").transacted().to("direct:inner2").to("mock:after2");
        b.from("direct:inner2").transacted_ref("PROPAGATION_REQUIRES_NEW").mark_rollback_only_last().to("mock:skipped");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    assert!(t.send_body("direct:outer", "x").is_err());
    assert_eq!(rec.events(), vec!["begin1", "rollback1"]);
    assert_eq!(ctx.mock_endpoint("mock:after").unwrap().received_counter(), 0);
    rec.clear();
    t.send_body("direct:outer2", "x").unwrap();
    assert_eq!(rec.events(), vec!["begin1", "suspend1", "begin2", "rollback2", "resume1", "commit1"]);
    assert_eq!(ctx.mock_endpoint("mock:skipped").unwrap().received_counter(), 0);
    assert_eq!(ctx.mock_endpoint("mock:after2").unwrap().received_counter(), 1);
    ctx.stop().unwrap();
}

struct Sync(Arc<Mutex<Vec<String>>>);
impl TransactionSynchronization for Sync {
    fn before_commit(&self) -> Result<()> {
        self.0.lock().unwrap().push("before".into());
        Ok(())
    }
    fn after_completion(&self, s: CompletionStatus) {
        self.0.lock().unwrap().push(format!("{:?}", s));
    }
}

#[test]
fn template_api_synchronizations_and_timeout() {
    let rec = Recorder::default();
    let policy = TransactionPolicy::required(Arc::new(rec.clone()));
    let log = Arc::new(Mutex::new(Vec::new()));
    let l2 = log.clone();
    let v = policy
        .execute(|| {
            current_transaction().unwrap().register_synchronization(Arc::new(Sync(l2)));
            Ok(42)
        })
        .unwrap();
    assert_eq!(v, 42);
    assert_eq!(*log.lock().unwrap(), vec!["before", "Committed"]);
    let r: Result<()> = policy.execute(|| Err(CamelError::illegal_state("x")));
    assert!(r.is_err());
    assert_eq!(rec.events(), vec!["begin1", "commit1", "begin2", "rollback2"]);
    rec.clear();
    let slow = TransactionPolicy::required(Arc::new(rec.clone())).with_timeout(Duration::from_millis(1));
    let e = slow
        .execute(|| {
            std::thread::sleep(Duration::from_millis(20));
            Ok(())
        })
        .unwrap_err();
    assert_eq!(e.kind, "TransactionTimedOutException");
    assert_eq!(rec.events(), vec!["begin1", "rollback1"]);
}

#[test]
fn transacted_without_manager_fails_route_creation() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:x").transacted().to("mock:x");
    })
    .unwrap();
    assert!(ctx.start().is_err());
}

#[test]
fn threads_options_and_rejection() {
    let ctx = CamelContext::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let c2 = calls.clone();
    ctx.add_routes(move |b| {
        b.from("direct:abort")
            .threads_with(ThreadsOptions::new().pool_size(1).max_pool_size(1).max_queue_size(0).rejected_policy(RejectedPolicy::Abort).thread_name("Busy"))
            .process(move |ex: &mut Exchange| {
                c2.fetch_add(1, Ordering::SeqCst);
                ex.set_header("thread", std::thread::current().name().unwrap_or("").to_string());
                std::thread::sleep(Duration::from_millis(200));
                Ok(())
            });
        b.from("direct:callerruns")
            .threads_with(ThreadsOptions::new().pool_size(1).max_pool_size(1).max_queue_size(0))
            .process(|ex: &mut Exchange| {
                ex.set_header("thread", std::thread::current().name().unwrap_or("").to_string());
                std::thread::sleep(Duration::from_millis(100));
                Ok(())
            });
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let t2 = ctx.create_producer_template();
    let h = std::thread::spawn(move || t2.request("direct:abort", |ex| ex.set_body("a")).unwrap());
    std::thread::sleep(Duration::from_millis(50));
    let e = t.request("direct:abort", |ex| ex.set_body("b")).unwrap_err();
    assert_eq!(e.cause.as_deref().unwrap_or(&e).kind, "RejectedExecutionException");
    let ex = h.join().unwrap();
    assert!(ex.header("thread").unwrap().to_string().starts_with("Busy"));
    // CallerRuns (default fallback): the second exchange runs in the caller thread
    let t3 = ctx.create_producer_template();
    let h = std::thread::spawn(move || t3.request("direct:callerruns", |ex| ex.set_body("a")).unwrap());
    std::thread::sleep(Duration::from_millis(30));
    std::thread::Builder::new()
        .name("caller-thread".into())
        .spawn(move || {
            let ex = t.request("direct:callerruns", |ex| ex.set_body("b")).unwrap();
            assert_eq!(ex.header("thread").unwrap().to_string(), "caller-thread");
        })
        .unwrap()
        .join()
        .unwrap();
    h.join().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    ctx.stop().unwrap();
}
