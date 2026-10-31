//! The `aws2-lambda` component (Camel `camel-aws2-lambda`, producer) over the Lambda REST API.
//!
//! `aws2-lambda://functionNameOrArn?operation=...` with Camel's `Lambda2Operations`:
//!
//! * `invokeFunction` (default) posts the body as payload and sets the response payload as the
//!   body; a function error is reported in the `CamelAwsLambdaFunctionError` header (the payload
//!   then holds the error document). Invocation type `RequestResponse` by default, or
//!   `CamelAwsLambdaInvocationType` (`Event`, `DryRun`).
//! * `listFunctions` (array of function configurations), `getFunction` (function document),
//!   `deleteFunction`.
//! * `createFunction`: code from `CamelAwsLambdaS3Bucket` / `CamelAwsLambdaS3Key` /
//!   `CamelAwsLambdaS3ObjectVersion` or the body (zip bytes); `CamelAwsLambdaRole` and
//!   `CamelAwsLambdaRuntime` required, `CamelAwsLambdaHandler`, `CamelAwsLambdaDescription`,
//!   `CamelAwsLambdaTargetArn` (dead letter target), `CamelAwsLambdaMemorySize`,
//!   `CamelAwsLambdaKMSKeyArn`, `CamelAwsLambdaPublish`, `CamelAwsLambdaTimeout`,
//!   `CamelAwsLambdaTracingConfig`, `CamelAwsLambdaEnvironmentVariables` (map),
//!   `CamelAwsLambdaTags` (map), `CamelAwsLambdaSecurityGroupIds` / `CamelAwsLambdaSubnetIds`.
//! * `updateFunction` (UpdateFunctionCode, code as for createFunction, `CamelAwsLambdaPublish`).
//! * `createEventSourceMapping` (`CamelAwsLambdaEventSourceArn`, `CamelAwsLambdaEventSourceBatchSize`,
//!   rust-camel `CamelAwsLambdaEventSourceStartingPosition`), `deleteEventSourceMapping`
//!   (`CamelAwsLambdaEventSourceUuid`), `listEventSourceMapping` (array).
//! * `listTags`, `tagResource` (`CamelAwsLambdaResourceTags`), `untagResource`
//!   (`CamelAwsLambdaResourceTagKeys`) on `CamelAwsLambdaResourceArn`.
//! * `publishVersion` (`CamelAwsLambdaVersionDescription`), `listVersions` (array).
//! * `createAlias` (`CamelAwsLambdaAliasFunctionName`, `CamelAwsLambdaFunctionVersion`,
//!   `CamelAwsLambdaAliasFunctionDescription`), `deleteAlias`, `getAlias`, `listAliases` (array).
//!
//! Every operation but invoke sets the response document (a map) as the body.

use std::sync::Arc;

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::{ObjectMap, Value};

use super::{header_json, need, put, AwsConfig};
use crate::endpoint::{operation, unsupported, CloudService};
use crate::http::Request;
use crate::util::{base64_encode, header_i64, header_list, header_str, kinds, obj, parse_json, uri_encode, Opts};

