//! The `aws2-ddb` component (Camel `camel-aws2-ddb`, producer) over the DynamoDB JSON 1.0 API.
//!
//! `aws2-ddb://tableName?operation=PutItem|GetItem|UpdateItem|DeleteItem|Query|Scan|BatchGetItems|DescribeTable|UpdateTable|DeleteTable`
//!
//! Items, keys and conditions are plain [`Value`] maps; they are marshalled to/from
//! DynamoDB attribute values ([`to_attribute_value`]: strings `S`, numbers `N`, booleans
//! `BOOL`, null `NULL`, bytes `B`, arrays `L`, maps `M`; `SS`/`NS`/`BS` are read as arrays).
//!
//! Headers (Camel `Ddb2Constants`): input `CamelAwsDdbItem` (PutItem), `CamelAwsDdbKey`
//! (Get/Update/DeleteItem), `CamelAwsDdbUpdateValues` (UpdateItem: attribute → new value, null
//! deletes), `CamelAwsDdbKeyConditions` (Query: attribute → value for `EQ` or
//! `{"ComparisonOperator": "...", "AttributeValueList": [...]}`), `CamelAwsDdbScanFilter`
//! (Scan, same shape), `CamelAwsDdbReturnValues`, `CamelAwsDdbConsistentRead`,
//! `CamelAwsDdbAttributeNames`, `CamelAwsDdbIndexName`, `CamelAwsDdbLimit`,
//! `CamelAwsDdbStartKey`, `CamelAwsDdbScanIndexForward`, `CamelAwsDdbOperation`,
//! `CamelAwsDdbTableName`; output `CamelAwsDdbAttributes`, `CamelAwsDdbItems`,
//! `CamelAwsDdbCount`, `CamelAwsDdbScannedCount`, `CamelAwsDdbLastEvaluatedKey`,
//! `CamelAwsDdbTableStatus`, `CamelAwsDdbItemCount`, `CamelAwsDdbKeySchema`.
//!
//! Expressions: `CamelAwsDdbUpdateExpression` (UpdateItem: `SET a = :v, n = n + :one REMOVE b ...`,
//! instead of `CamelAwsDdbUpdateValues`), `CamelAwsDdbKeyConditionExpression` (Query, instead of
//! `CamelAwsDdbKeyConditions`), `CamelAwsDdbFilterExpression` (Query / Scan),
//! `CamelAwsDdbConditionExpression` (Put/Update/DeleteItem), `CamelAwsDdbProjectionExpression`,
//! with `CamelAwsDdbExpressionAttributeNames` (`#n` → name) and
//! `CamelAwsDdbExpressionAttributeValues` (`:v` → plain value, marshalled); Camel's
//! `CamelAwsDdbFilterExpressionAttributeNames` / `...Values` are accepted too.
//!
//! `BatchGetItems`: `CamelAwsDdbBatchItems` = map of table name → `{"Keys": [key maps],
//! "ConsistentRead": .., "ProjectionExpression": ..}`; output `CamelAwsDdbBatchResponse` (table →
//! items) and `CamelAwsDdbUnprocessedKeys`. `UpdateTable`: `CamelAwsDdbReadCapacity` /
//! `CamelAwsDdbWriteCapacity` (provisioned throughput) and the Cameleer
//! `CamelAwsDdbStreamViewType` (`NEW_IMAGE`, `OLD_IMAGE`, `NEW_AND_OLD_IMAGES`, `KEYS_ONLY`;
//! `DISABLED` turns the stream off); output `CamelAwsDdbTableStatus`,
//! `CamelAwsDdbProvisionedThroughput`. DescribeTable also sets `CamelAwsDdbStreamArn` (Cameleer).
//!
//! On start the table is described and, when missing and `keyAttributeName` is set, created
//! (`keyAttributeName`, `keyAttributeType` default `S`, `readCapacity`, `writeCapacity`).

use std::sync::Arc;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::value::{ObjectMap, Value};

use super::{error_code, AwsConfig};
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{base64_decode, base64_encode, header_str, kinds, obj, Opts};

