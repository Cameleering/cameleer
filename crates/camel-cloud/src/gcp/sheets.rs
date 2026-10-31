//! The `google-sheets` component (Camel `camel-google-sheets`, producer subset) over the Sheets
//! v4 REST API, authenticated with a service account (scope
//! `https://www.googleapis.com/auth/spreadsheets`).
//!
//! `google-sheets://apiName/methodName?spreadsheetId=...&range=Sheet1!A1:B2&serviceAccountKey=...`
//!
//! Options can also be given as `CamelGoogleSheets.<option>` headers (Camel's convention).
//! * `data/get`: body = the values (list of rows).
//! * `data/update`, `data/append`: values from the `values` header / body (list of rows, or a
//!   `ValueRange` map with `values`), `valueInputOption` (default `USER_ENTERED`); body = the
//!   update result (`updatedRange`, `updatedRows`, ...).
//! * `data/clear`, `data/batchGet` (`ranges` list; body = list of value lists).
//! * `spreadsheets/get` (body = spreadsheet resource), `spreadsheets/create` (`title`; body =
//!   the new spreadsheet, `CamelGoogleSheets.spreadsheetId` set).

use std::sync::Arc;

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::Value;

use super::GcpConfig;
use crate::endpoint::CloudService;
use crate::http::{Request, Url};
use crate::util::{kinds, obj, parse_json, str_list, uri_encode, Opts};

/// OAuth scope of the Sheets API.
pub const SCOPE: &str = "https://www.googleapis.com/auth/spreadsheets";
/// Header prefix for options.
pub const HEADER_PREFIX: &str = "CamelGoogleSheets.";

/// The Sheets endpoint configuration and client.
pub struct SheetsService {
    pub gcp: GcpConfig,
    pub api: String,
    pub method: String,
    pub spreadsheet_id: Option<String>,
    pub range: Option<String>,
    pub ranges: Vec<String>,
    pub value_input_option: String,
    pub major_dimension: Option<String>,
    pub title: Option<String>,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let (api, method) = remaining.split_once('/').ok_or_else(|| CamelError::illegal_argument("Expected google-sheets://apiName/methodName (data/get, data/update, data/append, data/clear, data/batchGet, spreadsheets/get, spreadsheets/create)"))?;
    let ok = matches!((api, method), ("data", "get" | "update" | "append" | "clear" | "batchGet") | ("spreadsheets", "get" | "create"));
    if !ok {
        return Err(CamelError::illegal_argument(format!("Unsupported google-sheets api/method: {}/{}", api, method)));
    }
    let gcp = GcpConfig::from_opts(o)?;
    let s = SheetsService {
        api: api.to_string(),
        method: method.to_string(),
        spreadsheet_id: o.get("spreadsheetId"),
        range: o.get("range"),
        ranges: o.get("ranges").map(|r| str_list(&Value::from(r))).unwrap_or_default(),
        value_input_option: o.string("valueInputOption", "USER_ENTERED"),
        major_dimension: o.get("majorDimension"),
        title: o.get("title"),
        gcp,
    };
    o.ignore(&["clientId", "clientSecret", "accessToken", "refreshToken", "applicationName", "scopes", "delegate", "splitResult", "inBody", "includeGridData"]);
    Ok(Arc::new(s))
}

impl SheetsService {
    fn opt(&self, ex: &Exchange, name: &str, configured: &Option<String>) -> Option<String> {
        crate::util::header_str(ex, &format!("{}{}", HEADER_PREFIX, name)).or_else(|| configured.clone())
    }

    fn need(&self, ex: &Exchange, name: &str, configured: &Option<String>) -> Result<String> {
        self.opt(ex, name, configured).ok_or_else(|| CamelError::illegal_argument(format!("google-sheets {}/{} requires the {} option (or the {}{} header)", self.api, self.method, name, HEADER_PREFIX, name)))
    }