/// Header names (Camel `Lambda2Constants`).
pub mod headers {
    pub const OPERATION: &str = "CamelAwsLambdaOperation";
    pub const INVOCATION_TYPE: &str = "CamelAwsLambdaInvocationType";
    pub const FUNCTION_ERROR: &str = "CamelAwsLambdaFunctionError";
    pub const STATUS_CODE: &str = "CamelAwsLambdaStatusCode";
    pub const S3_BUCKET: &str = "CamelAwsLambdaS3Bucket";
    pub const S3_KEY: &str = "CamelAwsLambdaS3Key";
    pub const S3_OBJECT_VERSION: &str = "CamelAwsLambdaS3ObjectVersion";
    pub const DESCRIPTION: &str = "CamelAwsLambdaDescription";
    pub const ROLE: &str = "CamelAwsLambdaRole";
    pub const RUNTIME: &str = "CamelAwsLambdaRuntime";
    pub const HANDLER: &str = "CamelAwsLambdaHandler";
    pub const TARGET_ARN: &str = "CamelAwsLambdaTargetArn";
    pub const MEMORY_SIZE: &str = "CamelAwsLambdaMemorySize";
    pub const KMS_KEY_ARN: &str = "CamelAwsLambdaKMSKeyArn";
    pub const ENVIRONMENT_VARIABLES: &str = "CamelAwsLambdaEnvironmentVariables";
    pub const PUBLISH: &str = "CamelAwsLambdaPublish";
    pub const TIMEOUT: &str = "CamelAwsLambdaTimeout";
    pub const TAGS: &str = "CamelAwsLambdaTags";
    pub const TRACING_CONFIG: &str = "CamelAwsLambdaTracingConfig";
    pub const SECURITY_GROUP_IDS: &str = "CamelAwsLambdaSecurityGroupIds";
    pub const SUBNET_IDS: &str = "CamelAwsLambdaSubnetIds";
    pub const EVENT_SOURCE_ARN: &str = "CamelAwsLambdaEventSourceArn";
    pub const EVENT_SOURCE_BATCH_SIZE: &str = "CamelAwsLambdaEventSourceBatchSize";
    pub const EVENT_SOURCE_UUID: &str = "CamelAwsLambdaEventSourceUuid";
    /// rust-camel extension (`TRIM_HORIZON`, `LATEST`, ...).
    pub const EVENT_SOURCE_STARTING_POSITION: &str = "CamelAwsLambdaEventSourceStartingPosition";
    pub const RESOURCE_ARN: &str = "CamelAwsLambdaResourceArn";
    pub const RESOURCE_TAGS: &str = "CamelAwsLambdaResourceTags";
    pub const RESOURCE_TAG_KEYS: &str = "CamelAwsLambdaResourceTagKeys";
    pub const VERSION_DESCRIPTION: &str = "CamelAwsLambdaVersionDescription";
    pub const FUNCTION_VERSION: &str = "CamelAwsLambdaFunctionVersion";
    pub const ALIAS_FUNCTION_NAME: &str = "CamelAwsLambdaAliasFunctionName";
    pub const ALIAS_FUNCTION_DESCRIPTION: &str = "CamelAwsLambdaAliasFunctionDescription";
}

/// The Lambda endpoint configuration and client.
pub struct LambdaService {
    pub aws: AwsConfig,
    pub function: String,
    pub operation: Option<String>,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    if remaining.is_empty() {
        return Err(CamelError::illegal_argument("Function name must be specified: aws2-lambda://functionName"));
    }
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["pojoRequest", "awsLambdaClient"]);
    Ok(Arc::new(LambdaService { function: remaining.to_string(), operation: o.get("operation"), aws }))
}

impl LambdaService {
    fn url(&self, suffix: &str) -> Result<crate::http::Url> {
        let mut u = self.aws.endpoint("lambda")?;
        u.path = format!("{}/2015-03-31/functions{}", u.path.trim_end_matches('/'), suffix);
        Ok(u)
    }

    fn fname(&self) -> String {
        uri_encode(&self.function, false)
    }

    /// Invoke the function synchronously; returns (status, function error, payload).
    pub fn invoke(&self, payload: Vec<u8>, invocation_type: &str) -> Result<(u16, Option<String>, Vec<u8>)> {
        let req = Request::new("POST", self.url(&format!("/{}/invocations", self.fname()))?)
            .header("X-Amz-Invocation-Type", invocation_type)
            .header("Content-Type", "application/json")
            .body(payload);
        let resp = self.aws.send(req, "lambda", kinds::LAMBDA)?;
        Ok((resp.status, resp.header("X-Amz-Function-Error").map(|s| s.to_string()), resp.body))
    }

    /// A Lambda REST call (`path` below the API root, e.g. `/2015-03-31/functions/x/aliases`).
    pub fn rest(&self, method: &str, path: &str, query: &[(String, String)], body: Option<&Value>) -> Result<Value> {
        let resp = self.aws.rest_call(method, "lambda", path, query, body.map(|b| ("application/json", b.to_json().into_bytes())), &[], "lambda", kinds::LAMBDA)?;
        if resp.body.is_empty() {
            Ok(Value::Object(ObjectMap::new()))
        } else {
            parse_json(&resp.body)
        }
    }

    fn fpath(&self, suffix: &str) -> String {
        format!("/2015-03-31/functions/{}{}", self.fname(), suffix)
    }