/// Header names (Camel `Ddb2Constants`).
pub mod headers {
    pub const ITEM: &str = "CamelAwsDdbItem";
    pub const KEY: &str = "CamelAwsDdbKey";
    pub const ATTRIBUTES: &str = "CamelAwsDdbAttributes";
    pub const ITEMS: &str = "CamelAwsDdbItems";
    pub const COUNT: &str = "CamelAwsDdbCount";
    pub const SCANNED_COUNT: &str = "CamelAwsDdbScannedCount";
    pub const LAST_EVALUATED_KEY: &str = "CamelAwsDdbLastEvaluatedKey";
    pub const START_KEY: &str = "CamelAwsDdbStartKey";
    pub const KEY_CONDITIONS: &str = "CamelAwsDdbKeyConditions";
    pub const SCAN_FILTER: &str = "CamelAwsDdbScanFilter";
    pub const UPDATE_VALUES: &str = "CamelAwsDdbUpdateValues";
    pub const RETURN_VALUES: &str = "CamelAwsDdbReturnValues";
    pub const CONSISTENT_READ: &str = "CamelAwsDdbConsistentRead";
    pub const ATTRIBUTE_NAMES: &str = "CamelAwsDdbAttributeNames";
    pub const INDEX_NAME: &str = "CamelAwsDdbIndexName";
    pub const LIMIT: &str = "CamelAwsDdbLimit";
    pub const SCAN_INDEX_FORWARD: &str = "CamelAwsDdbScanIndexForward";
    pub const OPERATION: &str = "CamelAwsDdbOperation";
    pub const TABLE_NAME: &str = "CamelAwsDdbTableName";
    pub const TABLE_STATUS: &str = "CamelAwsDdbTableStatus";
    pub const ITEM_COUNT: &str = "CamelAwsDdbItemCount";
    pub const KEY_SCHEMA: &str = "CamelAwsDdbKeySchema";
    pub const UPDATE_EXPRESSION: &str = "CamelAwsDdbUpdateExpression";
    pub const KEY_CONDITION_EXPRESSION: &str = "CamelAwsDdbKeyConditionExpression";
    pub const FILTER_EXPRESSION: &str = "CamelAwsDdbFilterExpression";
    pub const CONDITION_EXPRESSION: &str = "CamelAwsDdbConditionExpression";
    pub const PROJECTION_EXPRESSION: &str = "CamelAwsDdbProjectionExpression";
    pub const EXPRESSION_ATTRIBUTE_NAMES: &str = "CamelAwsDdbExpressionAttributeNames";
    pub const EXPRESSION_ATTRIBUTE_VALUES: &str = "CamelAwsDdbExpressionAttributeValues";
    pub const FILTER_EXPRESSION_ATTRIBUTE_NAMES: &str = "CamelAwsDdbFilterExpressionAttributeNames";
    pub const FILTER_EXPRESSION_ATTRIBUTE_VALUES: &str = "CamelAwsDdbFilterExpressionAttributeValues";
    pub const BATCH_ITEMS: &str = "CamelAwsDdbBatchItems";
    pub const BATCH_RESPONSE: &str = "CamelAwsDdbBatchResponse";
    pub const UNPROCESSED_KEYS: &str = "CamelAwsDdbUnprocessedKeys";
    pub const READ_CAPACITY: &str = "CamelAwsDdbReadCapacity";
    pub const WRITE_CAPACITY: &str = "CamelAwsDdbWriteCapacity";
    pub const PROVISIONED_THROUGHPUT: &str = "CamelAwsDdbProvisionedThroughput";
    /// Cameleer extensions.
    pub const STREAM_VIEW_TYPE: &str = "CamelAwsDdbStreamViewType";
    pub const STREAM_ARN: &str = "CamelAwsDdbStreamArn";
}

