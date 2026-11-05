//! DSL methods for data types (route contracts, `transformDataType`), the service call,
//! script, poll and kamelet EIPs, and `removeVariables`.

use crate::builder::IntoExpression;
use crate::error::CamelError;
use crate::model::RouteDefinition;
use crate::processor::basic::{RemoveDefinition, RemoveTarget, ToDefinition};
use crate::processor::poll::PollDefinition;
use crate::processor::script::ScriptDefinition;
use crate::processor::service_call::{DiscoveryConfig, FilterConfig, ServiceCallDefinition, ServiceDefinition, ServiceDiscovery, ServiceLoadBalancer};
use crate::processor::transformer::{DataType, TransformDataTypeDefinition, TypeDeclaration, INPUT_TYPE, OUTPUT_TYPE};

impl RouteDefinition {
    // --- data types ---

    /// Declare the route input type (`json:Order`); the message is transformed to it on
    /// arrival and validated when `validate` is true.
    pub fn input_type(&mut self, urn: &str, validate: bool) -> &mut Self {
        let d = TypeDeclaration { data_type: DataType::parse(urn), validate };
        self.properties.insert(INPUT_TYPE.to_string(), d.encode());
        self
    }

    /// Declare the route output type; applied when the route completes successfully.
    pub fn output_type(&mut self, urn: &str, validate: bool) -> &mut Self {
        let d = TypeDeclaration { data_type: DataType::parse(urn), validate };
        self.properties.insert(OUTPUT_TYPE.to_string(), d.encode());
        self
    }

    /// Camel 4 `transformDataType`: transform the message to `to_type` (from `from_type`,
    /// or from the message's current data type).
    pub fn transform_data_type(&mut self, from_type: Option<&str>, to_type: &str) -> &mut Self {
        self.add(Box::new(TransformDataTypeDefinition::new(from_type.map(String::from), to_type)))
    }

    // --- service call ---

    /// Call a service by name (configure with `static_service_discovery`,
    /// `service_call_uri`, ... right after).
    pub fn service_call(&mut self, name: impl Into<String>) -> &mut Self {
        self.add(Box::new(ServiceCallDefinition::new(name)))
    }

    /// Add a fully configured service call.
    pub fn service_call_with(&mut self, def: ServiceCallDefinition) -> &mut Self {
        self.add(Box::new(def))
    }

    fn with_service_call(&mut self, option: &str, f: impl FnOnce(&mut ServiceCallDefinition) -> Result<(), CamelError>) -> &mut Self {
        let r = match self.last_mut().and_then(|d| d.as_any_mut().downcast_mut::<ServiceCallDefinition>()) {
            Some(d) => f(d),
            None => Err(CamelError::illegal_argument(format!("{} must follow service_call()", option))),
        };
        if let Err(e) = r {
            self.add_build_error(e);
        }
        self
    }

    /// Static servers `[name@]host:port,...`.
    pub fn static_service_discovery(&mut self, servers: &str) -> &mut Self {
        let s = servers.to_string();
        self.with_service_call("staticServiceDiscovery", move |d| {
            d.discovery.push(DiscoveryConfig::Static(ServiceDefinition::parse_list(&s)?));
            Ok(())
        })
    }

    /// A custom service discovery.
    pub fn service_discovery(&mut self, sd: impl ServiceDiscovery + 'static) -> &mut Self {
        let sd: std::sync::Arc<dyn ServiceDiscovery> = std::sync::Arc::new(sd);
        self.with_service_call("serviceDiscovery", move |d| {
            d.discovery.push(DiscoveryConfig::Custom(sd));
            Ok(())
        })
    }

    /// Uri template (`http://{host}:{port}/path`) or uri containing the service name.
    pub fn service_call_uri(&mut self, uri: impl Into<String>) -> &mut Self {
        let u = uri.into();
        self.with_service_call("uri", move |d| {
            d.uri = Some(u);
            Ok(())
        })
    }

    /// Component (scheme) of the default uri (default `http`).
    pub fn service_call_component(&mut self, c: impl Into<String>) -> &mut Self {
        let c = c.into();
        self.with_service_call("component", move |d| {
            d.component = c;
            Ok(())
        })
    }

    /// Expression computing the endpoint uri (the `CamelServiceCallService*` headers are set).
    pub fn service_call_expression(&mut self, e: impl IntoExpression) -> &mut Self {
        let e = e.into_expression();
        self.with_service_call("expression", move |d| {
            d.expression = Some(e);
            Ok(())
        })
    }

    /// Load balancer of the service call.
    pub fn service_call_load_balancer(&mut self, lb: ServiceLoadBalancer) -> &mut Self {
        self.with_service_call("loadBalancer", move |d| {
            d.load_balancer = lb;
            Ok(())
        })
    }

    /// Add a service filter.
    pub fn service_filter(&mut self, f: FilterConfig) -> &mut Self {
        self.with_service_call("serviceFilter", move |d| {
            d.filters.push(f);
            Ok(())
        })
    }

    /// Blacklist service filter (`[name@]host:port,...`).
    pub fn blacklist_service_filter(&mut self, servers: &str) -> &mut Self {
        let s = servers.to_string();
        self.with_service_call("blacklistServiceFilter", move |d| {
            d.filters.push(FilterConfig::Blacklist(ServiceDefinition::parse_list(&s)?));
            Ok(())
        })
    }

    // --- script / poll / kamelet / variables ---

    /// Script EIP: evaluate an expression (any language) and ignore the result.
    pub fn script(&mut self, e: impl IntoExpression) -> &mut Self {
        self.add(Box::new(ScriptDefinition::new(e.into_expression())))
    }

    /// Poll EIP: replace the message with one received from `uri` (timeout in millis,
    /// -1 = wait forever, 0 = no wait).
    pub fn poll(&mut self, uri: impl Into<String>, timeout_ms: i64) -> &mut Self {
        self.add(Box::new(PollDefinition::new(uri, timeout_ms)))
    }

    /// Kamelet EIP: call the Kamelet `name` (`name?param=value`) - `to("kamelet:name")`.
    /// The `kamelet` component is provided by `cameleer-yaml`.
    pub fn kamelet(&mut self, name: impl Into<String>) -> &mut Self {
        let name = name.into();
        let uri = if name.starts_with("kamelet:") { name } else { format!("kamelet:{}", name) };
        self.add(Box::new(ToDefinition::new(uri)))
    }

    /// Remove the variables matching a pattern (`*`, `foo*`, exact name).
    pub fn remove_variables(&mut self, pattern: impl Into<String>) -> &mut Self {
        let pattern = format!("CamelVariable.{}", pattern.into());
        self.add(Box::new(RemoveDefinition { id: None, target: RemoveTarget::Properties { pattern, excludes: vec![] } }))
    }
}
