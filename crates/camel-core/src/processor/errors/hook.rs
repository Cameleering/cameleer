//! Route factory hook wiring error handling, interceptors, onCompletion and route
//! policies into every route, plus builder-scoped and context-scoped configuration.
//!
//! Configuration sources (highest precedence first):
//! - route scoped: `RouteDefinition.extensions` (error handler, route policies) and
//!   `onException`/`onCompletion` blocks in the route outputs;
//! - builder scoped: definitions collected per [`RouteBuilder`] (applied to every route of
//!   that builder only);
//! - context scoped: [`CamelContextErrorExt::set_error_handler`].

use std::sync::{Arc, Mutex, RwLock};

use crate::context::{CamelContext, RouteFactoryHook};
use crate::dsl::RouteBuilder;
use crate::error::Result;
use crate::message::{props, Exchange};
use crate::model::{ChannelWrapper, ProcessorDefinition, RouteContext, RouteDefinition};
use crate::processor::basic::{ToDefinition, ToDynamicDefinition};
use crate::spi::Processor;

use super::handler::{default_error_handler, ErrorChannel, ErrorHandlerBuilder, ErrorHandlerKind, ErrorHandlerRuntime};
use super::intercept::{uri_matches, InterceptDefinition, InterceptKind, InterceptProcessor, InterceptRuntime, SendInterceptProcessor};
use super::on_completion::{OnCompletionDefinition, OnCompletionRuntime};
use super::on_exception::OnExceptionDefinition;
use super::route_policy::RoutePolicy;
use super::{matches_opt, snapshot_message, ALLOW_USE_ORIGINAL_MESSAGE, ORIGINAL_MESSAGE, TRY_ROUTE_BLOCK};

/// Route extension key: route-level [`ErrorHandlerBuilder`].
pub const ROUTE_ERROR_HANDLER: &str = "camel.errors.errorHandler";
/// Route extension key: [`BuilderScopeHandle`] of the builder that created the route.
pub const BUILDER_SCOPE: &str = "camel.errors.builderScope";
/// Route extension key: [`RoutePolicies`].
pub const ROUTE_POLICIES: &str = "camel.errors.routePolicies";
/// Builder extension key: [`BuilderErrorsExt`].
pub const BUILDER_EXT: &str = "camel.errors.builder";
/// Registry name of the context-scoped [`ContextErrorConfig`].
pub const CONTEXT_CONFIG: &str = "camel.errors.contextConfig";
const RUNTIME_ATTR: &str = "camel.errors.runtime";

/// Builder-scoped configuration shared by all routes of one builder.
#[derive(Default)]
pub struct BuilderScope {
    pub error_handler: Option<ErrorHandlerBuilder>,
    /// onException / onCompletion / intercept definitions.
    pub outputs: Vec<Box<dyn ProcessorDefinition>>,
}

/// Handle to a [`BuilderScope`], stored in each route of the builder.
#[derive(Clone)]
pub struct BuilderScopeHandle(pub Arc<Mutex<BuilderScope>>);

/// Builder extension: the shared scope plus the mini route definitions used to build
/// builder-scoped blocks with the full DSL. The blocks are moved into the shared scope
/// when the builder is dropped (i.e. after `into_definitions`, before routes are created).
pub struct BuilderErrorsExt {
    pub shared: Arc<Mutex<BuilderScope>>,
    pub pending: Vec<RouteDefinition>,
}

impl BuilderErrorsExt {
    /// Move pending blocks into the shared scope.
    pub fn flush(&mut self) {
        let mut shared = self.shared.lock().unwrap();
        for mut r in self.pending.drain(..) {
            if let Err(e) = r.finish() {
                crate::camel_warn!("RouteBuilder", "Invalid builder-scoped definition: {}", e);
            }
            shared.outputs.append(&mut r.outputs);
        }
    }
}

impl Drop for BuilderErrorsExt {
    fn drop(&mut self) {
        self.flush();
    }
}