/// Add the expression attribute names / values headers to a request.
fn expression_fields(ex: &Exchange, req: &mut ObjectMap) -> Result<()> {
    let names = super::header_json(ex, headers::EXPRESSION_ATTRIBUTE_NAMES).or_else(|| super::header_json(ex, headers::FILTER_EXPRESSION_ATTRIBUTE_NAMES));
    if let Some(n) = names {
        let m = n.as_object().ok_or_else(|| CamelError::illegal_argument("Expression attribute names must be a map"))?;
        req.insert("ExpressionAttributeNames".into(), Value::Object(m.iter().map(|(k, v)| (k.clone(), Value::from(v.to_string()))).collect()));
    }
    let values = super::header_json(ex, headers::EXPRESSION_ATTRIBUTE_VALUES).or_else(|| super::header_json(ex, headers::FILTER_EXPRESSION_ATTRIBUTE_VALUES));
    if let Some(v) = values {
        req.insert("ExpressionAttributeValues".into(), marshal_item(&v)?);
    }
    for (h, field) in [(headers::CONDITION_EXPRESSION, "ConditionExpression"), (headers::PROJECTION_EXPRESSION, "ProjectionExpression")] {
        if let Some(e) = header_str(ex, h) {
            req.insert(field.into(), Value::from(e));
        }
    }
    Ok(())
}

/// Marshal a value to a DynamoDB attribute value.
pub fn to_attribute_value(v: &Value) -> Value {
    match v {
        Value::Null => obj([("NULL", Value::Bool(true))]),
        Value::Bool(b) => obj([("BOOL", Value::Bool(*b))]),
        Value::Int(_) | Value::Float(_) => obj([("N", Value::from(v.to_string()))]),
        Value::String(s) => obj([("S", Value::from(s.clone()))]),
        Value::Bytes(b) => obj([("B", Value::from(base64_encode(b)))]),
        Value::Array(a) => obj([("L", Value::Array(a.iter().map(to_attribute_value).collect()))]),
        Value::Object(m) => obj([("M", marshal_map(m))]),
    }
}

fn marshal_map(m: &ObjectMap) -> Value {
    Value::Object(m.iter().map(|(k, v)| (k.clone(), to_attribute_value(v))).collect())
}

/// Marshal a map value (item, key) to an attribute-value map.
pub fn marshal_item(v: &Value) -> Result<Value> {
    match v {
        Value::Object(m) => Ok(marshal_map(m)),
        other => Err(CamelError::illegal_argument(format!("Expected a map of attributes, got {}", other.type_name()))),
    }
}

fn number(s: &str) -> Value {
    s.parse::<i64>().map(Value::Int).or_else(|_| s.parse::<f64>().map(Value::Float)).unwrap_or_else(|_| Value::String(s.to_string()))
}

/// Unmarshal an attribute value.
pub fn from_attribute_value(av: &Value) -> Value {
    let Some(m) = av.as_object() else { return av.clone() };
    let Some((t, v)) = m.iter().next() else { return Value::Null };
    match t.as_str() {
        "S" => v.clone(),
        "N" => number(&v.to_string()),
        "BOOL" => v.clone(),
        "NULL" => Value::Null,
        "B" => Value::Bytes(base64_decode(&v.to_string()).unwrap_or_default()),
        "SS" => v.clone(),
        "NS" => Value::Array(v.as_array().map(|a| a.iter().map(|x| number(&x.to_string())).collect()).unwrap_or_default()),
        "BS" => Value::Array(v.as_array().map(|a| a.iter().map(|x| Value::Bytes(base64_decode(&x.to_string()).unwrap_or_default())).collect()).unwrap_or_default()),
        "L" => Value::Array(v.as_array().map(|a| a.iter().map(from_attribute_value).collect()).unwrap_or_default()),
        "M" => unmarshal_item(v),
        _ => v.clone(),
    }
}

/// Unmarshal an attribute-value map.
pub fn unmarshal_item(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(m.iter().map(|(k, v)| (k.clone(), from_attribute_value(v))).collect()),
        _ => Value::Null,
    }
}

