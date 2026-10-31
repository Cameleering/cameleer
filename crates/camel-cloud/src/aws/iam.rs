//! The `aws2-iam` component (Camel `camel-aws2-iam`, producer subset) over the IAM Query API
//! (global endpoint `iam.amazonaws.com`, signed for `us-east-1`).
//!
//! `aws2-iam://label?operation=...`
//!
//! Operations (`operation` / `CamelAwsIAMOperation`, required): `createUser`, `getUser`,
//! `deleteUser` (`CamelAwsIAMUsername`), `listUsers` (body = list of users), `createGroup`,
//! `deleteGroup`, `listGroups` (`CamelAwsIAMGroupName`), `addUserToGroup`,
//! `removeUserFromGroup`, `createAccessKey` (body = `{accessKeyId, secretAccessKey, status}`),
//! `listAccessKeys`, `deleteAccessKey` (`CamelAwsIAMAccessKeyID`). Users and groups are maps
//! `{userName, userId, arn, path, createDate}` / `{groupName, groupId, arn, path, createDate}`.

use std::sync::Arc;

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::{ObjectMap, Value};

use super::AwsConfig;
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{header_str, kinds, Opts, Xml};

/// Header names (Camel `IAM2Constants`).
pub mod headers {
    pub const OPERATION: &str = "CamelAwsIAMOperation";
    pub const USERNAME: &str = "CamelAwsIAMUsername";
    pub const GROUP_NAME: &str = "CamelAwsIAMGroupName";
    pub const GROUP_PATH: &str = "CamelAwsIAMGroupPath";
    pub const ACCESS_KEY_ID: &str = "CamelAwsIAMAccessKeyID";
}

/// The IAM endpoint configuration and client.
pub struct IamService {
    pub aws: AwsConfig,
    pub operation: Option<String>,
}

/// Factory for the component.
pub fn create(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["iamClient", "pojoRequest"]);
    Ok(Arc::new(IamService { operation: o.get("operation"), aws }))
}

fn entity(x: &Xml, node: usize, fields: &[(&str, &str)]) -> Value {
    let mut m = ObjectMap::new();
    for (xml_name, key) in fields {
        m.insert(key.to_string(), Value::from(x.child_text(node, xml_name).unwrap_or_default()));
    }
    Value::Object(m)
}

const USER: &[(&str, &str)] = &[("UserName", "userName"), ("UserId", "userId"), ("Arn", "arn"), ("Path", "path"), ("CreateDate", "createDate")];
const GROUP: &[(&str, &str)] = &[("GroupName", "groupName"), ("GroupId", "groupId"), ("Arn", "arn"), ("Path", "path"), ("CreateDate", "createDate")];
const KEY: &[(&str, &str)] = &[("UserName", "userName"), ("AccessKeyId", "accessKeyId"), ("SecretAccessKey", "secretAccessKey"), ("Status", "status"), ("CreateDate", "createDate")];

impl IamService {
    /// Invoke an IAM action.
    pub fn call(&self, action: &str, params: Vec<(String, String)>) -> Result<Xml> {
        self.aws.query_call(self.aws.global_endpoint("iam.amazonaws.com")?, "iam", "us-east-1", "2010-05-08", action, params, kinds::IAM)
    }

    fn list_all(&self, action: &str, params: Vec<(String, String)>, element: &str, fields: &[(&str, &str)]) -> Result<Value> {
        let mut out = Vec::new();
        let mut marker: Option<String> = None;
        loop {
            let mut p = params.clone();
            if let Some(m) = &marker {
                p.push(("Marker".into(), m.clone()));
            }
            let x = self.call(action, p)?;
            for m in x.descendants("member") {
                if x.child(m, element).is_some() {
                    out.push(entity(&x, m, fields));
                }
            }
            marker = if x.find_text("IsTruncated").as_deref() == Some("true") { x.find_text("Marker") } else { None };
            if marker.is_none() {
                return Ok(Value::Array(out));
            }
        }
    }
}

fn need(ex: &Exchange, h: &str, what: &str) -> Result<String> {
    header_str(ex, h).ok_or_else(|| CamelError::illegal_argument(format!("{} must be specified ({})", what, h)))
}

impl CloudService for IamService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "");
        let user = |ex: &Exchange| need(ex, headers::USERNAME, "User name").map(|u| vec![("UserName".to_string(), u)]);
        let group = |ex: &Exchange| need(ex, headers::GROUP_NAME, "Group name").map(|g| vec![("GroupName".to_string(), g)]);
        match op.as_str() {
            "createUser" | "getUser" => {
                let x = self.call(if op == "createUser" { "CreateUser" } else { "GetUser" }, user(ex)?)?;
                let node = x.descendants("User").first().copied().unwrap_or(x.root());
                ex.set_body(entity(&x, node, USER));
            }
            "deleteUser" => {
                self.call("DeleteUser", user(ex)?)?;
            }
            "listUsers" => {
                let v = self.list_all("ListUsers", Vec::new(), "UserName", USER)?;
                ex.set_body(v);
            }
            "createGroup" => {
                let mut p = group(ex)?;
                if let Some(path) = header_str(ex, headers::GROUP_PATH) {
                    p.push(("Path".into(), path));
                }
                let x = self.call("CreateGroup", p)?;
                let node = x.descendants("Group").first().copied().unwrap_or(x.root());
                ex.set_body(entity(&x, node, GROUP));
            }
            "deleteGroup" => {
                self.call("DeleteGroup", group(ex)?)?;
            }
            "listGroups" => {
                let v = self.list_all("ListGroups", Vec::new(), "GroupName", GROUP)?;
                ex.set_body(v);
            }
            "addUserToGroup" | "removeUserFromGroup" => {
                let mut p = group(ex)?;
                p.extend(user(ex)?);
                self.call(if op == "addUserToGroup" { "AddUserToGroup" } else { "RemoveUserFromGroup" }, p)?;
            }
            "createAccessKey" => {
                let x = self.call("CreateAccessKey", user(ex)?)?;
                let node = x.descendants("AccessKey").first().copied().unwrap_or(x.root());
                ex.set_body(entity(&x, node, KEY));
            }
            "listAccessKeys" => {
                let p = header_str(ex, headers::USERNAME).map(|u| vec![("UserName".to_string(), u)]).unwrap_or_default();
                let v = self.list_all("ListAccessKeys", p, "AccessKeyId", KEY)?;
                ex.set_body(v);
            }
            "deleteAccessKey" => {
                let mut p = vec![("AccessKeyId".to_string(), need(ex, headers::ACCESS_KEY_ID, "Access key id")?)];
                if let Some(u) = header_str(ex, headers::USERNAME) {
                    p.push(("UserName".into(), u));
                }
                self.call("DeleteAccessKey", p)?;
            }
            "" => return Err(CamelError::illegal_argument("aws2-iam requires an operation (operation option or CamelAwsIAMOperation header)")),
            other => return Err(unsupported("aws2-iam", other)),
        }
        Ok(())
    }
}
