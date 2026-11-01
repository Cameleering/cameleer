//! The `aws2-iam` component (Camel `camel-aws2-iam`, producer subset) over the IAM Query API
//! (global endpoint `iam.amazonaws.com`, signed for `us-east-1`).
//!
//! `aws2-iam://label?operation=...`
//!
//! Operations (`operation` / `CamelAwsIAMOperation`, required): `createUser`, `getUser`,
//! `deleteUser` (`CamelAwsIAMUsername`), `listUsers` (body = list of users), `createGroup`,
//! `deleteGroup`, `listGroups` (`CamelAwsIAMGroupName`), `addUserToGroup`,
//! `removeUserFromGroup`, `createAccessKey` (body = `{accessKeyId, secretAccessKey, status}`),
//! `listAccessKeys`, `deleteAccessKey` (`CamelAwsIAMAccessKeyID`), `updateAccessKey`
//! (`CamelAwsIAMAccessKeyID`, `CamelAwsIAMAccessKeyStatus` Active / Inactive). Users and groups
//! are maps `{userName, userId, arn, path, createDate}` / `{groupName, groupId, arn, path,
//! createDate}`; `getGroup` returns the group with its `users`.
//!
//! Roles and policies: `createRole` (`CamelAwsIAMRoleName`,
//! `CamelAwsIAMAssumeRolePolicyDocument`, `CamelAwsIAMPath`, `CamelAwsIAMDescription`),
//! `getRole`, `deleteRole`, `listRoles`; `createPolicy` (`CamelAwsIAMPolicyName`,
//! `CamelAwsIAMPolicyDocument` or the body; body = `{policyName, policyId, arn,
//! defaultVersionId}`), `getPolicy`, `deletePolicy` (`CamelAwsIAMPolicyArn`), `listPolicies`
//! (local policies); `attachUserPolicy` / `detachUserPolicy` / `listAttachedUserPolicies`,
//! `attachGroupPolicy` / `detachGroupPolicy` / `listAttachedGroupPolicies`, `attachRolePolicy` /
//! `detachRolePolicy` / `listAttachedRolePolicies` (body of the list operations =
//! `{policyName, policyArn}` maps); instance profiles `createInstanceProfile`,
//! `getInstanceProfile`, `deleteInstanceProfile`, `listInstanceProfiles`,
//! `addRoleToInstanceProfile`, `removeRoleFromInstanceProfile` (`CamelAwsIAMInstanceProfileName`,
//! `CamelAwsIAMRoleName`).

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
    pub const ACCESS_KEY_STATUS: &str = "CamelAwsIAMAccessKeyStatus";
    pub const ROLE_NAME: &str = "CamelAwsIAMRoleName";
    pub const ASSUME_ROLE_POLICY_DOCUMENT: &str = "CamelAwsIAMAssumeRolePolicyDocument";
    pub const POLICY_NAME: &str = "CamelAwsIAMPolicyName";
    pub const POLICY_DOCUMENT: &str = "CamelAwsIAMPolicyDocument";
    pub const POLICY_ARN: &str = "CamelAwsIAMPolicyArn";
    pub const INSTANCE_PROFILE_NAME: &str = "CamelAwsIAMInstanceProfileName";
    pub const PATH: &str = "CamelAwsIAMPath";
    pub const DESCRIPTION: &str = "CamelAwsIAMDescription";
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
const ROLE: &[(&str, &str)] = &[("RoleName", "roleName"), ("RoleId", "roleId"), ("Arn", "arn"), ("Path", "path"), ("CreateDate", "createDate"), ("Description", "description")];
const POLICY: &[(&str, &str)] = &[("PolicyName", "policyName"), ("PolicyId", "policyId"), ("Arn", "arn"), ("Path", "path"), ("DefaultVersionId", "defaultVersionId"), ("AttachmentCount", "attachmentCount")];
const ATTACHED: &[(&str, &str)] = &[("PolicyName", "policyName"), ("PolicyArn", "policyArn")];
const PROFILE: &[(&str, &str)] = &[("InstanceProfileName", "instanceProfileName"), ("InstanceProfileId", "instanceProfileId"), ("Arn", "arn"), ("Path", "path")];

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
        let role = |ex: &Exchange| need(ex, headers::ROLE_NAME, "Role name").map(|r| vec![("RoleName".to_string(), r)]);
        let policy = |ex: &Exchange| need(ex, headers::POLICY_ARN, "Policy ARN").map(|r| vec![("PolicyArn".to_string(), r)]);
        let profile = |ex: &Exchange| need(ex, headers::INSTANCE_PROFILE_NAME, "Instance profile name").map(|r| vec![("InstanceProfileName".to_string(), r)]);
        let first = |x: &Xml, tag: &str, fields: &[(&str, &str)]| {
            let node = x.descendants(tag).first().copied().unwrap_or(x.root());
            entity(x, node, fields)
        };
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
            "updateAccessKey" => {
                let mut p = vec![("AccessKeyId".to_string(), need(ex, headers::ACCESS_KEY_ID, "Access key id")?), ("Status".to_string(), need(ex, headers::ACCESS_KEY_STATUS, "Access key status")?)];
                if let Some(u) = header_str(ex, headers::USERNAME) {
                    p.push(("UserName".into(), u));
                }
                self.call("UpdateAccessKey", p)?;
            }
            "getGroup" => {
                let x = self.call("GetGroup", group(ex)?)?;
                let node = x.descendants("Group").first().copied().unwrap_or(x.root());
                let mut g = entity(&x, node, GROUP);
                let users: Vec<Value> = x.descendants("member").into_iter().map(|m| entity(&x, m, USER)).collect();
                if let Value::Object(m) = &mut g {
                    m.insert("users".into(), Value::Array(users));
                }
                ex.set_body(g);
            }
            "createRole" => {
                let mut p = role(ex)?;
                p.push(("AssumeRolePolicyDocument".into(), need(ex, headers::ASSUME_ROLE_POLICY_DOCUMENT, "Assume role policy document")?));
                if let Some(v) = header_str(ex, headers::PATH) {
                    p.push(("Path".into(), v));
                }
                if let Some(v) = header_str(ex, headers::DESCRIPTION) {
                    p.push(("Description".into(), v));
                }
                let x = self.call("CreateRole", p)?;
                ex.set_body(first(&x, "Role", ROLE));
            }
            "getRole" => {
                let x = self.call("GetRole", role(ex)?)?;
                ex.set_body(first(&x, "Role", ROLE));
            }
            "deleteRole" => {
                self.call("DeleteRole", role(ex)?)?;
            }
            "listRoles" => {
                let v = self.list_all("ListRoles", Vec::new(), "RoleName", ROLE)?;
                ex.set_body(v);
            }
            "createPolicy" => {
                let doc = header_str(ex, headers::POLICY_DOCUMENT).unwrap_or_else(|| match ex.body() {
                    Value::Object(_) | Value::Array(_) => ex.body().to_json(),
                    other => other.to_string(),
                });
                let mut p = vec![("PolicyName".to_string(), need(ex, headers::POLICY_NAME, "Policy name")?), ("PolicyDocument".to_string(), doc)];
                if let Some(v) = header_str(ex, headers::PATH) {
                    p.push(("Path".into(), v));
                }
                if let Some(v) = header_str(ex, headers::DESCRIPTION) {
                    p.push(("Description".into(), v));
                }
                let x = self.call("CreatePolicy", p)?;
                ex.set_body(first(&x, "Policy", POLICY));
            }
            "getPolicy" => {
                let x = self.call("GetPolicy", policy(ex)?)?;
                ex.set_body(first(&x, "Policy", POLICY));
            }
            "deletePolicy" => {
                self.call("DeletePolicy", policy(ex)?)?;
            }
            "listPolicies" => {
                let v = self.list_all("ListPolicies", vec![("Scope".into(), "Local".into())], "PolicyName", POLICY)?;
                ex.set_body(v);
            }
            "attachUserPolicy" | "detachUserPolicy" | "attachGroupPolicy" | "detachGroupPolicy" | "attachRolePolicy" | "detachRolePolicy" => {
                let mut p = policy(ex)?;
                p.extend(if op.ends_with("UserPolicy") {
                    user(ex)?
                } else if op.ends_with("GroupPolicy") {
                    group(ex)?
                } else {
                    role(ex)?
                });
                let mut action = op.clone();
                action[..1].make_ascii_uppercase();
                self.call(&action, p)?;
            }
            "listAttachedUserPolicies" | "listAttachedGroupPolicies" | "listAttachedRolePolicies" => {
                let p = if op.contains("User") {
                    user(ex)?
                } else if op.contains("Group") {
                    group(ex)?
                } else {
                    role(ex)?
                };
                let mut action = op.clone();
                action[..1].make_ascii_uppercase();
                let v = self.list_all(&action, p, "PolicyArn", ATTACHED)?;
                ex.set_body(v);
            }
            "createInstanceProfile" | "getInstanceProfile" => {
                let mut p = profile(ex)?;
                if op == "createInstanceProfile" {
                    if let Some(v) = header_str(ex, headers::PATH) {
                        p.push(("Path".into(), v));
                    }
                }
                let mut action = op.clone();
                action[..1].make_ascii_uppercase();
                let x = self.call(&action, p)?;
                let node = x.descendants("InstanceProfile").first().copied().unwrap_or(x.root());
                let mut v = entity(&x, node, PROFILE);
                let roles: Vec<Value> = x.descendants("member").into_iter().filter(|m| x.child(*m, "RoleName").is_some()).map(|m| Value::from(x.child_text(m, "RoleName").unwrap_or_default())).collect();
                if let Value::Object(m) = &mut v {
                    m.insert("roles".into(), Value::Array(roles));
                }
                ex.set_body(v);
            }
            "deleteInstanceProfile" => {
                self.call("DeleteInstanceProfile", profile(ex)?)?;
            }
            "listInstanceProfiles" => {
                let v = self.list_all("ListInstanceProfiles", Vec::new(), "InstanceProfileName", PROFILE)?;
                ex.set_body(v);
            }
            "addRoleToInstanceProfile" | "removeRoleFromInstanceProfile" => {
                let mut p = profile(ex)?;
                p.extend(role(ex)?);
                let mut action = op.clone();
                action[..1].make_ascii_uppercase();
                self.call(&action, p)?;
            }
            "" => return Err(CamelError::illegal_argument("aws2-iam requires an operation (operation option or CamelAwsIAMOperation header)")),
            other => return Err(unsupported("aws2-iam", other)),
        }
        Ok(())
    }
}