/// Marshal a conditions map (Query `KeyConditions`, Scan `ScanFilter`).
pub fn marshal_conditions(v: &Value) -> Result<Value> {
    let m = v.as_object().ok_or_else(|| CamelError::illegal_argument("Conditions must be a map"))?;
    let mut out = ObjectMap::new();
    for (attr, cond) in m {
        let c = match cond.as_object().filter(|c| c.contains_key("ComparisonOperator")) {
            Some(c) => {
                let list = c.get("AttributeValueList").and_then(|l| l.as_array()).map(|a| a.iter().map(to_attribute_value).collect()).unwrap_or_default();
                obj([("ComparisonOperator", c["ComparisonOperator"].clone()), ("AttributeValueList", Value::Array(list))])
            }
            None => obj([("ComparisonOperator", Value::from("EQ")), ("AttributeValueList", Value::Array(vec![to_attribute_value(cond)]))]),
        };
        out.insert(attr.clone(), c);
    }
    Ok(Value::Object(out))
}

/// The DynamoDB endpoint configuration and client.
pub struct DdbService {
    pub aws: AwsConfig,
    pub table: String,
    pub operation: Option<String>,
    pub consistent_read: bool,
    pub key_attribute_name: Option<String>,
    pub key_attribute_type: String,
    pub read_capacity: i64,
    pub write_capacity: i64,
    pub enabled_initial_describe_table: bool,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    if remaining.is_empty() {
        return Err(CamelError::illegal_argument("Table name must be specified: aws2-ddb://tableName"));
    }
    let aws = AwsConfig::from_opts(o)?;
    Ok(Arc::new(DdbService {
        table: remaining.to_string(),
        operation: o.get("operation"),
        consistent_read: o.bool("consistentRead", false)?,
        key_attribute_name: o.get("keyAttributeName"),
        key_attribute_type: o.string("keyAttributeType", "S"),
        read_capacity: o.i64("readCapacity", 1)?,
        write_capacity: o.i64("writeCapacity", 1)?,
        enabled_initial_describe_table: o.bool("enabledInitialDescribeTable", true)?,
        aws,
    }))
}

impl DdbService {
    /// Invoke a DynamoDB API action.
    pub fn call(&self, action: &str, body: Value) -> Result<Value> {
        self.aws.json_call("dynamodb", "dynamodb", "DynamoDB_20120810", action, &body, "1.0", kinds::DYNAMODB)
    }

    fn table(&self, ex: &Exchange) -> String {
        header_str(ex, headers::TABLE_NAME).unwrap_or_else(|| self.table.clone())
    }

