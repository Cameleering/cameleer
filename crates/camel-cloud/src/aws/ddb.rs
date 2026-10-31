//! The `aws2-ddb` component (Camel `camel-aws2-ddb`, producer) over the DynamoDB JSON 1.0 API.
//!
//! `aws2-ddb://tableName?operation=PutItem|GetItem|UpdateItem|DeleteItem|Query|Scan|DescribeTable|DeleteTable`
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
//! On start the table is described and, when missing and `keyAttributeName` is set, created
//! (`keyAttributeName`, `keyAttributeType` default `S`, `readCapacity`, `writeCapacity`).

use std::sync::Arc;

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::{ObjectMap, Value};

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
                let r = self.call("DeleteItem", Value::Object(req))?;
                set_attrs(ex, &r, "Attributes");
            }
            "UpdateItem" => {
                req.insert("Key".into(), marshal_item(&need(headers::KEY)?)?);
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
                if op == "Query" {
                    req.insert("KeyConditions".into(), marshal_conditions(&need(headers::KEY_CONDITIONS)?)?);
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
        let ss = camel_core::json::parse(r#"{"SS":["a","b"]}"#).unwrap();
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
