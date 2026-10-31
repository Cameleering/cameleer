//! Built-in components shipped with camel-core.
//!
//! | scheme | module |
//! |---|---|
//! | `direct` | [`direct`] synchronous in-process call |
//! | `mock` | [`mock`] testing endpoint with expectations |
//! | `log` | [`log`] logs exchanges |
//! | `timer` | [`timer`] periodic exchanges |
//! | `scheduler` | [`scheduler`] scheduled poll exchanges |
//! | `seda`, `vm`, `stub` | [`seda`] asynchronous queues |
//! | `file` | [`file`] file system consumer/producer |
//! | `bean`, `class` | [`bean`] invoke registry beans |
//! | `ref` | [`reference`] endpoints bound in the registry |
//! | `controlbus` | [`controlbus`] route management, script evaluation |
//! | `browse` | [`browse`] browsable endpoint |
//! | `exec` | [`exec`] run system commands |
//! | `stream` | [`stream`] stdin/stdout/stderr/file streams |
//! | `language` | [`language`] evaluate language scripts |
//! | `dataset`, `dataset-test`, `test` | [`dataset`] data set testing |
//!
//! Helpers: [`support`] (stoppable background threads), [`scheduled`] (scheduled poll consumer).

use std::sync::Arc;

use crate::context::CamelContext;

pub mod bean;
pub mod browse;
pub mod controlbus;
pub mod dataset;
pub mod direct;
pub mod exec;
pub mod file;
pub mod language;
pub mod log;
pub mod mock;
pub mod reference;
pub mod scheduled;
pub mod scheduler;
pub mod seda;
pub mod stream;
pub mod support;
pub mod timer;

/// Register built-in components on a new context. Components are created lazily.
pub fn register_builtin_components(ctx: &CamelContext) {
    ctx.add_component_factory("direct", || Arc::new(direct::DirectComponent::new()));
    ctx.add_component_factory("mock", || Arc::new(mock::MockComponent::new()));
    ctx.add_component_factory("log", || Arc::new(log::LogComponent));
    ctx.add_component_factory("file", || Arc::new(file::FileComponent));
    ctx.add_component_factory("timer", || Arc::new(timer::TimerComponent));
    ctx.add_component_factory("scheduler", || Arc::new(scheduler::SchedulerComponent));
    ctx.add_component_factory("seda", || Arc::new(seda::SedaComponent::new()));
    ctx.add_component_factory("vm", || Arc::new(seda::SedaComponent::vm()));
    ctx.add_component_factory("stub", || Arc::new(seda::SedaComponent::stub()));
    ctx.add_component_factory("bean", || Arc::new(bean::BeanComponent));
    ctx.add_component_factory("class", || Arc::new(bean::BeanComponent));
    ctx.add_component_factory("ref", || Arc::new(reference::RefComponent));
    ctx.add_component_factory("controlbus", || Arc::new(controlbus::ControlBusComponent));
    ctx.add_component_factory("browse", || Arc::new(browse::BrowseComponent::default()));
    ctx.add_component_factory("exec", || Arc::new(exec::ExecComponent));
    ctx.add_component_factory("stream", || Arc::new(stream::StreamComponent));
    ctx.add_component_factory("language", || Arc::new(language::LanguageComponent));
    ctx.add_component_factory("dataset", || Arc::new(dataset::DataSetComponent::default()));
    ctx.add_component_factory("dataset-test", || Arc::new(dataset::DataSetTestComponent::default()));
    ctx.add_component_factory("test", || Arc::new(dataset::DataSetTestComponent::default()));
}