    fn produce_op(&self, ex: &mut Exchange, op: &str) -> Result<()> {
        let table = self.table(ex);
        let mut req = ObjectMap::new();
        req.insert("TableName".to_string(), Value::from(table.clone()));
        let need = |name: &str| ex.header(name).filter(|v| !v.is_null()).cloned().ok_or_else(|| CamelError::illegal_argument(format!("{} requires the {} header", op, name)));
        if let Some(rv) = header_str(ex, headers::RETURN_VALUES) {
            req.insert("ReturnValues".into(), Value::from(rv));
        }
        let consistent = ex.header(headers::CONSISTENT_READ).and_then(|v| v.as_bool()).unwrap_or(self.consistent_read);
        let attr_names = ex.header(headers::ATTRIBUTE_NAMES).and_then(|v| v.as_array()).cloned();
        let set_attrs = |ex: &mut Exchange, r: &Value, field: &str| {
            if let Some(a) = r.get(field) {
                ex.set_header(headers::ATTRIBUTES, unmarshal_item(a));
            }
        };
        match op {
            "PutItem" => {
                req.insert("Item".into(), marshal_item(&need(headers::ITEM)?)?);
                expression_fields(ex, &mut req)?;
                let r = self.call("PutItem", Value::Object(req))?;
                set_attrs(ex, &r, "Attributes");
            }
            "GetItem" => {
                req.insert("Key".into(), marshal_item(&need(headers::KEY)?)?);
                req.insert("ConsistentRead".into(), Value::Bool(consistent));
                if let Some(a) = attr_names {
                    req.insert("AttributesToGet".into(), Value::Array(a));
                }
                let r = self.call("GetItem", Value::Object(req))?;
                ex.set_header(headers::ATTRIBUTES, r.get("Item").map(unmarshal_item).unwrap_or(Value::Null));
            }
            "DeleteItem" => {
                req.insert("Key".into(), marshal_item(&need(headers::KEY)?)?);
                expression_fields(ex, &mut req)?;
                let r = self.call("DeleteItem", Value::Object(req))?;
                set_attrs(ex, &r, "Attributes");
            }
            "UpdateItem" if header_str(ex, headers::UPDATE_EXPRESSION).is_some() => {
                req.insert("Key".into(), marshal_item(&need(headers::KEY)?)?);
                req.insert("UpdateExpression".into(), Value::from(header_str(ex, headers::UPDATE_EXPRESSION).unwrap_or_default()));
                expression_fields(ex, &mut req)?;
                let r = self.call("UpdateItem", Value::Object(req))?;
                set_attrs(ex, &r, "Attributes");
            }
            "UpdateItem" => {
                req.insert("Key".into(), marshal_item(&need(headers::KEY)?)?);
                expression_fields(ex, &mut req)?;
                let values = need(headers::UPDATE_VALUES)?;
                let m = values.as_object().ok_or_else(|| CamelError::illegal_argument("CamelAwsDdbUpdateValues must be a map"))?;
                let updates: ObjectMap = m
                    .iter()
                    .map(|(k, v)| {
                        let u = if v.is_null() { obj([("Action", Value::from("DELETE"))]) } else { obj([("Action", Value::from("PUT")), ("Value", to_attribute_value(v))]) };
                        (k.clone(), u)
                    })
                    .collect();
                req.insert("AttributeUpdates".into(), Value::Object(updates));
                let r = self.call("UpdateItem", Value::Object(req))?;
                set_attrs(ex, &r, "Attributes");
            }
            "Query" | "Scan" => {
                expression_fields(ex, &mut req)?;
                if let Some(f) = header_str(ex, headers::FILTER_EXPRESSION) {
                    req.insert("FilterExpression".into(), Value::from(f));
                }
                if op == "Query" {
                    match header_str(ex, headers::KEY_CONDITION_EXPRESSION) {
                        Some(kce) => {
                            req.insert("KeyConditionExpression".into(), Value::from(kce));
                        }
                        None => {
                            req.insert("KeyConditions".into(), marshal_conditions(&need(headers::KEY_CONDITIONS)?)?);
                        }
                    }
                    req.insert("ConsistentRead".into(), Value::Bool(consistent));
                    if let Some(f) = ex.header(headers::SCAN_INDEX_FORWARD).and_then(|v| v.as_bool()) {
                        req.insert("ScanIndexForward".into(), Value::Bool(f));
                    }
                } else if let Some(f) = ex.header(headers::SCAN_FILTER).filter(|v| !v.is_null()) {
                    req.insert("ScanFilter".into(), marshal_conditions(f)?);
                }
                if let Some(i) = header_str(ex, headers::INDEX_NAME) {
                    req.insert("IndexName".into(), Value::from(i));
                }
                if let Some(l) = ex.header(headers::LIMIT).and_then(|v| v.as_i64()) {
                    req.insert("Limit".into(), Value::Int(l));
                }
                if let Some(k) = ex.header(headers::START_KEY).filter(|v| !v.is_null()) {
                    req.insert("ExclusiveStartKey".into(), marshal_item(k)?);
                }
                if let Some(a) = attr_names {
                    req.insert("AttributesToGet".into(), Value::Array(a));
                }
                let r = self.call(op, Value::Object(req))?;
                let items: Vec<Value> = r.get("Items").and_then(|i| i.as_array()).map(|a| a.iter().map(unmarshal_item).collect()).unwrap_or_default();
                ex.set_header(headers::COUNT, r.get("Count").and_then(|c| c.as_i64()).unwrap_or(items.len() as i64));
                if let Some(s) = r.get("ScannedCount").and_then(|c| c.as_i64()) {
                    ex.set_header(headers::SCANNED_COUNT, s);
                }
                ex.set_header(headers::LAST_EVALUATED_KEY, r.get("LastEvaluatedKey").map(unmarshal_item).unwrap_or(Value::Null));
                ex.set_header(headers::ITEMS, Value::Array(items));
            }
            "DescribeTable" => {
                let r = self.call("DescribeTable", Value::Object(req))?;
                let t = r.get("Table").cloned().unwrap_or_default();
                ex.set_header(headers::TABLE_NAME, t.get("TableName").cloned().unwrap_or_default());
                ex.set_header(headers::TABLE_STATUS, t.get("TableStatus").cloned().unwrap_or_default());
                ex.set_header(headers::ITEM_COUNT, t.get("ItemCount").cloned().unwrap_or(Value::Int(0)));
                ex.set_header(headers::KEY_SCHEMA, t.get("KeySchema").cloned().unwrap_or_default());
                if let Some(a) = t.get("LatestStreamArn") {
                    ex.set_header(headers::STREAM_ARN, a.clone());
                }
            }
            "BatchGetItems" => {
                let items = super::header_json(ex, headers::BATCH_ITEMS).ok_or_else(|| CamelError::illegal_argument("BatchGetItems requires the CamelAwsDdbBatchItems header"))?;
                let tables = items.as_object().ok_or_else(|| CamelError::illegal_argument("CamelAwsDdbBatchItems must be a map of table name to keys"))?;
                let mut ri = ObjectMap::new();
                for (t, spec) in tables {
                    let mut kaa = ObjectMap::new();
                    let keys = match spec {
                        Value::Array(a) => a.clone(),
                        _ => spec.get("Keys").and_then(|k| k.as_array()).cloned().unwrap_or_default(),
                    };
                    kaa.insert("Keys".into(), Value::Array(keys.iter().map(marshal_item).collect::<Result<Vec<_>>>()?));
                    for f in ["ConsistentRead", "ProjectionExpression", "AttributesToGet", "ExpressionAttributeNames"] {
                        if let Some(v) = spec.get(f) {
                            kaa.insert(f.into(), v.clone());
                        }
                    }
                    ri.insert(t.clone(), Value::Object(kaa));
                }
                let r = self.call("BatchGetItem", obj([("RequestItems", Value::Object(ri))]))?;
                let resp: ObjectMap = r
                    .get("Responses")
                    .and_then(|x| x.as_object())
                    .map(|m| m.iter().map(|(t, items)| (t.clone(), Value::Array(items.as_array().map(|a| a.iter().map(unmarshal_item).collect()).unwrap_or_default()))).collect())
                    .unwrap_or_default();
                let unprocessed: ObjectMap = r
                    .get("UnprocessedKeys")
                    .and_then(|x| x.as_object())
                    .map(|m| m.iter().map(|(t, k)| (t.clone(), Value::Array(k.get("Keys").and_then(|a| a.as_array()).map(|a| a.iter().map(unmarshal_item).collect()).unwrap_or_default()))).collect())
                    .unwrap_or_default();
                ex.set_header(headers::BATCH_RESPONSE, Value::Object(resp));
                ex.set_header(headers::UNPROCESSED_KEYS, Value::Object(unprocessed));
            }
            "UpdateTable" => {
                let rc = ex.header(headers::READ_CAPACITY).and_then(|v| v.as_i64().or_else(|| v.to_string().parse().ok()));
                let wc = ex.header(headers::WRITE_CAPACITY).and_then(|v| v.as_i64().or_else(|| v.to_string().parse().ok()));
                if rc.is_some() || wc.is_some() {
                    req.insert(
                        "ProvisionedThroughput".into(),
                        obj([("ReadCapacityUnits", Value::Int(rc.unwrap_or(self.read_capacity))), ("WriteCapacityUnits", Value::Int(wc.unwrap_or(self.write_capacity)))]),
                    );
                }
                if let Some(view) = header_str(ex, headers::STREAM_VIEW_TYPE) {
                    let spec = if view.eq_ignore_ascii_case("DISABLED") { obj([("StreamEnabled", Value::Bool(false))]) } else { obj([("StreamEnabled", Value::Bool(true)), ("StreamViewType", Value::from(view))]) };
                    req.insert("StreamSpecification".into(), spec);
                }
                let r = self.call("UpdateTable", Value::Object(req))?;
                let t = r.get("TableDescription").cloned().unwrap_or_default();
                ex.set_header(headers::TABLE_STATUS, t.get("TableStatus").cloned().unwrap_or_default());
                ex.set_header(headers::PROVISIONED_THROUGHPUT, t.get("ProvisionedThroughput").cloned().unwrap_or_default());
                if let Some(a) = t.get("LatestStreamArn") {
                    ex.set_header(headers::STREAM_ARN, a.clone());
                }
            }
            "DeleteTable" => {
                self.call("DeleteTable", Value::Object(req))?;
            }
            other => return Err(unsupported("aws2-ddb", other)),
        }
        Ok(())
    }
}

