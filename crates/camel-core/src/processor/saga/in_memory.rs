//! In-memory saga service (Camel `InMemorySagaService`): coordinators live in the JVM -
//! here, the process. Not durable; compensation/completion calls are retried
//! `max_retry_attempts` times with `retry_delay` between attempts.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use crate::context::{CamelContext, WeakContext};
use crate::error::{CamelError, Result};
use crate::message::{generate_id, Exchange};
use crate::value::Value;

use super::{send_finalization, CamelSagaCoordinator, CamelSagaService, CamelSagaStep};

/// Status of an in-memory saga.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SagaStatus {
    Running,
    Compensating,
    Compensated,
    Completing,
    Completed,
}

struct Shared {
    sagas: Mutex<HashMap<String, Arc<InMemorySagaCoordinator>>>,
    ctx: WeakContext,
    max_retry_attempts: Mutex<u32>,
    retry_delay: Mutex<Duration>,
}

/// The default saga service. Cheap to clone (shared state).
#[derive(Clone)]
pub struct InMemorySagaService {
    shared: Arc<Shared>,
}

impl InMemorySagaService {
    /// A service sending finalization messages through `ctx`. Defaults as in Camel:
    /// 5 retry attempts, 5 seconds between attempts.
    pub fn new(ctx: &CamelContext) -> Self {
        InMemorySagaService {
            shared: Arc::new(Shared {
                sagas: Mutex::new(HashMap::new()),
                ctx: ctx.downgrade(),
                max_retry_attempts: Mutex::new(5),
                retry_delay: Mutex::new(Duration::from_millis(5000)),
            }),
        }
    }

    /// Number of attempts for each compensation/completion call (minimum 1).
    pub fn with_max_retry_attempts(self, n: u32) -> Self {
        *self.shared.max_retry_attempts.lock().unwrap() = n.max(1);
        self
    }

    /// Delay between attempts.
    pub fn with_retry_delay(self, d: Duration) -> Self {
        *self.shared.retry_delay.lock().unwrap() = d;
        self
    }

    /// Ids of the sagas that are still running or finalizing.
    pub fn active_sagas(&self) -> Vec<String> {
        self.shared.sagas.lock().unwrap().keys().cloned().collect()
    }

    /// Status of a saga known to this service.
    pub fn status(&self, id: &str) -> Option<SagaStatus> {
        self.shared.sagas.lock().unwrap().get(id).map(|c| c.state.lock().unwrap().status)
    }
}

impl CamelSagaService for InMemorySagaService {
    fn new_saga(&self, _exchange: &Exchange) -> Result<Arc<dyn CamelSagaCoordinator>> {
        let id = generate_id();
        let c = Arc::new(InMemorySagaCoordinator {
            id: id.clone(),
            shared: Arc::downgrade(&self.shared),
            state: Mutex::new(State { status: SagaStatus::Running, steps: Vec::new() }),
            changed: Condvar::new(),
        });
        self.shared.sagas.lock().unwrap().insert(id, c.clone());
        Ok(c)
    }

    fn get_saga(&self, id: &str) -> Result<Option<Arc<dyn CamelSagaCoordinator>>> {
        Ok(self.shared.sagas.lock().unwrap().get(id).map(|c| c.clone() as Arc<dyn CamelSagaCoordinator>))
    }
}

struct State {
    status: SagaStatus,
    /// Registered steps with their option values (evaluated at begin).
    steps: Vec<(CamelSagaStep, BTreeMap<String, Value>)>,
}

/// Coordinator of one in-memory saga.
pub struct InMemorySagaCoordinator {
    id: String,
    shared: Weak<Shared>,
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Compensation,
    Completion,
}