    fn call(&self, method: &str, url: String, body: Option<Value>) -> Result<Value> {
        let mut req = Request::new(method, Url::parse(&url)?);
        if let Some(b) = body {
            req = req.header("Content-Type", "application/json").body(b.to_json().into_bytes());
        }
        let resp = self.gcp.send_scoped(req, kinds::GOOGLE_JSON, SCOPE)?;
        parse_json(&resp.body)
    }

    fn values(ex: &Exchange) -> Result<Value> {
        let v = ex.header(&format!("{}values", HEADER_PREFIX)).cloned().unwrap_or_else(|| ex.body().clone());
        let v = match v {
            Value::String(_) | Value::Bytes(_) => parse_json(&v.to_bytes())?,
            other => other,
        };
        let rows = match v.get("values") {
            Some(r) => r.clone(),
            None => v,
        };
        match &rows {
            Value::Array(a) if a.iter().all(|r| matches!(r, Value::Array(_))) => Ok(rows),
            Value::Array(a) => Ok(Value::Array(vec![Value::Array(a.clone())])),
            _ => Err(CamelError::illegal_argument("Sheet values must be a list of rows (lists)")),
        }
    }
}

impl CloudService for SheetsService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let base = format!("{}/v4/spreadsheets", self.gcp.base("https://sheets.googleapis.com"));
        if (self.api.as_str(), self.method.as_str()) == ("spreadsheets", "create") {
            let title = self.opt(ex, "title", &self.title).unwrap_or_else(|| "Untitled spreadsheet".into());
            let r = self.call("POST", base, Some(obj([("properties", obj([("title", Value::from(title))]))])))?;
            ex.set_header(&format!("{}spreadsheetId", HEADER_PREFIX), r.get("spreadsheetId").cloned().unwrap_or_default());
            ex.set_body(r);
            return Ok(());
        }
        let id = uri_encode(&self.need(ex, "spreadsheetId", &self.spreadsheet_id)?, false);
        let sheet = format!("{}/{}", base, id);
        let r = match (self.api.as_str(), self.method.as_str()) {
            ("spreadsheets", "get") => self.call("GET", sheet, None)?,
            ("data", "batchGet") => {
                let ranges = match ex.header(&format!("{}ranges", HEADER_PREFIX)) {
                    Some(v) => str_list(v),
                    None => self.ranges.clone(),
                };
                let q: Vec<String> = ranges.iter().map(|r| format!("ranges={}", uri_encode(r, false))).collect();
                let r = self.call("GET", format!("{}/values:batchGet?{}", sheet, q.join("&")), None)?;
                let out: Vec<Value> = r.get("valueRanges").and_then(|v| v.as_array()).cloned().unwrap_or_default().iter().map(|vr| vr.get("values").cloned().unwrap_or(Value::Array(vec![]))).collect();
                ex.set_body(Value::Array(out));
                return Ok(());
            }
            (_, m) => {
                let range = uri_encode(&self.need(ex, "range", &self.range)?, false);
                let vio = self.opt(ex, "valueInputOption", &Some(self.value_input_option.clone())).unwrap_or_default();
                match m {
                    "get" => {
                        let mut url = format!("{}/values/{}", sheet, range);
                        if let Some(d) = self.opt(ex, "majorDimension", &self.major_dimension) {
                            url.push_str(&format!("?majorDimension={}", d));
                        }
                        let r = self.call("GET", url, None)?;
                        ex.set_body(r.get("values").cloned().unwrap_or(Value::Array(vec![])));
                        return Ok(());
                    }
                    "update" => self.call("PUT", format!("{}/values/{}?valueInputOption={}", sheet, range, vio), Some(obj([("values", Self::values(ex)?)])))?,
                    "append" => {
                        let r = self.call("POST", format!("{}/values/{}:append?valueInputOption={}", sheet, range, vio), Some(obj([("values", Self::values(ex)?)])))?;
                        r.get("updates").cloned().unwrap_or(r)
                    }
                    _ => self.call("POST", format!("{}/values/{}:clear", sheet, range), Some(obj([])))?,
                }
            }
        };
        ex.set_body(r);
        Ok(())
    }
}