impl CloudService for DdbService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "PutItem");
        self.produce_op(ex, &op)
    }

    fn on_start(&self) -> Result<()> {
        if !self.enabled_initial_describe_table {
            return Ok(());
        }
        match self.call("DescribeTable", obj([("TableName", Value::from(self.table.clone()))])) {
            Ok(_) => Ok(()),
            Err(e) if error_code(&e).as_deref() == Some("ResourceNotFoundException") => match &self.key_attribute_name {
                Some(key) => {
                    let body = obj([
                        ("TableName", Value::from(self.table.clone())),
                        ("KeySchema", Value::Array(vec![obj([("AttributeName", Value::from(key.clone())), ("KeyType", Value::from("HASH"))])])),
                        (
                            "AttributeDefinitions",
                            Value::Array(vec![obj([("AttributeName", Value::from(key.clone())), ("AttributeType", Value::from(self.key_attribute_type.clone()))])]),
                        ),
                        (
                            "ProvisionedThroughput",
                            obj([("ReadCapacityUnits", Value::Int(self.read_capacity)), ("WriteCapacityUnits", Value::Int(self.write_capacity))]),
                        ),
                    ]);
                    self.call("CreateTable", body).map(|_| ())
                }
                None => Err(e),
            },
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marshalling_roundtrip() {
        let item = Value::object([
            ("id", Value::from("a")),
            ("n", Value::Int(3)),
            ("f", Value::Float(1.5)),
            ("ok", Value::Bool(true)),
            ("none", Value::Null),
            ("raw", Value::Bytes(vec![1, 2])),
            ("tags", Value::from(vec!["x", "y"])),
            ("nested", Value::object([("k", Value::Int(1))])),
        ]);
        let m = marshal_item(&item).unwrap();
        assert_eq!(m.get("n").unwrap().to_json(), r#"{"N":"3"}"#);
        assert_eq!(m.get("tags").unwrap().to_json(), r#"{"L":[{"S":"x"},{"S":"y"}]}"#);
        assert_eq!(unmarshal_item(&m), item);
        let ss = cameleer_core::json::parse(r#"{"SS":["a","b"]}"#).unwrap();
        assert_eq!(from_attribute_value(&ss), Value::from(vec!["a", "b"]));
    }

    #[test]
    fn conditions() {
        let c = marshal_conditions(&Value::object([
            ("id", Value::from("a")),
            ("ts", Value::object([("ComparisonOperator", Value::from("GT")), ("AttributeValueList", Value::Array(vec![Value::Int(5)]))])),
        ]))
        .unwrap();
        assert_eq!(c.get("id").unwrap().to_json(), r#"{"ComparisonOperator":"EQ","AttributeValueList":[{"S":"a"}]}"#);
        assert_eq!(c.get("ts").unwrap().get("ComparisonOperator").unwrap(), &Value::from("GT"));
    }
}
