//! The `azure-schema-registry` component (rust-camel, producer) over the Azure Event Hubs Schema
//! Registry REST API (`api-version=2022-10`) with Microsoft Entra ID client credentials
//! (scope `https://eventhubs.azure.net/.default`).
//!
//! `azure-schema-registry://namespace/group?tenantId=...&clientId=...&clientSecret=...`
//! (`namespace` = `myns` → `https://myns.servicebus.windows.net`, or a fully qualified name)
//!
//! Operations (`operation` / `CamelAzureSchemaRegistryOperation`; schema name `schemaName` /
//! `CamelAzureSchemaRegistrySchemaName`, `format` / `CamelAzureSchemaRegistryFormat` = `Avro`
//! (default), `Json` or `Custom`):
//! * `registerSchema`: body = schema definition; body = schema id, headers
//!   `CamelAzureSchemaRegistrySchemaId`, `...SchemaVersion`.
//! * `getSchemaProperties`: finds the id of the body's definition (same headers).
//! * `getSchema` (default, `CamelAzureSchemaRegistrySchemaId` or `schemaId`): body = schema
//!   definition, headers name / group / version / format.
//! * `getSchemaByVersion` (name + `CamelAzureSchemaRegistrySchemaVersion`).
//! * `listVersions` (name): body = list of versions. `listGroups`: body = group names.

use std::sync::Arc;

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::Value;

use super::rest::{aad_credential, AzureAuth, AzureRest};
use crate::endpoint::{operation, unsupported, CloudService};
use crate::http::{Request, Response};
use crate::util::{header_str, kinds, parse_json, uri_encode, Opts};

/// Header names.
pub mod headers {
    pub const OPERATION: &str = "CamelAzureSchemaRegistryOperation";
    pub const SCHEMA_NAME: &str = "CamelAzureSchemaRegistrySchemaName";
    pub const SCHEMA_ID: &str = "CamelAzureSchemaRegistrySchemaId";
    pub const SCHEMA_VERSION: &str = "CamelAzureSchemaRegistrySchemaVersion";
    pub const SCHEMA_GROUP: &str = "CamelAzureSchemaRegistrySchemaGroup";
    pub const FORMAT: &str = "CamelAzureSchemaRegistryFormat";
}

/// Schema Registry API version.
pub const API_VERSION: &str = "2022-10";
/// Entra ID scope.
pub const SCOPE: &str = "https://eventhubs.azure.net/.default";

/// The Schema Registry endpoint configuration and client.
pub struct SchemaRegistryService {
    pub rest: AzureRest,
    pub group: String,
    pub operation: Option<String>,
    pub schema_name: Option<String>,
    pub schema_id: Option<String>,
    pub format: String,
}

/// Content type of a schema format.
pub fn content_type(format: &str) -> Result<&'static str> {
    Ok(match format.to_ascii_lowercase().as_str() {
        "avro" => "application/json; serialization=Avro",
        "json" => "application/json; serialization=Json",
        "custom" => "text/plain; charset=utf-8",
        other => return Err(CamelError::illegal_argument(format!("Invalid schema format: {} (Avro, Json or Custom)", other))),
    })
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let (ns, group) = remaining.split_once('/').map(|(n, g)| (n.to_string(), g.to_string())).unwrap_or((remaining.to_string(), String::new()));
    if ns.is_empty() {
        return Err(CamelError::illegal_argument("Expected azure-schema-registry://namespace/group"));
    }
    let host = if ns.contains('.') { ns } else { format!("{}.servicebus.windows.net", ns) };
    let identity = o.string("credentialType", "CLIENT_SECRET").eq_ignore_ascii_case("AZURE_IDENTITY");
    let cred = aad_credential(o, identity)?.ok_or_else(|| CamelError::illegal_argument("azure-schema-registry requires tenantId / clientId / clientSecret (or credentialType=AZURE_IDENTITY)"))?;
    let rest = AzureRest::from_opts(o, &format!("https://{}", host), AzureAuth::Bearer(cred, SCOPE.into()), kinds::AZURE_HTTP)?;
    let format = o.string("format", "Avro");
    content_type(&format)?;
    Ok(Arc::new(SchemaRegistryService { rest, group, operation: o.get("operation"), schema_name: o.get("schemaName"), schema_id: o.get("schemaId"), format }))
}

fn q() -> String {
    format!("api-version={}", API_VERSION)
}