impl InMemorySagaCoordinator {
    fn finalize(&self, phase: Phase) -> Result<()> {
        let (from, during, done) = match phase {
            Phase::Compensation => (SagaStatus::Running, SagaStatus::Compensating, SagaStatus::Compensated),
            Phase::Completion => (SagaStatus::Running, SagaStatus::Completing, SagaStatus::Completed),
        };
        let steps = {
            let mut st = self.state.lock().unwrap();
            if st.status != from {
                let same_direction = st.status == during || st.status == done;
                if same_direction {
                    return Ok(());
                }
                let what = if phase == Phase::Compensation { "compensate" } else { "complete" };
                return Err(CamelError::illegal_state(format!("Cannot {} saga {}: status is {:?}", what, self.id, st.status)));
            }
            st.status = during;
            st.steps.clone()
        };
        self.changed.notify_all();
        let shared = self.shared.upgrade();
        let ctx = shared.as_ref().and_then(|s| s.ctx.upgrade());
        let (attempts, delay) = match &shared {
            Some(s) => (*s.max_retry_attempts.lock().unwrap(), *s.retry_delay.lock().unwrap()),
            None => (1, Duration::ZERO),
        };
        let mut failures = Vec::new();
        if let Some(ctx) = ctx {
            for (step, values) in steps.iter().rev() {
                let uri = match phase {
                    Phase::Compensation => step.compensation.as_ref(),
                    Phase::Completion => step.completion.as_ref(),
                };
                let Some(uri) = uri else { continue };
                let mut last_err = None;
                for attempt in 0..attempts {
                    if attempt > 0 {
                        std::thread::sleep(delay);
                    }
                    match send_finalization(&ctx, uri, &self.id, values) {
                        Ok(()) => {
                            last_err = None;
                            break;
                        }
                        Err(e) => last_err = Some(e),
                    }
                }
                if let Some(e) = last_err {
                    crate::camel_error!("saga", "Cannot call {} of saga {} on {}: {}", phase_name(phase), self.id, uri, e);
                    failures.push(e);
                }
            }
        }
        self.state.lock().unwrap().status = done;
        self.changed.notify_all();
        if let Some(s) = shared {
            s.sagas.lock().unwrap().remove(&self.id);
        }
        if let Some(e) = failures.into_iter().next() {
            crate::camel_warn!("saga", "Saga {} finished its {} with errors: {}", self.id, phase_name(phase), e);
        }
        Ok(())
    }

    fn schedule_timeout(self: &Arc<Self>, timeout: Duration) {
        let me = Arc::downgrade(self);
        std::thread::Builder::new()
            .name(format!("saga-timeout-{}", self.id))
            .spawn(move || {
                let deadline = Instant::now() + timeout;
                let Some(c) = me.upgrade() else { return };
                let mut st = c.state.lock().unwrap();
                while st.status == SagaStatus::Running {
                    let now = Instant::now();
                    if now >= deadline {
                        drop(st);
                        crate::camel_warn!("saga", "Saga {} timed out after {:?}: compensating", c.id, timeout);
                        let _ = c.finalize(Phase::Compensation);
                        return;
                    }
                    st = c.changed.wait_timeout(st, deadline - now).unwrap().0;
                }
            })
            .ok();
    }
}

fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Compensation => "compensation",
        Phase::Completion => "completion",
    }
}

impl CamelSagaCoordinator for InMemorySagaCoordinator {
    fn id(&self) -> String {
        self.id.clone()
    }

    fn begin_step(&self, exchange: &Exchange, step: &CamelSagaStep) -> Result<()> {
        let values = step.evaluate_options(exchange)?;
        {
            let mut st = self.state.lock().unwrap();
            if st.status != SagaStatus::Running {
                return Err(CamelError::illegal_state(format!("Cannot begin a step of saga {}: status is {:?}", self.id, st.status)));
            }
            st.steps.push((step.clone(), values));
        }
        if let Some(t) = step.timeout {
            // the timer needs an Arc of this coordinator: find it in the service
            if let Some(shared) = self.shared.upgrade() {
                if let Some(me) = shared.sagas.lock().unwrap().get(&self.id).cloned() {
                    me.schedule_timeout(t);
                }
            }
        }
        Ok(())
    }

    fn compensate(&self, _exchange: &Exchange) -> Result<()> {
        self.finalize(Phase::Compensation)
    }

    fn complete(&self, _exchange: &Exchange) -> Result<()> {
        self.finalize(Phase::Completion)
    }
}