/// Get (or create) the builder's errors extension; registers the customizer that links
/// every route of the builder to the builder scope.
pub fn builder_ext(b: &mut RouteBuilder) -> &mut BuilderErrorsExt {
    if !b.extensions.get(BUILDER_EXT).map(|e| e.is::<BuilderErrorsExt>()).unwrap_or(false) {
        let shared = Arc::new(Mutex::new(BuilderScope::default()));
        let handle = BuilderScopeHandle(shared.clone());
        b.customizers.push(Box::new(move |r: &mut RouteDefinition| {
            r.extensions.insert(BUILDER_SCOPE.to_string(), Box::new(handle.clone()));
        }));
        b.extensions.insert(BUILDER_EXT.to_string(), Box::new(BuilderErrorsExt { shared, pending: Vec::new() }));
    }
    b.extensions.get_mut(BUILDER_EXT).unwrap().downcast_mut::<BuilderErrorsExt>().unwrap()
}

/// A route policy reference (instance or registry name of an `Arc<dyn RoutePolicy>`).
#[derive(Clone)]
pub enum RoutePolicyRef {
    Policy(Arc<dyn RoutePolicy>),
    Ref(String),
}

/// Route extension holding the route policies.
#[derive(Default, Clone)]
pub struct RoutePolicies(pub Vec<RoutePolicyRef>);

/// Context-scoped error handling configuration (bound in the registry).
#[derive(Default)]
pub struct ContextErrorConfig {
    pub error_handler: RwLock<Option<ErrorHandlerBuilder>>,
}

/// Context-level error handler API.
pub trait CamelContextErrorExt {
    /// Set the default error handler for all routes (overridden by builder/route level).
    fn set_error_handler(&self, eh: ErrorHandlerBuilder);
    /// The context-level error handler, if configured.
    fn context_error_handler(&self) -> Option<ErrorHandlerBuilder>;
}

impl CamelContextErrorExt for CamelContext {
    fn set_error_handler(&self, eh: ErrorHandlerBuilder) {
        match self.registry().lookup::<ContextErrorConfig>(CONTEXT_CONFIG) {
            Some(c) => *c.error_handler.write().unwrap() = Some(eh),
            None => {
                let c = ContextErrorConfig::default();
                *c.error_handler.write().unwrap() = Some(eh);
                self.registry().bind(CONTEXT_CONFIG, c);
            }
        }
    }
    fn context_error_handler(&self) -> Option<ErrorHandlerBuilder> {
        self.registry().lookup::<ContextErrorConfig>(CONTEXT_CONFIG).and_then(|c| c.error_handler.read().unwrap().clone())
    }
}

/// Register the hook on a new context.
pub fn register(ctx: &CamelContext) {
    ctx.registry().bind(CONTEXT_CONFIG, ContextErrorConfig::default());
    ctx.add_route_factory_hook(Arc::new(ErrorsRouteHook));
}

/// Everything error/intercept related for one route.
pub(crate) struct RouteErrorsRuntime {
    eh: Option<Arc<ErrorHandlerRuntime>>,
    intercepts: Arc<Vec<InterceptRuntime>>,
    send_intercepts: Vec<InterceptRuntime>,
    from_intercepts: Vec<InterceptRuntime>,
    on_completions: Vec<Arc<OnCompletionRuntime>>,
    route_policies: Vec<Arc<dyn RoutePolicy>>,
    needs_original: bool,
}

/// The route factory hook.
pub struct ErrorsRouteHook;