impl SchemaRegistryService {
    fn group(&self, ex: &Exchange) -> Result<String> {
        header_str(ex, headers::SCHEMA_GROUP).or_else(|| Some(self.group.clone()).filter(|g| !g.is_empty())).ok_or_else(|| CamelError::illegal_argument("A schema group is required: azure-schema-registry://namespace/group"))
    }

    fn name(&self, ex: &Exchange) -> Result<String> {
        header_str(ex, headers::SCHEMA_NAME).or_else(|| self.schema_name.clone()).ok_or_else(|| CamelError::illegal_argument("A schema name is required (schemaName option or CamelAzureSchemaRegistrySchemaName header)"))
    }

    fn schema_path(&self, ex: &Exchange) -> Result<String> {
        Ok(format!("/$schemaGroups/{}/schemas/{}", uri_encode(&self.group(ex)?, false), uri_encode(&self.name(ex)?, false)))
    }

    fn set_props(ex: &mut Exchange, r: &Response) {
        for (h, name) in [("Schema-Id", headers::SCHEMA_ID), ("Schema-Group-Name", headers::SCHEMA_GROUP), ("Schema-Name", headers::SCHEMA_NAME)] {
            if let Some(v) = r.header(h) {
                ex.set_header(name, v.to_string());
            }
        }
        if let Some(v) = r.header("Schema-Version").and_then(|v| v.parse::<i64>().ok()) {
            ex.set_header(headers::SCHEMA_VERSION, v);
        }
        if let Some(ct) = r.header("Content-Type") {
            let f = if ct.contains("serialization=Avro") {
                "Avro"
            } else if ct.contains("serialization=Json") {
                "Json"
            } else {
                "Custom"
            };
            ex.set_header(headers::FORMAT, f);
        }
    }
}

impl CloudService for SchemaRegistryService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "getSchema");
        let format = header_str(ex, headers::FORMAT).unwrap_or_else(|| self.format.clone());
        match op.as_str() {
            "registerSchema" | "getSchemaProperties" => {
                let (method, path) = if op == "registerSchema" { ("PUT", self.schema_path(ex)?) } else { ("POST", format!("{}:get-id", self.schema_path(ex)?)) };
                let body = match ex.body() {
                    Value::Object(_) | Value::Array(_) => ex.body().to_json(),
                    other => other.to_string(),
                };
                let req = Request::new(method, self.rest.url(&path, &q())?).header("Content-Type", content_type(&format)?).body(body.into_bytes());
                let r = self.rest.send(req)?;
                Self::set_props(ex, &r);
                ex.message.remove_header(headers::FORMAT);
                ex.set_body(r.header("Schema-Id").unwrap_or("").to_string());
            }
            "getSchema" => {
                let id = header_str(ex, headers::SCHEMA_ID).or_else(|| self.schema_id.clone()).ok_or_else(|| CamelError::illegal_argument("getSchema requires CamelAzureSchemaRegistrySchemaId (or schemaId)"))?;
                let r = self.rest.send(Request::new("GET", self.rest.url(&format!("/$schemaGroups/$schemas/{}", uri_encode(&id, false)), &q())?))?;
                Self::set_props(ex, &r);
                ex.set_body(r.text());
            }
            "getSchemaByVersion" => {
                let v = header_str(ex, headers::SCHEMA_VERSION).ok_or_else(|| CamelError::illegal_argument("getSchemaByVersion requires CamelAzureSchemaRegistrySchemaVersion"))?;
                let r = self.rest.send(Request::new("GET", self.rest.url(&format!("{}/versions/{}", self.schema_path(ex)?, uri_encode(&v, false)), &q())?))?;
                Self::set_props(ex, &r);
                ex.set_body(r.text());
            }
            "listVersions" => {
                let r = self.rest.send(Request::new("GET", self.rest.url(&format!("{}/versions", self.schema_path(ex)?), &q())?))?;
                ex.set_body(parse_json(&r.body)?.get("schemaVersions").cloned().unwrap_or(Value::Array(vec![])));
            }
            "listGroups" => {
                let r = self.rest.send(Request::new("GET", self.rest.url("/$schemaGroups", &q())?))?;
                ex.set_body(parse_json(&r.body)?.get("schemaGroups").cloned().unwrap_or(Value::Array(vec![])));
            }
            other => return Err(unsupported("Schema Registry", other)),
        }
        Ok(())
    }
}
