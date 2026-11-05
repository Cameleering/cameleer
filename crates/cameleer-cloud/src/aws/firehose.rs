//! The `aws2-kinesis-firehose` component (Camel `camel-aws2-kinesis-firehose`, producer) over
//! the Firehose AWS JSON 1.1 protocol.
//!
//! `aws2-kinesis-firehose://deliveryStreamName?operation=...`
//!
//! Operations (`operation` / `CamelAwsKinesisFirehoseOperation`): `sendRecord` (default;
//! `PutRecord` of the body, sets `CamelAwsKinesisFirehoseRecordId`), `putRecordBatch` (array body,
//! one record per element; the body becomes the list of record ids / errors and
//! `CamelAwsKinesisFirehoseFailedPutCount` is set; an array body defaults to this operation),
//! `createDeliveryStream` (DirectPut; body = optional request JSON merged in),
//! `describeDeliveryStream`, `deleteDeliveryStream`. The stream name may be overridden with
//! `CamelAwsKinesisFirehoseDeliveryStreamName`.

use std::sync::Arc;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::value::Value;

use super::AwsConfig;
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{base64_encode, header_str, kinds, obj, Opts};

/// Header names (Camel `KinesisFirehose2Constants`).
pub mod headers {
    pub const RECORD_ID: &str = "CamelAwsKinesisFirehoseRecordId";
    pub const OPERATION: &str = "CamelAwsKinesisFirehoseOperation";
    pub const DELIVERY_STREAM_NAME: &str = "CamelAwsKinesisFirehoseDeliveryStreamName";
    /// Cameleer extension.
    pub const FAILED_PUT_COUNT: &str = "CamelAwsKinesisFirehoseFailedPutCount";
}

/// The Firehose endpoint configuration and client.
pub struct FirehoseService {
    pub aws: AwsConfig,
    pub stream: String,
    pub operation: Option<String>,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let stream = remaining.rsplit('/').next().unwrap_or(remaining).to_string();
    if stream.is_empty() {
        return Err(CamelError::illegal_argument("Delivery stream name must be specified: aws2-kinesis-firehose://streamName"));
    }
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["cborEnabled", "amazonKinesisFirehoseClient"]);
    Ok(Arc::new(FirehoseService { stream, operation: o.get("operation"), aws }))
}

impl FirehoseService {
    /// Invoke a Firehose API action.
    pub fn call(&self, action: &str, body: Value) -> Result<Value> {
        self.aws.json_call("firehose", "firehose", "Firehose_20150804", action, &body, "1.1", kinds::FIREHOSE)
    }
}

fn record(v: &Value) -> Value {
    obj([("Data", Value::from(base64_encode(&v.to_bytes())))])
}

impl CloudService for FirehoseService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let default = if matches!(ex.body(), Value::Array(_)) { "putRecordBatch" } else { "sendRecord" };
        let op = operation(ex, headers::OPERATION, &self.operation, default);
        let name = Value::from(header_str(ex, headers::DELIVERY_STREAM_NAME).unwrap_or_else(|| self.stream.clone()));
        match op.as_str() {
            "sendRecord" => {
                let r = self.call("PutRecord", obj([("DeliveryStreamName", name), ("Record", record(ex.body()))]))?;
                ex.set_header(headers::RECORD_ID, r.get("RecordId").cloned().unwrap_or_default());
            }
            "putRecordBatch" => {
                let items: Vec<Value> = match ex.body() {
                    Value::Array(a) => a.iter().map(record).collect(),
                    other => vec![record(other)],
                };
                let r = self.call("PutRecordBatch", obj([("DeliveryStreamName", name), ("Records", Value::Array(items))]))?;
                ex.set_header(headers::FAILED_PUT_COUNT, r.get("FailedPutCount").and_then(|c| c.as_i64()).unwrap_or(0));
                let out: Vec<Value> = r
                    .get("RequestResponses")
                    .and_then(|a| a.as_array())
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|e| obj([("recordId", e.get("RecordId").cloned().unwrap_or_default()), ("errorCode", e.get("ErrorCode").cloned().unwrap_or_default())]))
                    .collect();
                ex.set_body(Value::Array(out));
            }
            "createDeliveryStream" => {
                let mut req = obj([("DeliveryStreamName", name), ("DeliveryStreamType", Value::from("DirectPut"))]);
                if let (Some(Value::Object(extra)), Value::Object(m)) = (crate::util::parse_json(&ex.body().to_bytes()).ok(), &mut req) {
                    m.extend(extra);
                }
                let r = self.call("CreateDeliveryStream", req)?;
                ex.set_body(r.get("DeliveryStreamARN").cloned().unwrap_or_default());
            }
            "describeDeliveryStream" => {
                let r = self.call("DescribeDeliveryStream", obj([("DeliveryStreamName", name)]))?;
                ex.set_body(r.get("DeliveryStreamDescription").cloned().unwrap_or_default());
            }
            "deleteDeliveryStream" => {
                self.call("DeleteDeliveryStream", obj([("DeliveryStreamName", name)]))?;
            }
            other => return Err(unsupported("aws2-kinesis-firehose", other)),
        }
        Ok(())
    }
}