    /// Code: S3 location headers, or the body as zip bytes.
    fn code(ex: &Exchange) -> Result<Value> {
        let mut code = ObjectMap::new();
        if let Some(b) = header_str(ex, headers::S3_BUCKET) {
            code.insert("S3Bucket".into(), Value::from(b));
            code.insert("S3Key".into(), Value::from(need(ex, headers::S3_KEY)?));
            put(&mut code, "S3ObjectVersion", header_str(ex, headers::S3_OBJECT_VERSION));
        } else {
            let zip = ex.body().to_bytes();
            if zip.is_empty() {
                return Err(CamelError::illegal_argument("Function code is required: a zip file body or the CamelAwsLambdaS3Bucket / CamelAwsLambdaS3Key headers"));
            }
            code.insert("ZipFile".into(), Value::from(base64_encode(&zip)));
        }
        Ok(Value::Object(code))
    }

    fn string_map(ex: &Exchange, h: &str) -> Option<Value> {
        header_json(ex, h).map(|v| match v {
            Value::Object(m) => Value::Object(m.into_iter().map(|(k, v)| (k, Value::from(v.to_string()))).collect()),
            other => other,
        })
    }

    fn create_function(&self, ex: &Exchange) -> Result<Value> {
        let mut m = ObjectMap::new();
        m.insert("FunctionName".into(), Value::from(self.function.clone()));
        m.insert("Code".into(), Self::code(ex)?);
        m.insert("Role".into(), Value::from(need(ex, headers::ROLE)?));
        m.insert("Runtime".into(), Value::from(need(ex, headers::RUNTIME)?));
        put(&mut m, "Handler", header_str(ex, headers::HANDLER));
        put(&mut m, "Description", header_str(ex, headers::DESCRIPTION));
        put(&mut m, "MemorySize", header_i64(ex, headers::MEMORY_SIZE));
        put(&mut m, "Timeout", header_i64(ex, headers::TIMEOUT));
        put(&mut m, "KMSKeyArn", header_str(ex, headers::KMS_KEY_ARN));
        put(&mut m, "Publish", ex.header(headers::PUBLISH).map(|v| Value::Bool(v.is_truthy())));
        if let Some(t) = header_str(ex, headers::TARGET_ARN) {
            m.insert("DeadLetterConfig".into(), obj([("TargetArn", Value::from(t))]));
        }
        if let Some(t) = header_str(ex, headers::TRACING_CONFIG) {
            m.insert("TracingConfig".into(), obj([("Mode", Value::from(t))]));
        }
        if let Some(env) = Self::string_map(ex, headers::ENVIRONMENT_VARIABLES) {
            m.insert("Environment".into(), obj([("Variables", env)]));
        }
        put(&mut m, "Tags", Self::string_map(ex, headers::TAGS));
        let (sg, sn) = (header_list(ex, headers::SECURITY_GROUP_IDS), header_list(ex, headers::SUBNET_IDS));
        if !sg.is_empty() || !sn.is_empty() {
            m.insert("VpcConfig".into(), obj([("SecurityGroupIds", Value::from(sg)), ("SubnetIds", Value::from(sn))]));
        }
        self.rest("POST", "/2015-03-31/functions", &[], Some(&Value::Object(m)))
    }

