//! `scheduler:` component - generates message exchanges using a scheduled poll consumer.
//!
//! `scheduler:name?initialDelay=1000&delay=500&useFixedDelay=true&repeatCount=0&greedy=false&poolSize=1&timeUnit=MILLISECONDS`
//!
//! Each poll sends an exchange with a null body and the properties `CamelTimerName`
//! and `CamelTimerFiredTime`. A route may set the exchange property
//! `CamelSchedulerPolledMessages=false` to tell the scheduler that nothing was polled
//! (affects `greedy` and the backoff options).

use std::any::Any;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::context::{CamelContext, WeakContext};
use crate::error::Result;
use crate::message::props;
use crate::spi::{Component, Consumer, Endpoint, Processor, Service};
use crate::util::Parameters;
use crate::value::Value;

use super::scheduled::{ScheduledPollConfig, ScheduledPollConsumer};
use super::support::{new_exchange, process_consumer_exchange};

/// Property a route can set to `false` to signal that the poll found nothing.
pub const SCHEDULER_POLLED_MESSAGES: &str = "CamelSchedulerPolledMessages";

#[derive(Default)]
pub struct SchedulerComponent;

impl Component for SchedulerComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let config = ScheduledPollConfig::from_params(params, Duration::from_millis(1000), Duration::from_millis(500))?;
        params.take("poolSize");
        params.take("synchronous");
        Ok(Arc::new(SchedulerEndpoint { uri: uri.to_string(), name: remaining.to_string(), ctx: ctx.downgrade(), config }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A scheduler endpoint (consumer only).
pub struct SchedulerEndpoint {
    uri: String,
    name: String,
    ctx: WeakContext,
    config: ScheduledPollConfig,
}

impl SchedulerEndpoint {
    pub fn config(&self) -> &ScheduledPollConfig {
        &self.config
    }
}

/// Consumer handle so that callers (e.g. tests with `startScheduler=false`) can trigger polls.
pub struct SchedulerConsumer {
    poll: ScheduledPollConsumer,
}

impl SchedulerConsumer {
    pub fn poll_once(&self) -> Result<usize> {
        self.poll.poll_once()
    }
}

impl Service for SchedulerConsumer {
    fn start(&self) -> Result<()> {
        self.poll.start()
    }
    fn stop(&self) -> Result<()> {
        self.poll.stop()
    }
}

impl Endpoint for SchedulerEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        let (name, ctx) = (self.name.clone(), self.ctx.clone());
        let logger = format!("scheduler://{}", self.name);
        let counter = Arc::new(Mutex::new(0u64));
        let poll = ScheduledPollConsumer::new(logger.clone(), self.config.clone(), move || {
            let mut ex = new_exchange(&ctx);
            let n = {
                let mut c = counter.lock().unwrap();
                *c += 1;
                *c
            };
            ex.set_property(props::TIMER_NAME, name.clone());
            ex.set_property(props::TIMER_FIRED_TIME, Value::from(crate::util_time::now_millis()));
            ex.set_property(props::TIMER_COUNTER, Value::from(n));
            process_consumer_exchange(&logger, processor.as_ref(), &mut ex);
            let polled = ex.property(SCHEDULER_POLLED_MESSAGES).and_then(|v| v.as_bool()).unwrap_or(true);
            Ok(if polled { 1 } else { 0 })
        });
        Ok(Arc::new(SchedulerConsumer { poll }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
