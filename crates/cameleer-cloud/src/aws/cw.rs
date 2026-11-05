//! The `aws2-cw` component (Camel `camel-aws2-cw`, producer) over the CloudWatch Query API.
//!
//! `aws2-cw://namespace?name=requests&value=1&unit=Count`
//!
//! Each exchange is one `PutMetricData` call. The datum comes from the headers
//! `CamelAwsCwMetricName`, `CamelAwsCwMetricValue` (default 1.0), `CamelAwsCwMetricUnit`
//! (default `Count`), `CamelAwsCwMetricTimestamp` (epoch millis or ISO 8601; default now),
//! `CamelAwsCwMetricDimensionName` + `CamelAwsCwMetricDimensionValue` and
//! `CamelAwsCwMetricDimensions` (map name → value), falling back to the options `name`,
//! `value`, `unit`, `timestamp`. `CamelAwsCwMetricNamespace` overrides the namespace. An
//! array body sends several data, each an object `{name, value, unit, timestamp, dimensions}`.

use std::collections::BTreeMap;
use std::sync::Arc;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::value::Value;

use super::AwsConfig;
use crate::endpoint::CloudService;
use crate::util::{header_str, iso8601_millis, kinds, now_millis, parse_iso8601, Opts};

/// Header names (Camel `Cw2Constants`).
pub mod headers {
    pub const METRIC_NAMESPACE: &str = "CamelAwsCwMetricNamespace";
    pub const METRIC_NAME: &str = "CamelAwsCwMetricName";
    pub const METRIC_VALUE: &str = "CamelAwsCwMetricValue";
    pub const METRIC_UNIT: &str = "CamelAwsCwMetricUnit";
    pub const METRIC_TIMESTAMP: &str = "CamelAwsCwMetricTimestamp";
    pub const METRIC_DIMENSIONS: &str = "CamelAwsCwMetricDimensions";
    pub const METRIC_DIMENSION_NAME: &str = "CamelAwsCwMetricDimensionName";
    pub const METRIC_DIMENSION_VALUE: &str = "CamelAwsCwMetricDimensionValue";
}

/// The CloudWatch endpoint configuration and client.
pub struct CwService {
    pub aws: AwsConfig,
    pub namespace: String,
    pub name: Option<String>,
    pub value: Option<f64>,
    pub unit: Option<String>,
    pub timestamp: Option<String>,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    if remaining.is_empty() {
        return Err(CamelError::illegal_argument("Namespace must be specified: aws2-cw://namespace"));
    }
    let aws = AwsConfig::from_opts(o)?;
    let value = o.get("value").map(|v| v.parse::<f64>().map_err(|_| CamelError::illegal_argument(format!("Invalid metric value: {}", v)))).transpose()?;
    o.ignore(&["amazonCwClient"]);
    Ok(Arc::new(CwService { namespace: remaining.to_string(), name: o.get("name"), value, unit: o.get("unit"), timestamp: o.get("timestamp"), aws }))
}

/// One metric datum.
#[derive(Clone, Debug, PartialEq)]
pub struct Datum {
    pub name: String,
    pub value: f64,
    pub unit: String,
    pub timestamp_millis: i64,
    pub dimensions: Vec<(String, String)>,
}

fn timestamp_of(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| parse_iso8601(&v.to_string()).map(|s| s * 1000))
}

fn dimensions_of(v: Option<&Value>) -> Vec<(String, String)> {
    match v {
        Some(Value::Object(m)) => m.iter().map(|(k, v)| (k.clone(), v.to_string())).collect(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|d| Some((d.get("name").or_else(|| d.get("Name"))?.to_string(), d.get("value").or_else(|| d.get("Value"))?.to_string())))
            .collect(),
        _ => Vec::new(),
    }
}

impl CwService {
    fn datum_from_headers(&self, ex: &Exchange) -> Result<Datum> {
        let name = header_str(ex, headers::METRIC_NAME)
            .or_else(|| self.name.clone())
            .ok_or_else(|| CamelError::illegal_argument("Metric name must be specified (name option or CamelAwsCwMetricName header)"))?;
        let value = ex.header(headers::METRIC_VALUE).and_then(|v| v.as_f64()).or(self.value).unwrap_or(1.0);
        let unit = header_str(ex, headers::METRIC_UNIT).or_else(|| self.unit.clone()).unwrap_or_else(|| "Count".into());
        let ts = ex.header(headers::METRIC_TIMESTAMP).and_then(timestamp_of).or_else(|| self.timestamp.as_ref().and_then(|t| timestamp_of(&Value::from(t.clone())))).unwrap_or_else(now_millis);
        let mut dims = dimensions_of(ex.header(headers::METRIC_DIMENSIONS));
        if let (Some(n), Some(v)) = (header_str(ex, headers::METRIC_DIMENSION_NAME), header_str(ex, headers::METRIC_DIMENSION_VALUE)) {
            dims.push((n, v));
        }
        Ok(Datum { name, value, unit, timestamp_millis: ts, dimensions: dims })
    }

    fn datum_from_value(&self, v: &Value) -> Result<Datum> {
        let get = |a: &str, b: &str| v.get(a).or_else(|| v.get(b));
        Ok(Datum {
            name: get("name", "MetricName").map(|n| n.to_string()).or_else(|| self.name.clone()).ok_or_else(|| CamelError::illegal_argument("Metric datum without name"))?,
            value: get("value", "Value").and_then(|x| x.as_f64()).or(self.value).unwrap_or(1.0),
            unit: get("unit", "Unit").map(|u| u.to_string()).or_else(|| self.unit.clone()).unwrap_or_else(|| "Count".into()),
            timestamp_millis: get("timestamp", "Timestamp").and_then(timestamp_of).unwrap_or_else(now_millis),
            dimensions: dimensions_of(get("dimensions", "Dimensions")),
        })
    }
}

/// Query parameters of a `PutMetricData` request.
pub fn put_metric_params(namespace: &str, data: &[Datum]) -> Vec<(String, String)> {
    let mut p = vec![("Namespace".to_string(), namespace.to_string())];
    for (i, d) in data.iter().enumerate() {
        let m = format!("MetricData.member.{}", i + 1);
        p.push((format!("{}.MetricName", m), d.name.clone()));
        p.push((format!("{}.Value", m), d.value.to_string()));
        p.push((format!("{}.Unit", m), d.unit.clone()));
        p.push((format!("{}.Timestamp", m), iso8601_millis(d.timestamp_millis)));
        let dims: BTreeMap<_, _> = d.dimensions.iter().cloned().collect();
        for (j, (k, v)) in dims.iter().enumerate() {
            p.push((format!("{}.Dimensions.member.{}.Name", m, j + 1), k.clone()));
            p.push((format!("{}.Dimensions.member.{}.Value", m, j + 1), v.clone()));
        }
    }
    p
}

impl CloudService for CwService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let data = match ex.body() {
            Value::Array(a) if !a.is_empty() && a.iter().all(|v| matches!(v, Value::Object(_))) => a.iter().map(|v| self.datum_from_value(v)).collect::<Result<Vec<_>>>()?,
            _ => vec![self.datum_from_headers(ex)?],
        };
        let ns = header_str(ex, headers::METRIC_NAMESPACE).unwrap_or_else(|| self.namespace.clone());
        self.aws.query_call(self.aws.endpoint("monitoring")?, "monitoring", &self.aws.region.clone(), "2010-08-01", "PutMetricData", put_metric_params(&ns, &data), kinds::CLOUDWATCH)?;
        Ok(())
    }
}
