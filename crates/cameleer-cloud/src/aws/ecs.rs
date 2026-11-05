//! The `aws2-ecs` and `aws2-eks` components (Camel `camel-aws2-ecs` / `camel-aws2-eks`,
//! producers): ECS over AWS JSON 1.1 (signing name `ecs`), EKS over its REST API (signing name
//! `eks`).
//!
//! `aws2-ecs://label?operation=...` (`operation` / `CamelAwsECSOperation`):
//! `listClusters` (`CamelAwsECSMaxResults`; body = list of cluster ARNs, all pages),
//! `createCluster` / `describeCluster` / `deleteCluster` (`CamelAwsECSClusterName`; body =
//! the cluster map).
//!
//! `aws2-eks://label?operation=...` (`operation` / `CamelAwsEKSOperation`):
//! `listClusters` (`CamelAwsEKSMaxResults`; body = list of names, all pages),
//! `createCluster` (`CamelAwsEKSClusterName`, `CamelAwsEKSRoleARN`, `CamelAwsEKSVPCConfig`
//! map of `subnetIds` / `securityGroupIds`), `describeCluster`, `deleteCluster` (body = the
//! cluster map).

use std::sync::Arc;

use cameleer_core::error::Result;
use cameleer_core::message::Exchange;
use cameleer_core::value::{ObjectMap, Value};

use super::{header_json, need, put, AwsConfig, JsonApi};
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{header_i64, kinds, obj, uri_encode, Opts};

/// ECS header names (Camel `ECS2Constants`).
pub mod ecs_headers {
    pub const OPERATION: &str = "CamelAwsECSOperation";
    pub const MAX_RESULTS: &str = "CamelAwsECSMaxResults";
    pub const CLUSTER_NAME: &str = "CamelAwsECSClusterName";
}

/// EKS header names (Camel `EKS2Constants`).
pub mod eks_headers {
    pub const OPERATION: &str = "CamelAwsEKSOperation";
    pub const MAX_RESULTS: &str = "CamelAwsEKSMaxResults";
    pub const CLUSTER_NAME: &str = "CamelAwsEKSClusterName";
    pub const ROLE_ARN: &str = "CamelAwsEKSRoleARN";
    pub const VPC_CONFIG: &str = "CamelAwsEKSVPCConfig";
}

const ECS: JsonApi = JsonApi { host: "ecs", service: "ecs", target: "AmazonEC2ContainerServiceV20141113", version: "1.1", kind: kinds::ECS };

/// The ECS endpoint configuration and client.
pub struct EcsService {
    pub aws: AwsConfig,
    pub operation: Option<String>,
}

/// Factory for `aws2-ecs`.
pub fn create_ecs(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["ecsClient", "pojoRequest"]);
    Ok(Arc::new(EcsService { operation: o.get("operation"), aws }))
}

impl CloudService for EcsService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, ecs_headers::OPERATION, &self.operation, "");
        let call = |action: &str, body: Value| self.aws.api(&ECS, action, &body);
        match op.as_str() {
            "listClusters" => {
                let mut m = ObjectMap::new();
                put(&mut m, "maxResults", header_i64(ex, ecs_headers::MAX_RESULTS));
                ex.set_body(Value::Array(self.aws.api_paged(&ECS, "ListClusters", m, "clusterArns", "nextToken")?));
            }
            "createCluster" => {
                let r = call("CreateCluster", obj([("clusterName", Value::from(need(ex, ecs_headers::CLUSTER_NAME)?))]))?;
                ex.set_body(r.get("cluster").cloned().unwrap_or_default());
            }
            "describeCluster" => {
                let r = call("DescribeClusters", obj([("clusters", Value::from(vec![need(ex, ecs_headers::CLUSTER_NAME)?]))]))?;
                ex.set_body(r.get("clusters").and_then(|c| c.as_array()).and_then(|c| c.first().cloned()).unwrap_or_default());
            }
            "deleteCluster" => {
                let r = call("DeleteCluster", obj([("cluster", Value::from(need(ex, ecs_headers::CLUSTER_NAME)?))]))?;
                ex.set_body(r.get("cluster").cloned().unwrap_or_default());
            }
            other => return Err(unsupported("ECS", other)),
        }
        Ok(())
    }
}

/// The EKS endpoint configuration and client.
pub struct EksService {
    pub aws: AwsConfig,
    pub operation: Option<String>,
}

/// Factory for `aws2-eks`.
pub fn create_eks(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["eksClient", "pojoRequest"]);
    Ok(Arc::new(EksService { operation: o.get("operation"), aws }))
}

impl EksService {
    fn rest(&self, method: &str, path: &str, query: &[(String, String)], body: Option<&Value>) -> Result<Value> {
        self.aws.rest_json(method, "eks", path, query, body, "eks", kinds::EKS)
    }
}

impl CloudService for EksService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, eks_headers::OPERATION, &self.operation, "");
        let cluster_path = |ex: &Exchange| -> Result<String> { Ok(format!("/clusters/{}", uri_encode(&need(ex, eks_headers::CLUSTER_NAME)?, false))) };
        match op.as_str() {
            "listClusters" => {
                let mut all = Vec::new();
                let mut token: Option<String> = None;
                loop {
                    let mut q = Vec::new();
                    if let Some(m) = header_i64(ex, eks_headers::MAX_RESULTS) {
                        q.push(("maxResults".to_string(), m.to_string()));
                    }
                    if let Some(t) = &token {
                        q.push(("nextToken".to_string(), t.clone()));
                    }
                    let r = self.rest("GET", "/clusters", &q, None)?;
                    all.extend(r.get("clusters").and_then(|b| b.as_array()).cloned().unwrap_or_default());
                    token = r.get("nextToken").map(|t| t.to_string()).filter(|t| !t.is_empty());
                    if token.is_none() {
                        break;
                    }
                }
                ex.set_body(Value::Array(all));
            }
            "createCluster" => {
                let mut m = ObjectMap::new();
                m.insert("name".into(), Value::from(need(ex, eks_headers::CLUSTER_NAME)?));
                m.insert("roleArn".into(), Value::from(need(ex, eks_headers::ROLE_ARN)?));
                m.insert("resourcesVpcConfig".into(), header_json(ex, eks_headers::VPC_CONFIG).unwrap_or_else(|| Value::Object(ObjectMap::new())));
                let r = self.rest("POST", "/clusters", &[], Some(&Value::Object(m)))?;
                ex.set_body(r.get("cluster").cloned().unwrap_or_default());
            }
            "describeCluster" => {
                let r = self.rest("GET", &cluster_path(ex)?, &[], None)?;
                ex.set_body(r.get("cluster").cloned().unwrap_or_default());
            }
            "deleteCluster" => {
                let r = self.rest("DELETE", &cluster_path(ex)?, &[], None)?;
                ex.set_body(r.get("cluster").cloned().unwrap_or_default());
            }
            other => return Err(unsupported("EKS", other)),
        }
        Ok(())
    }
}
