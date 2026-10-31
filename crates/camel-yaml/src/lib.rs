//! # camel-yaml
//!
//! YAML support for rust-camel:
//!
//! - [`yaml`]: a std-only YAML 1.2 parser ([`yaml::parse`], [`yaml::parse_all`]) producing
//!   [`camel_core::Value`]s, and an emitter ([`yaml::to_yaml`]).
//! - [`dsl`]: the Camel YAML DSL loader ([`load_routes`], [`add_routes_from_yaml`],
//!   [`add_routes_from_file`]) built around an extensible [`StepRegistry`].
//! - [`xml`]: a minimal XML parser and the Camel XML DSL loader, reusing the YAML step handlers.
//!
//! ```ignore
//! let ctx = CamelContext::new();
//! camel_yaml::add_routes_from_yaml(&ctx, r#"
//! - route:
//!     id: hello
//!     from:
//!       uri: direct:start
//!       steps:
//!         - setBody:
//!             simple: "Hello ${body}"
//!         - to: mock:result
//! "#)?;
//! ctx.start()?;
//! ```

pub mod dsl;
pub mod xml;
pub mod yaml;

pub use dsl::{
    add_route_from_template, add_routes_from_file, add_routes_from_yaml, build_uri, dump_route_definitions_as_yaml, dump_routes_as_yaml,
    endpoint_uri, find_expression, load_routes, load_routes_from_file, parse_expression, register_bean_factory, register_step,
    register_top_level_handler, route_definition_to_value, step_to_value, yaml_state, BeanDefinition, BeanFactory, RouteTemplateDefinition,
    StepContext, StepHandler, StepRegistry, TemplateParameter, TopLevelContext, TopLevelHandler, YamlLoader, YamlState, YAML_DSL_ERROR,
};
pub use xml::{add_routes_from_xml, load_routes_from_xml};
pub use yaml::{parse as parse_yaml, parse_all as parse_yaml_all, to_yaml, YamlError};