impl RouteFactoryHook for ErrorsRouteHook {
    fn before_create(&self, def: &RouteDefinition, rc: &mut RouteContext) -> Result<()> {
        let handle = def.extension::<BuilderScopeHandle>(BUILDER_SCOPE).cloned();
        let guard = handle.as_ref().map(|h| h.0.lock().unwrap());
        let builder_outputs: &[Box<dyn ProcessorDefinition>] = guard.as_ref().map(|g| g.outputs.as_slice()).unwrap_or(&[]);
        let eh_builder = def
            .extension::<ErrorHandlerBuilder>(ROUTE_ERROR_HANDLER)
            .cloned()
            .or_else(|| guard.as_ref().and_then(|g| g.error_handler.clone()))
            .or_else(|| rc.context.context_error_handler())
            .unwrap_or_else(default_error_handler);

        let rc_ref: &RouteContext = rc;
        let eh = if eh_builder.kind == ErrorHandlerKind::NoErrorHandler {
            None
        } else {
            let mut rt = ErrorHandlerRuntime::new(&rc_ref.route_id, &eh_builder, &rc_ref.context)?;
            for d in &def.outputs {
                if let Some(oe) = d.as_any().downcast_ref::<OnExceptionDefinition>() {
                    let p = oe.create_policy(rc_ref, &rt.policy, true)?;
                    rt.policies.push(p);
                }
            }
            for d in builder_outputs {
                if let Some(oe) = d.as_any().downcast_ref::<OnExceptionDefinition>() {
                    let p = oe.create_policy(rc_ref, &rt.policy, false)?;
                    rt.policies.push(p);
                }
            }
            Some(Arc::new(rt))
        };

        let mut intercepts = Vec::new();
        let mut send_intercepts = Vec::new();
        let mut from_intercepts = Vec::new();
        let input_uris = {
            let mut v = vec![def.input.clone()];
            if let Ok(ep) = rc_ref.context.get_endpoint(&def.input) {
                v.push(ep.uri().to_string());
            }
            v
        };
        for d in builder_outputs.iter().chain(def.outputs.iter()) {
            if let Some(i) = d.as_any().downcast_ref::<InterceptDefinition>() {
                let r = i.create_runtime(rc_ref)?;
                match &r.kind {
                    InterceptKind::All => intercepts.push(r),
                    InterceptKind::From(p) => {
                        if uri_matches(&input_uris, p) {
                            from_intercepts.push(r)
                        }
                    }
                    InterceptKind::SendToEndpoint { .. } => send_intercepts.push(r),
                }
            }
        }

        let route_ocs: Vec<&OnCompletionDefinition> =
            def.outputs.iter().filter_map(|d| d.as_any().downcast_ref::<OnCompletionDefinition>()).collect();
        let ocs: Vec<&OnCompletionDefinition> = if route_ocs.is_empty() {
            builder_outputs.iter().filter_map(|d| d.as_any().downcast_ref::<OnCompletionDefinition>()).collect()
        } else {
            route_ocs
        };
        let mut on_completions = Vec::new();
        for oc in ocs {
            on_completions.push(Arc::new(oc.create_runtime(rc_ref)?));
        }

        let mut route_policies: Vec<Arc<dyn RoutePolicy>> = Vec::new();
        if let Some(ps) = def.extension::<RoutePolicies>(ROUTE_POLICIES) {
            for p in &ps.0 {
                match p {
                    RoutePolicyRef::Policy(p) => route_policies.push(p.clone()),
                    RoutePolicyRef::Ref(name) => {
                        let p = rc_ref
                            .context
                            .registry()
                            .lookup::<Arc<dyn RoutePolicy>>(name)
                            .map(|p| (*p).clone())
                            .ok_or_else(|| crate::error::CamelError::no_such_bean(name))?;
                        route_policies.push(p);
                    }
                }
            }
        }
        for p in &route_policies {
            p.on_init(&rc_ref.route_id);
        }
        drop(guard);

        let needs_original = eh.as_ref().map(|e| e.needs_original()).unwrap_or(false);
        if needs_original {
            rc.context.set_global_option(ALLOW_USE_ORIGINAL_MESSAGE, "true");
        }
        let rt = RouteErrorsRuntime {
            eh,
            intercepts: Arc::new(intercepts),
            send_intercepts,
            from_intercepts,
            on_completions,
            route_policies,
            needs_original,
        };
        rc.set_attribute(RUNTIME_ATTR, rt);
        let rt = rc.attribute::<RouteErrorsRuntime>(RUNTIME_ATTR).unwrap();
        rc.channel_wrappers.push(Arc::new(ErrorsChannelWrapper { rt }));
        Ok(())
    }

    fn wrap_route(&self, def: &RouteDefinition, rc: &RouteContext, processor: Arc<dyn Processor>) -> Result<Arc<dyn Processor>> {
        let Some(rt) = rc.attribute::<RouteErrorsRuntime>(RUNTIME_ATTR) else { return Ok(processor) };
        let from_uri = rc.context.get_endpoint(&def.input).map(|e| e.uri().to_string()).unwrap_or_else(|_| def.input.clone());
        Ok(Arc::new(RouteWrapper { route_id: rc.route_id.clone(), from_uri, rt, inner: processor }))
    }
}

/// Channel wrapper: interceptors + error handler around each processor.
struct ErrorsChannelWrapper {
    rt: Arc<RouteErrorsRuntime>,
}

