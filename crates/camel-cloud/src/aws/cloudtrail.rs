//! The `aws-cloudtrail` component (Camel `camel-aws-cloudtrail`, polling consumer) over the
//! CloudTrail AWS JSON 1.1 protocol (signing name `cloudtrail`).
//!
//! `aws-cloudtrail://label?eventSource=s3.amazonaws.com&maxResults=10`
//!
//! Each poll calls `LookupEvents` (filtered by `eventSource` when set) from the time of the
//! newest event already seen (all events of the trail on the first poll), up to `maxResults`
//! (default 1) events per poll, oldest first; events already delivered are skipped. Body = the
//! CloudTrail event JSON document (bytes), headers `CamelAwsCloudTrailEventId`,
//! `CamelAwsCloudTrailEventSource`, `CamelAwsCloudTrailEventName`,
//! `CamelAwsCloudTrailEventUsername`, `CamelAwsCloudTrailEventTime` (epoch millis).

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::component::scheduled::ScheduledPollConfig;
use camel_core::context::CamelContext;
use camel_core::error::Result;
use camel_core::message::Exchange;
use camel_core::spi::Processor;
use camel_core::value::{ObjectMap, Value};

use super::{put, AwsConfig, JsonApi};
use crate::endpoint::{process, set_batch, CloudService};
use crate::util::{kinds, obj, parse_iso8601, Opts};

/// Header names (Camel `CloudtrailConstants`).
pub mod headers {
    pub const EVENT_ID: &str = "CamelAwsCloudTrailEventId";
    pub const EVENT_SOURCE: &str = "CamelAwsCloudTrailEventSource";
    pub const EVENT_NAME: &str = "CamelAwsCloudTrailEventName";
    pub const USERNAME: &str = "CamelAwsCloudTrailEventUsername";
    pub const EVENT_TIME: &str = "CamelAwsCloudTrailEventTime";
}

const API: JsonApi = JsonApi { host: "cloudtrail", service: "cloudtrail", target: "com.amazonaws.cloudtrail.v20131101.CloudTrail_20131101", version: "1.1", kind: kinds::CLOUDTRAIL };

/// The CloudTrail consumer configuration and state.
pub struct CloudTrailService {
    pub aws: AwsConfig,
    pub event_source: Option<String>,
    pub max_results: i64,
    pub scheduler: ScheduledPollConfig,
    /// (newest event time in millis, ids of the events delivered at that time).
    last: Mutex<(Option<i64>, HashSet<String>)>,
}

/// Factory for the component.
pub fn create(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["cloudTrailClient"]);
    Ok(Arc::new(CloudTrailService {
        event_source: o.get("eventSource"),
        max_results: o.i64("maxResults", 1)?.max(1),
        scheduler: ScheduledPollConfig::from_params(o.params, Duration::from_millis(1000), Duration::from_millis(500))?,
        last: Mutex::new((None, HashSet::new())),
        aws,
    }))
}

fn event_millis(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Int(i)) => i * 1000,
        Some(Value::Float(f)) => (f * 1000.0) as i64,
        Some(other) => other.to_string().parse::<f64>().map(|f| (f * 1000.0) as i64).unwrap_or_else(|_| parse_iso8601(&other.to_string()).unwrap_or(0) * 1000),
        None => 0,
    }
}

impl CloudService for CloudTrailService {
    fn produce(&self, _ex: &mut Exchange) -> Result<()> {
        Err(camel_core::error::CamelError::new(camel_core::error::kinds::UNSUPPORTED_OPERATION, "aws-cloudtrail only supports consumers"))
    }

    fn scheduler(&self) -> Option<&ScheduledPollConfig> {
        Some(&self.scheduler)
    }

    fn poll(&self, ctx: Option<CamelContext>, processor: &dyn Processor, logger: &str) -> Result<usize> {
        let (since, seen) = self.last.lock().unwrap().clone();
        let mut req = ObjectMap::new();
        if let Some(src) = &self.event_source {
            req.insert("LookupAttributes".into(), Value::Array(vec![obj([("AttributeKey", Value::from("EventSource")), ("AttributeValue", Value::from(src.clone()))])]));
        }
        put(&mut req, "StartTime", since.map(|ms| Value::Float(ms as f64 / 1000.0)));
        // read every page, then deliver the oldest undelivered events first
        let mut events = self.aws.api_paged(&API, "LookupEvents", req, "Events", "NextToken")?;
        events.retain(|e| !seen.contains(&e.get("EventId").map(|i| i.to_string()).unwrap_or_default()));
        events.sort_by_key(|e| event_millis(e.get("EventTime")));
        events.truncate(self.max_results as usize);
        let total = events.len();
        for (i, e) in events.iter().enumerate() {
            let id = e.get("EventId").map(|i| i.to_string()).unwrap_or_default();
            let t = event_millis(e.get("EventTime"));
            let mut ex = Exchange::new(ctx.clone());
            ex.set_body(Value::Bytes(e.get("CloudTrailEvent").map(|c| c.to_string()).unwrap_or_default().into_bytes()));
            ex.set_header(headers::EVENT_ID, id.clone());
            ex.set_header(headers::EVENT_SOURCE, e.get("EventSource").cloned().unwrap_or_default());
            ex.set_header(headers::EVENT_NAME, e.get("EventName").cloned().unwrap_or_default());
            ex.set_header(headers::USERNAME, e.get("Username").cloned().unwrap_or_default());
            ex.set_header(headers::EVENT_TIME, t);
            set_batch(&mut ex, i, total);
            process(logger, processor, &mut ex);
            let mut last = self.last.lock().unwrap();
            if last.0 != Some(t) && last.0.is_none_or(|l| t > l) {
                *last = (Some(t), HashSet::new());
            }
            last.1.insert(id);
        }
        Ok(total)
    }
}