    fn produce_op(&self, ex: &mut Exchange, op: &str) -> Result<()> {
        let body = match op {
            "invokeFunction" => {
                let it = header_str(ex, headers::INVOCATION_TYPE).unwrap_or_else(|| "RequestResponse".into());
                let (status, err, payload) = self.invoke(ex.body().to_bytes(), &it)?;
                ex.set_header(headers::STATUS_CODE, status as i64);
                if let Some(e) = err {
                    ex.set_header(headers::FUNCTION_ERROR, e);
                }
                ex.set_body(String::from_utf8_lossy(&payload).into_owned());
                return Ok(());
            }
            "listFunctions" => {
                let resp = self.aws.send(Request::new("GET", self.url("/")?), "lambda", kinds::LAMBDA)?;
                parse_json(&resp.body)?.get("Functions").cloned().unwrap_or(Value::Array(vec![]))
            }
            "getFunction" => self.rest("GET", &self.fpath(""), &[], None)?,
            "deleteFunction" => self.rest("DELETE", &self.fpath(""), &[], None)?,
            "createFunction" => self.create_function(ex)?,
            "updateFunction" => {
                let mut m = match Self::code(ex)? {
                    Value::Object(m) => m,
                    _ => ObjectMap::new(),
                };
                put(&mut m, "Publish", ex.header(headers::PUBLISH).map(|v| Value::Bool(v.is_truthy())));
                self.rest("PUT", &self.fpath("/code"), &[], Some(&Value::Object(m)))?
            }
            "createEventSourceMapping" => {
                let mut m = ObjectMap::new();
                m.insert("FunctionName".into(), Value::from(self.function.clone()));
                m.insert("EventSourceArn".into(), Value::from(need(ex, headers::EVENT_SOURCE_ARN)?));
                put(&mut m, "BatchSize", header_i64(ex, headers::EVENT_SOURCE_BATCH_SIZE));
                put(&mut m, "StartingPosition", header_str(ex, headers::EVENT_SOURCE_STARTING_POSITION));
                self.rest("POST", "/2015-03-31/event-source-mappings/", &[], Some(&Value::Object(m)))?
            }
            "deleteEventSourceMapping" => {
                let id = need(ex, headers::EVENT_SOURCE_UUID)?;
                self.rest("DELETE", &format!("/2015-03-31/event-source-mappings/{}", uri_encode(&id, false)), &[], None)?
            }
            "listEventSourceMapping" => {
                let r = self.rest("GET", "/2015-03-31/event-source-mappings/", &[("FunctionName".into(), self.function.clone())], None)?;
                r.get("EventSourceMappings").cloned().unwrap_or(Value::Array(vec![]))
            }
            "listTags" => {
                let arn = need(ex, headers::RESOURCE_ARN)?;
                self.rest("GET", &format!("/2017-03-31/tags/{}", uri_encode(&arn, false)), &[], None)?.get("Tags").cloned().unwrap_or_default()
            }
            "tagResource" => {
                let arn = need(ex, headers::RESOURCE_ARN)?;
                let tags = Self::string_map(ex, headers::RESOURCE_TAGS).ok_or_else(|| CamelError::illegal_argument("tagResource requires the CamelAwsLambdaResourceTags header"))?;
                self.rest("POST", &format!("/2017-03-31/tags/{}", uri_encode(&arn, false)), &[], Some(&obj([("Tags", tags)])))?
            }
            "untagResource" => {
                let arn = need(ex, headers::RESOURCE_ARN)?;
                let keys: Vec<(String, String)> = header_list(ex, headers::RESOURCE_TAG_KEYS).into_iter().map(|k| ("tagKeys".to_string(), k)).collect();
                if keys.is_empty() {
                    return Err(CamelError::illegal_argument("untagResource requires the CamelAwsLambdaResourceTagKeys header"));
                }
                self.rest("DELETE", &format!("/2017-03-31/tags/{}", uri_encode(&arn, false)), &keys, None)?
            }
            "publishVersion" => {
                let mut m = ObjectMap::new();
                put(&mut m, "Description", header_str(ex, headers::VERSION_DESCRIPTION));
                self.rest("POST", &self.fpath("/versions"), &[], Some(&Value::Object(m)))?
            }
            "listVersions" => self.rest("GET", &self.fpath("/versions"), &[], None)?.get("Versions").cloned().unwrap_or(Value::Array(vec![])),
            "createAlias" => {
                let mut m = ObjectMap::new();
                m.insert("Name".into(), Value::from(need(ex, headers::ALIAS_FUNCTION_NAME)?));
                m.insert("FunctionVersion".into(), Value::from(need(ex, headers::FUNCTION_VERSION)?));
                put(&mut m, "Description", header_str(ex, headers::ALIAS_FUNCTION_DESCRIPTION));
                self.rest("POST", &self.fpath("/aliases"), &[], Some(&Value::Object(m)))?
            }
            "deleteAlias" | "getAlias" => {
                let alias = need(ex, headers::ALIAS_FUNCTION_NAME)?;
                let method = if op == "deleteAlias" { "DELETE" } else { "GET" };
                self.rest(method, &self.fpath(&format!("/aliases/{}", uri_encode(&alias, false))), &[], None)?
            }
            "listAliases" => self.rest("GET", &self.fpath("/aliases"), &[], None)?.get("Aliases").cloned().unwrap_or(Value::Array(vec![])),
            other => return Err(unsupported("aws2-lambda", other)),
        };
        ex.set_body(body);
        Ok(())
    }
}

impl CloudService for LambdaService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "invokeFunction");
        self.produce_op(ex, &op)
    }
}