impl ChannelWrapper for ErrorsChannelWrapper {
    fn wrap(&self, rc: &RouteContext, def: &dyn ProcessorDefinition, processor: Arc<dyn Processor>) -> Result<Arc<dyn Processor>> {
        let mut p = processor;
        if !self.rt.send_intercepts.is_empty() {
            if let Some(to) = def.as_any().downcast_ref::<ToDefinition>() {
                let mut uris = vec![to.uri.clone()];
                if let Ok(r) = rc.context.resolve_placeholders(&to.uri) {
                    uris.push(r);
                }
                if let Ok(ep) = rc.context.get_endpoint(&to.uri) {
                    uris.push(ep.uri().to_string());
                }
                let matching: Vec<InterceptRuntime> =
                    self.rt.send_intercepts.iter().filter(|i| uri_matches(&uris, i.pattern().unwrap_or("*"))).cloned().collect();
                if !matching.is_empty() {
                    p = Arc::new(SendInterceptProcessor { static_uris: vec![uris.last().unwrap().clone()], dynamic_uri: None, intercepts: matching, inner: p });
                }
            } else if let Some(td) = def.as_any().downcast_ref::<ToDynamicDefinition>() {
                p = Arc::new(SendInterceptProcessor {
                    static_uris: Vec::new(),
                    dynamic_uri: Some(td.uri.resolve_expression(&rc.context)?),
                    intercepts: self.rt.send_intercepts.clone(),
                    inner: p,
                });
            }
        }
        if !self.rt.intercepts.is_empty() {
            p = Arc::new(InterceptProcessor { intercepts: self.rt.intercepts.clone(), inner: p });
        }
        if let Some(eh) = &self.rt.eh {
            p = Arc::new(ErrorChannel { inner: p, eh: eh.clone() });
        }
        Ok(p)
    }
}

/// Wraps the whole route: route policies, interceptFrom, onCompletion registration,
/// original message capture and doTry scope isolation.
struct RouteWrapper {
    route_id: String,
    from_uri: String,
    rt: Arc<RouteErrorsRuntime>,
    inner: Arc<dyn Processor>,
}

impl Processor for RouteWrapper {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        // a called route has its own error handler scope (doTry of the caller does not apply)
        let saved_try = ex.remove_property(TRY_ROUTE_BLOCK);
        // The original message belongs to the unit of work, which spans routes connected
        // with direct/seda: only the first route of the unit of work takes the snapshot.
        let set_orig = !ex.properties.contains_key(ORIGINAL_MESSAGE)
            && (self.rt.needs_original
                || ex.context.as_ref().map(|c| c.global_option(ALLOW_USE_ORIGINAL_MESSAGE).is_some()).unwrap_or(false));
        if set_orig {
            let snap = snapshot_message(ex);
            ex.set_property(ORIGINAL_MESSAGE, snap);
        }
        for p in &self.rt.route_policies {
            p.on_exchange_begin(&self.route_id, ex);
        }
        for oc in &self.rt.on_completions {
            if !oc.before_consumer && oc.mark_registered(ex) {
                let oc = oc.clone();
                ex.add_on_completion(move |done: &Exchange| oc.run_on_copy(done));
            }
        }
        let mut proceed = true;
        for ic in &self.rt.from_intercepts {
            if !matches_opt(&ic.on_when, ex) {
                continue;
            }
            ex.set_header(props::INTERCEPTED_ENDPOINT, self.from_uri.clone());
            match ic.run(ex) {
                Ok((_, true)) => {}
                Ok((_, false)) => {
                    proceed = false;
                    break;
                }
                Err(e) => {
                    ex.set_exception(e);
                    proceed = false;
                    break;
                }
            }
        }
        if proceed {
            if let Err(e) = self.inner.process(ex) {
                ex.set_exception(e);
            }
        }
        for oc in &self.rt.on_completions {
            if oc.before_consumer && oc.mark_registered(ex) {
                oc.run_in_place(ex);
            }
        }
        for p in self.rt.route_policies.iter().rev() {
            p.on_exchange_done(&self.route_id, ex);
        }
        if set_orig {
            ex.remove_property(ORIGINAL_MESSAGE);
        }
        if let Some(v) = saved_try {
            ex.set_property(TRY_ROUTE_BLOCK, v);
        }
        Ok(())
    }
}
