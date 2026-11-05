//! The shape of the Camel YAML DSL as the loader in [`crate::dsl`] reads it: the top-level
//! elements, every step and the keys each step accepts, which keys hold expressions, steps
//! or endpoints, and which are required.
//!
//! The tables are data, not code, so tools can use them without loading routes:
//! `cameleer validate` checks route files against them (unknown keys, missing required keys,
//! steps in the wrong place) and `cameleer schema` turns them into a JSON Schema for editors.
//!
//! They describe the loader's handlers (`dsl.rs`, `dsl/eips.rs`, `dsl/eips_extra.rs`,
//! `dsl/config.rs` and cameleer-main's REST DSL), plus Apache Camel's own option names that the
//! loader accepts and ignores (for example `executorService` or `variableReceive`), so a route
//! that Apache Camel accepts is not reported. The tests at the bottom check that every
//! registered step has a model and that every key a handler reads is listed.

use crate::dsl::LANGUAGES;

/// What a key holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Any value.
    Any,
    /// A string, number or boolean (property placeholders allowed).
    Scalar,
    /// A boolean (or a placeholder).
    Bool,
    /// An integer (or a placeholder).
    Int,
    /// A number (or a placeholder).
    Number,
    /// A duration: milliseconds or a text such as `5s` (or a placeholder).
    Duration,
    /// A list of strings or a comma separated string.
    StringList,
    /// A free-form mapping (bean properties, data format options).
    Map,
    /// A list of steps.
    Steps,
    /// An expression: a scalar (Simple) or `{<language>: ...}` / `{expression: ...}`.
    Expression,
    /// A boolean or an expression (`handled`, `continued`).
    Predicate,
    /// An endpoint: a URI string or `{uri, parameters}` (checked like a `to`).
    Endpoint,
    /// An endpoint URI string (the `uri` of an endpoint step).
    Uri,
    /// Endpoint parameters (`parameters:` mapping added to the URI query).
    Parameters,
    /// A mapping of the named model.
    Object(&'static str),
    /// A list of mappings of the named model (a single mapping is accepted too); items may be
    /// wrapped in a one-key mapping named after the model (`- when: {...}`).
    List(&'static str),
    /// A list of steps, or a mapping of the named model holding `steps`
    /// (`otherwise`, `doFinally`, `onFallback`).
    Section(&'static str),
}

/// Whether a model carries an inline expression (`simple: ...`, `expression: ...`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expr {
    No,
    Optional,
    Required,
}

/// What a scalar value given instead of a mapping means (`to: mock:x`, `log: "${body}"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shorthand {
    /// A scalar is not accepted.
    No,
    /// The endpoint URI.
    Uri,
    /// The expression (Simple).
    Expression,
    /// A name, reference, message or duration (the step's main option).
    Text,
}

/// The endpoint a step sends to or consumes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    No,
    /// A static producer endpoint (`to`).
    Producer,
    /// A producer whose URI may be a Simple expression (`toD`, `wireTap`, `enrich`).
    Dynamic,
    /// A consumer (`from`).
    Consumer,
    /// A polling consumer whose URI may be a Simple expression (`pollEnrich`, `poll`).
    Poll,
}

/// One key of a model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub name: &'static str,
    pub kind: Kind,
}

/// A step or another mapping of the DSL.
#[derive(Clone, Copy, Debug)]
pub struct Model {
    pub name: &'static str,
    /// One line for schema descriptions.
    pub doc: &'static str,
    pub keys: &'static [Key],
    /// Each inner list is a set of alternatives of which at least one must be present.
    pub required: &'static [&'static [&'static str]],
    pub expression: Expr,
    /// Keys that replace a required inline expression (`aggregate`: `correlationExpression`).
    pub expression_alternatives: &'static [&'static str],
    pub shorthand: Shorthand,
    pub role: Role,
    /// Keys that are not listed are accepted (options passed through).
    pub open: bool,
    /// Exactly one key that is not listed names the variant (`marshal: {json: {...}}`).
    pub one_extra: bool,
    /// Exactly one of the listed keys (besides `id`) must be present (`errorHandler`).
    pub one_of: bool,
    /// A step: [`COMMON_STEP_KEYS`] apply.
    pub step: bool,
}

/// Keys every step accepts (the loader reads `id` and `disabled` for every step).
pub const COMMON_STEP_KEYS: &[Key] = &[
    Key { name: "id", kind: Kind::Scalar },
    Key { name: "description", kind: Kind::Scalar },
    Key { name: "disabled", kind: Kind::Bool },
    Key { name: "note", kind: Kind::Scalar },
];

const STEP: Model = Model {
    name: "",
    doc: "",
    keys: &[],
    required: &[],
    expression: Expr::No,
    expression_alternatives: &[],
    shorthand: Shorthand::No,
    role: Role::No,
    open: false,
    one_extra: false,
    one_of: false,
    step: true,
};

const MAPPING: Model = Model { step: false, ..STEP };

macro_rules! keys {
    ($($n:literal : $k:expr),* $(,)?) => { &[$(Key { name: $n, kind: $k }),*] };
}

use Kind::*;

/// Every step of the YAML DSL.
pub static STEPS: &[Model] = &[
    // --- endpoints ---
    Model {
        name: "to",
        doc: "Send the message to an endpoint",
        keys: keys! {"uri": Uri, "parameters": Parameters, "pattern": Scalar, "variableSend": Scalar, "variableReceive": Scalar},
        required: &[&["uri"]],
        shorthand: Shorthand::Uri,
        role: Role::Producer,
        ..STEP
    },
    Model {
        name: "inOnly",
        doc: "Send the message to an endpoint as InOnly",
        keys: keys! {"uri": Uri, "parameters": Parameters, "pattern": Scalar, "variableSend": Scalar, "variableReceive": Scalar},
        required: &[&["uri"]],
        shorthand: Shorthand::Uri,
        role: Role::Producer,
        ..STEP
    },
    Model {
        name: "inOut",
        doc: "Send the message to an endpoint as InOut",
        keys: keys! {"uri": Uri, "parameters": Parameters, "pattern": Scalar, "variableSend": Scalar, "variableReceive": Scalar},
        required: &[&["uri"]],
        shorthand: Shorthand::Uri,
        role: Role::Producer,
        ..STEP
    },
    Model {
        name: "toD",
        doc: "Send the message to a dynamic endpoint (the URI is a Simple expression)",
        keys: keys! {
            "uri": Uri, "parameters": Parameters, "pattern": Scalar, "ignoreInvalidEndpoint": Bool, "cacheSize": Int,
            "allowOptimisedComponents": Bool, "autoStartComponents": Bool, "variableSend": Scalar, "variableReceive": Scalar,
        },
        required: &[&["uri"]],
        shorthand: Shorthand::Uri,
        role: Role::Dynamic,
        ..STEP
    },
    Model {
        name: "wireTap",
        doc: "Send a copy of the message to an endpoint (fire and forget)",
        keys: keys! {
            "uri": Uri, "parameters": Parameters, "copy": Bool, "ignoreInvalidEndpoint": Bool, "body": Expression, "newExchangeBody": Expression,
            "newExchangeExpression": Expression, "setHeader": List("setHeader"), "setHeaders": List("setHeader"), "headers": List("setHeader"),
            "onPrepare": Scalar, "onPrepareRef": Scalar, "dynamicUri": Bool, "cacheSize": Int, "executorService": Scalar, "pattern": Scalar,
            "allowOptimisedComponents": Bool, "autoStartComponents": Bool,
        },
        required: &[&["uri"]],
        shorthand: Shorthand::Uri,
        role: Role::Dynamic,
        ..STEP
    },
    Model {
        name: "enrich",
        doc: "Enrich the message with the reply of an endpoint",
        keys: keys! {
            "uri": Uri, "parameters": Parameters, "aggregationStrategy": Scalar, "aggregationStrategyRef": Scalar, "strategyRef": Scalar,
            "aggregateOnException": Bool, "ignoreInvalidEndpoint": Bool, "shareUnitOfWork": Bool, "cacheSize": Int,
            "allowOptimisedComponents": Bool, "autoStartComponents": Bool, "aggregationStrategyMethodName": Scalar,
            "aggregationStrategyMethodAllowNull": Bool, "variableSend": Scalar, "variableReceive": Scalar,
        },
        expression: Expr::Optional,
        shorthand: Shorthand::Uri,
        role: Role::Dynamic,
        ..STEP
    },
    Model {
        name: "pollEnrich",
        doc: "Enrich the message with a message polled from an endpoint",
        keys: keys! {
            "uri": Uri, "parameters": Parameters, "timeout": Duration, "aggregationStrategy": Scalar, "aggregationStrategyRef": Scalar,
            "strategyRef": Scalar, "aggregateOnException": Bool, "ignoreInvalidEndpoint": Bool, "cacheSize": Int,
            "aggregationStrategyMethodName": Scalar, "aggregationStrategyMethodAllowNull": Bool, "autoStartComponents": Bool,
            "variableReceive": Scalar,
        },
        expression: Expr::Optional,
        shorthand: Shorthand::Uri,
        role: Role::Poll,
        ..STEP
    },
    Model {
        name: "poll",
        doc: "Replace the message with one polled from an endpoint",
        keys: keys! {"uri": Uri, "parameters": Parameters, "timeout": Duration, "variableReceive": Scalar},
        required: &[&["uri"]],
        shorthand: Shorthand::Uri,
        role: Role::Poll,
        ..STEP
    },
    Model {
        name: "kamelet",
        doc: "Call a Kamelet",
        keys: keys! {"name": Scalar, "parameters": Parameters},
        required: &[&["name"]],
        shorthand: Shorthand::Text,
        ..STEP
    },
    // --- message ---
    Model {
        name: "setBody",
        doc: "Set the message body",
        expression: Expr::Required,
        shorthand: Shorthand::Expression,
        ..STEP
    },
    Model {
        name: "transform",
        doc: "Transform the message body",
        expression: Expr::Required,
        shorthand: Shorthand::Expression,
        ..STEP
    },
    Model {
        name: "setHeader",
        doc: "Set a message header",
        keys: keys! {"name": Scalar, "headerName": Scalar},
        required: &[&["name", "headerName"]],
        expression: Expr::Required,
        ..STEP
    },
    Model {
        name: "setProperty",
        doc: "Set an exchange property",
        keys: keys! {"name": Scalar, "propertyName": Scalar},
        required: &[&["name", "propertyName"]],
        expression: Expr::Required,
        ..STEP
    },
    Model {
        name: "setVariable",
        doc: "Set a variable",
        keys: keys! {"name": Scalar, "variableName": Scalar},
        required: &[&["name", "variableName"]],
        expression: Expr::Required,
        ..STEP
    },
    Model {
        name: "setHeaders",
        doc: "Set several message headers",
        keys: keys! {"headers": List("setHeader")},
        required: &[&["headers"]],
        ..STEP
    },
    Model {
        name: "setVariables",
        doc: "Set several variables",
        keys: keys! {"variables": List("setVariable")},
        required: &[&["variables"]],
        ..STEP
    },
    Model {
        name: "removeHeader",
        doc: "Remove a message header",
        keys: keys! {"name": Scalar, "headerName": Scalar},
        required: &[&["name", "headerName"]],
        shorthand: Shorthand::Text,
        ..STEP
    },
    Model {
        name: "removeHeaders",
        doc: "Remove the message headers matching a pattern",
        keys: keys! {"pattern": Scalar, "excludePattern": StringList},
        required: &[&["pattern"]],
        shorthand: Shorthand::Text,
        ..STEP
    },
    Model {
        name: "removeProperty",
        doc: "Remove an exchange property",
        keys: keys! {"name": Scalar, "propertyName": Scalar},
        required: &[&["name", "propertyName"]],
        shorthand: Shorthand::Text,
        ..STEP
    },
    Model {
        name: "removeProperties",
        doc: "Remove the exchange properties matching a pattern",
        keys: keys! {"pattern": Scalar, "excludePattern": StringList},
        required: &[&["pattern"]],
        shorthand: Shorthand::Text,
        ..STEP
    },
    Model {
        name: "removeVariable",
        doc: "Remove a variable",
        keys: keys! {"name": Scalar, "variableName": Scalar},
        required: &[&["name", "variableName"]],
        shorthand: Shorthand::Text,
        ..STEP
    },
    Model {
        name: "convertBodyTo",
        doc: "Convert the message body to a type",
        keys: keys! {"type": Scalar, "mandatory": Bool, "charset": Scalar},
        required: &[&["type"]],
        shorthand: Shorthand::Text,
        ..STEP
    },
    Model {
        name: "convertHeaderTo",
        doc: "Convert a message header to a type",
        keys: keys! {"name": Scalar, "headerName": Scalar, "type": Scalar, "mandatory": Bool, "charset": Scalar, "toName": Scalar},
        required: &[&["name", "headerName"], &["type"]],
        ..STEP
    },
    Model {
        name: "log",
        doc: "Log a message (a Simple expression)",
        keys: keys! {"message": Scalar, "loggingLevel": Scalar, "logName": Scalar, "marker": Scalar, "logger": Scalar},
        required: &[&["message"]],
        shorthand: Shorthand::Text,
        ..STEP
    },
    Model {
        name: "process",
        doc: "Call a processor bean",
        keys: keys! {"ref": Scalar},
        required: &[&["ref"]],
        shorthand: Shorthand::Text,
        ..STEP
    },
    Model { name: "stop", doc: "Stop routing the message", ..STEP },
    Model {
        name: "setExchangePattern",
        doc: "Set the exchange pattern (InOnly, InOut)",
        keys: keys! {"pattern": Scalar},
        required: &[&["pattern"]],
        shorthand: Shorthand::Text,
        ..STEP
    },
    Model { name: "step", doc: "Group steps", keys: keys! {"steps": Steps}, ..STEP },
    Model {
        name: "bean",
        doc: "Call a bean method",
        keys: keys! {"ref": Scalar, "beanType": Scalar, "type": Scalar, "method": Scalar, "scope": Scalar},
        required: &[&["ref", "beanType", "type"]],
        shorthand: Shorthand::Text,
        ..STEP
    },
    Model {
        name: "marshal",
        doc: "Marshal the body with a data format (`json: {}`, `csv: {delimiter: ';'}`, ...)",
        keys: keys! {"variableSend": Scalar, "variableReceive": Scalar, "allowNullBody": Bool},
        shorthand: Shorthand::Text,
        one_extra: true,
        ..STEP
    },
    Model {
        name: "unmarshal",
        doc: "Unmarshal the body with a data format (`json: {}`, `csv: {delimiter: ';'}`, ...)",
        keys: keys! {"variableSend": Scalar, "variableReceive": Scalar, "allowNullBody": Bool},
        shorthand: Shorthand::Text,
        one_extra: true,
        ..STEP
    },
    Model {
        name: "script",
        doc: "Run a script (the expression) without changing the message body",
        expression: Expr::Required,
        shorthand: Shorthand::Expression,
        ..STEP
    },
    Model {
        name: "transformDataType",
        doc: "Transform the message to a data type",
        keys: keys! {"toType": Scalar, "fromType": Scalar},
        required: &[&["toType"]],
        ..STEP
    },
    // --- routing ---
    Model {
        name: "filter",
        doc: "Only route messages matching the predicate through the nested steps",
        keys: keys! {"steps": Steps, "statusPropertyName": Scalar},
        expression: Expr::Required,
        ..STEP
    },
    Model {
        name: "choice",
        doc: "Content based router",
        keys: keys! {"when": List("when"), "otherwise": Section("otherwise"), "precondition": Bool},
        required: &[&["when"]],
        ..STEP
    },
    Model { name: "pipeline", doc: "Run the nested steps in sequence", keys: keys! {"steps": Steps}, ..STEP },
    Model {
        name: "split",
        doc: "Split the message and route each part through the nested steps",
        keys: keys! {
            "steps": Steps, "delimiter": Scalar, "parallelProcessing": Bool, "streaming": Bool, "stopOnException": Bool,
            "shareUnitOfWork": Bool, "timeout": Duration, "aggregationStrategy": Scalar, "aggregationStrategyRef": Scalar, "strategyRef": Scalar,
            "parallelAggregate": Bool, "synchronous": Bool, "executorService": Scalar, "onPrepare": Scalar,
            "aggregationStrategyMethodName": Scalar, "aggregationStrategyMethodAllowNull": Bool,
        },
        expression: Expr::Required,
        ..STEP
    },
    Model {
        name: "multicast",
        doc: "Send the message to every nested step",
        keys: keys! {
            "steps": Steps, "parallelProcessing": Bool, "streaming": Bool, "stopOnException": Bool, "shareUnitOfWork": Bool,
            "timeout": Duration, "aggregationStrategy": Scalar, "aggregationStrategyRef": Scalar, "strategyRef": Scalar,
            "parallelAggregate": Bool, "synchronous": Bool, "executorService": Scalar, "onPrepare": Scalar,
            "aggregationStrategyMethodName": Scalar, "aggregationStrategyMethodAllowNull": Bool,
        },
        ..STEP
    },
    Model {
        name: "recipientList",
        doc: "Send the message to the endpoints computed by the expression",
        keys: keys! {
            "delimiter": Scalar, "ignoreInvalidEndpoints": Bool, "parallelProcessing": Bool, "streaming": Bool, "stopOnException": Bool,
            "shareUnitOfWork": Bool, "timeout": Duration, "aggregationStrategy": Scalar, "aggregationStrategyRef": Scalar,
            "strategyRef": Scalar, "parallelAggregate": Bool, "synchronous": Bool, "executorService": Scalar, "onPrepare": Scalar,
            "cacheSize": Int, "aggregationStrategyMethodName": Scalar, "aggregationStrategyMethodAllowNull": Bool,
        },
        expression: Expr::Required,
        ..STEP
    },
    Model {
        name: "routingSlip",
        doc: "Route the message through the endpoints computed by the expression, in order",
        keys: keys! {"uriDelimiter": Scalar, "ignoreInvalidEndpoints": Bool, "cacheSize": Int},
        expression: Expr::Required,
        ..STEP
    },
    Model {
        name: "dynamicRouter",
        doc: "Route the message while the expression returns endpoints",
        keys: keys! {"uriDelimiter": Scalar, "ignoreInvalidEndpoints": Bool, "cacheSize": Int},
        expression: Expr::Required,
        ..STEP
    },
    Model {
        name: "aggregate",
        doc: "Aggregate messages with the same correlation key",
        keys: keys! {
            "steps": Steps, "correlationExpression": Expression, "aggregationStrategy": Scalar, "aggregationStrategyRef": Scalar,
            "strategyRef": Scalar, "completionSize": Int, "completionSizeExpression": Expression, "completionPredicate": Expression,
            "completionTimeout": Duration, "completionTimeoutExpression": Expression, "completionInterval": Duration,
            "completionFromBatchConsumer": Bool, "completionOnNewCorrelationGroup": Bool, "eagerCheckCompletion": Bool,
            "forceCompletionOnStop": Bool, "discardOnCompletionTimeout": Bool, "ignoreInvalidCorrelationKeys": Bool,
            "closeCorrelationKeyOnCompletion": Int, "aggregationRepository": Scalar, "aggregationRepositoryRef": Scalar,
            "parallelProcessing": Bool, "optimisticLocking": Bool, "executorService": Scalar, "timeoutCheckerExecutorService": Scalar,
            "aggregateController": Scalar, "completeAllOnStop": Bool, "discardOnAggregationFailure": Bool,
            "aggregationStrategyMethodName": Scalar, "aggregationStrategyMethodAllowNull": Bool, "completionTimeoutCheckerInterval": Duration,
        },
        required: &[&["aggregationStrategy", "aggregationStrategyRef", "strategyRef"]],
        expression: Expr::Required,
        expression_alternatives: &["correlationExpression"],
        ..STEP
    },
    Model {
        name: "loop",
        doc: "Run the nested steps a number of times (or while a predicate holds)",
        keys: keys! {"steps": Steps, "doWhile": Bool, "copy": Bool, "breakOnShutdown": Bool, "onPrepare": Scalar},
        expression: Expr::Required,
        ..STEP
    },
    Model {
        name: "loadBalance",
        doc: "Balance the messages over the nested steps",
        keys: keys! {
            "steps": Steps, "inheritErrorHandler": Bool, "roundRobin": Any, "roundRobinLoadBalancer": Any, "random": Any,
            "randomLoadBalancer": Any, "topic": Any, "topicLoadBalancer": Any, "sticky": Object("sticky"),
            "stickyLoadBalancer": Object("sticky"), "weighted": Object("weighted"), "weightedLoadBalancer": Object("weighted"),
            "failover": Object("failover"), "failoverLoadBalancer": Object("failover"), "custom": Object("customLoadBalancer"),
            "customLoadBalancer": Object("customLoadBalancer"),
        },
        ..STEP
    },
    Model {
        name: "resequence",
        doc: "Put messages back in order",
        keys: keys! {
            "steps": Steps, "batchConfig": Object("batchConfig"), "batch-config": Object("batchConfig"),
            "streamConfig": Object("streamConfig"), "stream-config": Object("streamConfig"),
        },
        expression: Expr::Required,
        ..STEP
    },
    Model {
        name: "sort",
        doc: "Sort the message body",
        keys: keys! {"comparator": Scalar, "comparatorRef": Scalar},
        expression: Expr::Required,
        shorthand: Shorthand::Expression,
        ..STEP
    },
    Model {
        name: "claimCheck",
        doc: "Store or retrieve message data (Set, Get, GetAndRemove, Push, Pop)",
        keys: keys! {"operation": Scalar, "key": Scalar, "filter": Scalar, "aggregationStrategy": Scalar, "aggregationStrategyMethodName": Scalar},
        required: &[&["operation"]],
        ..STEP
    },
    Model {
        name: "sample",
        doc: "Let one message through per period (or every N messages)",
        keys: keys! {"samplePeriod": Duration, "messageFrequency": Int},
        shorthand: Shorthand::Text,
        ..STEP
    },
    Model {
        name: "throttle",
        doc: "Limit the number of messages per period",
        keys: keys! {
            "maximumRequests": Scalar, "timePeriodMillis": Duration, "rejectExecution": Bool, "asyncDelayed": Bool,
            "correlationExpression": Expression, "mode": Scalar, "callerRunsWhenRejected": Bool, "executorService": Scalar,
        },
        expression: Expr::Required,
        expression_alternatives: &["maximumRequests"],
        shorthand: Shorthand::Expression,
        ..STEP
    },
    Model {
        name: "delay",
        doc: "Delay the message (milliseconds, an expression)",
        keys: keys! {"asyncDelayed": Bool, "callerRunsWhenRejected": Bool, "executorService": Scalar},
        expression: Expr::Required,
        shorthand: Shorthand::Expression,
        ..STEP
    },
    Model {
        name: "idempotentConsumer",
        doc: "Filter out duplicate messages",
        keys: keys! {
            "steps": Steps, "idempotentRepository": Scalar, "idempotentRepositoryRef": Scalar, "eager": Bool, "skipDuplicate": Bool,
            "removeOnFailure": Bool, "completionEager": Bool,
        },
        expression: Expr::Required,
        ..STEP
    },
    Model {
        name: "validate",
        doc: "Fail with a PredicateValidationException when the predicate does not hold",
        keys: keys! {"predicateExceptionFactory": Scalar},
        expression: Expr::Required,
        shorthand: Shorthand::Expression,
        ..STEP
    },
    Model {
        name: "serviceCall",
        doc: "Call a remote service",
        keys: keys! {
            "name": Scalar, "uri": Scalar, "component": Scalar, "pattern": Scalar, "expression": Expression,
            "staticServiceDiscovery": Any, "combinedServiceDiscovery": Any, "serviceDiscoveryRef": Scalar,
            "blacklistServiceFilter": Any, "healthyServiceFilter": Any, "passThroughServiceFilter": Any, "randomLoadBalancer": Any,
            "roundRobinLoadBalancer": Any, "defaultLoadBalancer": Any, "configurationRef": Scalar, "serviceFilterRef": Scalar,
            "loadBalancerRef": Scalar, "serviceChooserRef": Scalar, "expressionRef": Scalar,
        },
        required: &[&["name"]],
        shorthand: Shorthand::Text,
        ..STEP
    },
    // --- error handling ---
    Model {
        name: "doTry",
        doc: "Try the nested steps, with doCatch and doFinally",
        keys: keys! {"steps": Steps, "doCatch": List("doCatch"), "doFinally": Section("doFinally")},
        ..STEP
    },
    Model {
        name: "onException",
        doc: "Handle exceptions of the given kinds",
        keys: keys! {
            "exception": StringList, "handled": Predicate, "continued": Predicate, "onWhen": Expression, "retryWhile": Expression,
            "redeliveryPolicy": Object("redeliveryPolicy"), "redeliveryPolicyRef": Scalar, "useOriginalMessage": Bool,
            "useOriginalBody": Bool, "onRedeliveryRef": Scalar, "onRedelivery": Scalar, "onExceptionOccurredRef": Scalar,
            "onExceptionOccurred": Scalar, "steps": Steps,
        },
        ..STEP
    },
    Model {
        name: "onCompletion",
        doc: "Run the nested steps when the exchange completes",
        keys: keys! {
            "onCompleteOnly": Bool, "onFailureOnly": Bool, "mode": Scalar, "onWhen": Expression, "steps": Steps,
            "parallelProcessing": Bool, "executorService": Scalar, "useOriginalMessage": Bool,
        },
        ..STEP
    },
    Model {
        name: "intercept",
        doc: "Intercept every step",
        keys: keys! {"onWhen": Expression, "steps": Steps},
        ..STEP
    },
    Model {
        name: "interceptFrom",
        doc: "Intercept messages arriving at consumers matching the URI pattern",
        keys: keys! {"uri": Scalar, "onWhen": Expression, "steps": Steps},
        shorthand: Shorthand::Text,
        ..STEP
    },
    Model {
        name: "interceptSendToEndpoint",
        doc: "Intercept messages sent to endpoints matching the URI pattern",
        keys: keys! {"uri": Scalar, "skipSendToOriginalEndpoint": Bool, "onWhen": Expression, "afterUri": Scalar, "steps": Steps},
        required: &[&["uri"]],
        shorthand: Shorthand::Text,
        ..STEP
    },
    Model {
        name: "circuitBreaker",
        doc: "Circuit breaker around the nested steps, with onFallback",
        keys: keys! {
            "steps": Steps, "onFallback": Section("onFallback"), "configuration": Scalar, "configurationRef": Scalar,
            "resilience4jConfiguration": Object("resilience4jConfiguration"), "faultToleranceConfiguration": Object("faultToleranceConfiguration"),
        },
        ..STEP
    },
    Model {
        name: "rollback",
        doc: "Roll back the transaction",
        keys: keys! {"markRollbackOnlyLast": Bool, "markRollbackOnly": Bool, "message": Scalar},
        shorthand: Shorthand::Text,
        ..STEP
    },
    Model {
        name: "throwException",
        doc: "Throw an exception",
        keys: keys! {"exceptionType": Scalar, "message": Scalar, "ref": Scalar},
        required: &[&["exceptionType"]],
        ..STEP
    },
    Model {
        name: "saga",
        doc: "Saga (long running action) around the nested steps",
        keys: keys! {
            "propagation": Scalar, "completionMode": Scalar, "timeout": Duration, "compensation": Endpoint, "completion": Endpoint,
            "sagaService": Scalar, "sagaServiceRef": Scalar, "option": List("option"), "steps": Steps,
        },
        ..STEP
    },
    // --- threads and transactions ---
    Model {
        name: "threads",
        doc: "Continue routing on a thread pool",
        keys: keys! {
            "poolSize": Int, "maxPoolSize": Int, "keepAliveTime": Duration, "timeUnit": Scalar, "maxQueueSize": Int,
            "allowCoreThreadTimeOut": Bool, "rejectedPolicy": Scalar, "callerRunsWhenRejected": Bool, "threadName": Scalar,
            "executorService": Scalar, "executorServiceRef": Scalar, "steps": Steps,
        },
        ..STEP
    },
    Model {
        name: "transacted",
        doc: "Run the rest of the route in a transaction",
        keys: keys! {"ref": Scalar, "steps": Steps},
        shorthand: Shorthand::Text,
        ..STEP
    },
    Model {
        name: "policy",
        doc: "Wrap the rest of the route in a policy bean",
        keys: keys! {"ref": Scalar, "steps": Steps},
        required: &[&["ref"]],
        shorthand: Shorthand::Text,
        ..STEP
    },
];

/// Mappings nested in steps and top-level elements.
pub static MODELS: &[Model] = &[
    Model {
        name: "when",
        doc: "A branch of a choice",
        keys: keys! {"id": Scalar, "description": Scalar, "note": Scalar, "steps": Steps},
        expression: Expr::Required,
        ..MAPPING
    },
    Model { name: "otherwise", doc: "The default branch of a choice", keys: keys! {"id": Scalar, "description": Scalar, "note": Scalar, "steps": Steps}, ..MAPPING },
    Model {
        name: "doCatch",
        doc: "Catch exceptions of the given kinds",
        keys: keys! {"id": Scalar, "description": Scalar, "note": Scalar, "exception": StringList, "onWhen": Expression, "steps": Steps},
        ..MAPPING
    },
    Model { name: "doFinally", doc: "Steps that always run", keys: keys! {"id": Scalar, "description": Scalar, "note": Scalar, "steps": Steps}, ..MAPPING },
    Model {
        name: "onFallback",
        doc: "Steps that run when the circuit breaker is open or the call fails",
        keys: keys! {"id": Scalar, "description": Scalar, "note": Scalar, "steps": Steps, "fallbackViaNetwork": Bool},
        ..MAPPING
    },
    Model {
        name: "option",
        doc: "A saga option",
        keys: keys! {"key": Scalar, "name": Scalar},
        required: &[&["key", "name"]],
        expression: Expr::Required,
        ..MAPPING
    },
    Model {
        name: "sticky",
        doc: "Sticky load balancer",
        keys: keys! {"id": Scalar, "correlationExpression": Expression},
        expression: Expr::Required,
        expression_alternatives: &["correlationExpression"],
        ..MAPPING
    },
    Model {
        name: "weighted",
        doc: "Weighted load balancer",
        keys: keys! {"id": Scalar, "distributionRatio": Any, "distributionRatioDelimiter": Scalar, "roundRobin": Bool},
        required: &[&["distributionRatio"]],
        ..MAPPING
    },
    Model {
        name: "failover",
        doc: "Failover load balancer",
        keys: keys! {"id": Scalar, "exception": StringList, "maximumFailoverAttempts": Int, "roundRobin": Bool, "sticky": Bool, "inheritErrorHandler": Bool},
        ..MAPPING
    },
    Model {
        name: "customLoadBalancer",
        doc: "A load balancer bean",
        keys: keys! {"id": Scalar, "ref": Scalar},
        required: &[&["ref"]],
        shorthand: Shorthand::Text,
        ..MAPPING
    },
    Model {
        name: "batchConfig",
        doc: "Batch resequencer",
        keys: keys! {"batchSize": Int, "batchTimeout": Duration, "allowDuplicates": Bool, "reverse": Bool, "ignoreInvalidExchanges": Bool},
        ..MAPPING
    },
    Model {
        name: "streamConfig",
        doc: "Stream resequencer",
        keys: keys! {"capacity": Int, "timeout": Duration, "rejectOld": Bool, "ignoreInvalidExchanges": Bool, "deliveryAttemptInterval": Duration, "comparator": Scalar},
        ..MAPPING
    },
    Model {
        name: "redeliveryPolicy",
        doc: "Redelivery options",
        keys: keys! {
            "id": Scalar, "maximumRedeliveries": Int, "redeliveryDelay": Duration, "asyncDelayedRedelivery": Bool, "backOffMultiplier": Number,
            "useExponentialBackOff": Bool, "collisionAvoidanceFactor": Number, "useCollisionAvoidance": Bool,
            "maximumRedeliveryDelay": Duration, "retriesExhaustedLogLevel": Scalar, "retryAttemptedLogLevel": Scalar,
            "retryAttemptedLogInterval": Int, "logRetryAttempted": Bool, "logStackTrace": Bool, "logRetryStackTrace": Bool,
            "logHandled": Bool, "logNewException": Bool, "logContinued": Bool, "logExhausted": Bool, "logExhaustedMessageHistory": Bool,
            "logExhaustedMessageBody": Bool, "disableRedelivery": Bool, "delayPattern": Scalar, "allowRedeliveryWhileStopping": Bool,
            "exchangeFormatterRef": Scalar,
        },
        ..MAPPING
    },
    Model {
        name: "resilience4jConfiguration",
        doc: "Resilience4j circuit breaker options",
        keys: keys! {
            "id": Scalar, "failureRateThreshold": Number, "minimumNumberOfCalls": Int, "slidingWindowSize": Int,
            "waitDurationInOpenState": Duration, "permittedNumberOfCallsInHalfOpenState": Int, "timeoutEnabled": Bool,
            "timeoutDuration": Duration, "slidingWindowType": Scalar, "slowCallRateThreshold": Number, "slowCallDurationThreshold": Duration,
            "automaticTransitionFromOpenToHalfOpenEnabled": Bool, "writableStackTraceEnabled": Bool, "bulkheadEnabled": Bool,
            "bulkheadMaxConcurrentCalls": Int, "bulkheadMaxWaitDuration": Duration, "timeoutExecutorService": Scalar,
            "timeoutCancelRunningFuture": Bool, "circuitBreaker": Scalar, "config": Scalar, "throwExceptionWhenHalfOpenOrOpenState": Bool,
            "recordException": StringList, "ignoreException": StringList,
        },
        ..MAPPING
    },
    Model {
        name: "faultToleranceConfiguration",
        doc: "MicroProfile Fault Tolerance circuit breaker options",
        keys: keys! {
            "id": Scalar, "requestVolumeThreshold": Int, "failureRatio": Number, "delay": Duration, "successThreshold": Int,
            "timeoutEnabled": Bool, "timeoutDuration": Duration, "timeoutPoolSize": Int, "bulkheadEnabled": Bool,
            "bulkheadMaxConcurrentCalls": Int, "bulkheadWaitingTaskQueue": Int, "circuitBreaker": Scalar, "typedGuard": Scalar,
        },
        ..MAPPING
    },
    // --- error handlers ---
    Model {
        name: "errorHandler",
        doc: "An error handler: deadLetterChannel, defaultErrorHandler, noErrorHandler or refErrorHandler",
        keys: keys! {
            "id": Scalar, "deadLetterChannel": Object("deadLetterChannel"), "defaultErrorHandler": Object("defaultErrorHandler"),
            "jtaTransactionErrorHandler": Object("defaultErrorHandler"), "springTransactionErrorHandler": Object("defaultErrorHandler"),
            "noErrorHandler": Any, "refErrorHandler": Object("refErrorHandler"),
        },
        shorthand: Shorthand::Text,
        one_of: true,
        ..MAPPING
    },
    Model {
        name: "deadLetterChannel",
        doc: "Move failed messages to a dead letter endpoint",
        keys: keys! {
            "deadLetterUri": Endpoint, "id": Scalar, "level": Scalar, "logName": Scalar, "loggerRef": Scalar, "executorServiceRef": Scalar,
            "redeliveryPolicy": Object("redeliveryPolicy"), "redeliveryPolicyRef": Scalar, "useOriginalMessage": Bool,
            "useOriginalBody": Bool, "deadLetterHandleNewException": Bool, "onRedeliveryRef": Scalar, "onRedelivery": Scalar,
            "onExceptionOccurredRef": Scalar, "onExceptionOccurred": Scalar, "onPrepareFailureRef": Scalar, "onPrepareFailure": Scalar,
            "retryWhile": Expression,
        },
        required: &[&["deadLetterUri"]],
        ..MAPPING
    },
    Model {
        name: "defaultErrorHandler",
        doc: "Camel's default error handler",
        keys: keys! {
            "id": Scalar, "level": Scalar, "logName": Scalar, "loggerRef": Scalar, "executorServiceRef": Scalar,
            "transactionTemplateRef": Scalar, "transactionManagerRef": Scalar, "rollbackLoggingLevel": Scalar,
            "redeliveryPolicy": Object("redeliveryPolicy"), "redeliveryPolicyRef": Scalar, "useOriginalMessage": Bool,
            "useOriginalBody": Bool, "onRedeliveryRef": Scalar, "onRedelivery": Scalar, "onExceptionOccurredRef": Scalar,
            "onExceptionOccurred": Scalar, "onPrepareFailureRef": Scalar, "onPrepareFailure": Scalar, "retryWhile": Expression,
        },
        ..MAPPING
    },
    Model {
        name: "refErrorHandler",
        doc: "An error handler bean",
        keys: keys! {"id": Scalar, "ref": Scalar},
        required: &[&["ref"]],
        shorthand: Shorthand::Text,
        ..MAPPING
    },
    // --- routes ---
    Model {
        name: "route",
        doc: "A route",
        keys: keys! {
            "id": Scalar, "routeId": Scalar, "description": Scalar, "note": Scalar, "group": Scalar, "autoStartup": Bool, "startupOrder": Int,
            "routeConfigurationId": Scalar, "errorHandler": Object("errorHandler"), "errorHandlerRef": Scalar, "routePolicy": StringList,
            "routePolicyRef": StringList, "from": Object("from"), "steps": Steps, "inputType": Any, "outputType": Any,
            "precondition": Scalar, "nodePrefixId": Scalar, "logMask": Bool, "messageHistory": Bool, "streamCache": Bool,
            "streamCaching": Bool, "trace": Bool, "tracing": Bool, "shutdownRoute": Scalar, "shutdownRunningTask": Scalar, "delayer": Duration,
            "kamelet": Bool,
        },
        required: &[&["from"]],
        ..MAPPING
    },
    Model {
        name: "from",
        doc: "The consumer endpoint of a route and its steps",
        keys: keys! {"uri": Uri, "parameters": Parameters, "steps": Steps, "id": Scalar, "description": Scalar, "note": Scalar, "variableReceive": Scalar},
        required: &[&["uri"]],
        shorthand: Shorthand::Uri,
        role: Role::Consumer,
        ..MAPPING
    },
    Model {
        name: "routeTemplate",
        doc: "A route template with parameters",
        keys: keys! {
            "id": Scalar, "description": Scalar, "note": Scalar, "parameters": List("templateParameter"), "route": Object("route"),
            "from": Object("from"), "beans": List("bean"),
        },
        required: &[&["id"], &["route", "from"]],
        ..MAPPING
    },
    Model {
        name: "templateParameter",
        doc: "A route template parameter",
        keys: keys! {"name": Scalar, "defaultValue": Scalar, "required": Bool, "description": Scalar, "allowedValues": Any},
        required: &[&["name"]],
        ..MAPPING
    },
    Model {
        name: "templatedRoute",
        doc: "A route created from a route template",
        keys: keys! {"routeTemplateRef": Scalar, "routeId": Scalar, "prefixId": Scalar, "parameters": Any, "beans": List("bean")},
        required: &[&["routeTemplateRef"]],
        ..MAPPING
    },
    Model {
        name: "bean",
        doc: "A bean (created by a registered bean factory for its type)",
        keys: keys! {
            "name": Scalar, "type": Scalar, "properties": Map, "constructors": Any, "builderClass": Scalar, "builderMethod": Scalar,
            "factoryMethod": Scalar, "factoryBean": Scalar, "initMethod": Scalar, "destroyMethod": Scalar, "scriptLanguage": Scalar,
            "script": Scalar,
        },
        required: &[&["name"]],
        ..MAPPING
    },
    Model {
        name: "routeConfiguration",
        doc: "Error handling and interceptors shared by routes",
        keys: keys! {
            "id": Scalar, "description": Scalar, "precondition": Scalar, "errorHandler": Object("errorHandler"),
            "onException": List("onException"), "onCompletion": List("onCompletion"), "intercept": List("intercept"),
            "interceptFrom": List("interceptFrom"), "interceptSendToEndpoint": List("interceptSendToEndpoint"),
        },
        ..MAPPING
    },
    // --- REST DSL ---
    Model {
        name: "restConfiguration",
        doc: "REST DSL configuration",
        keys: keys! {
            "component": Scalar, "apiComponent": Scalar, "producerComponent": Scalar, "scheme": Scalar, "host": Scalar, "port": Scalar,
            "apiHost": Scalar, "useXForwardHeaders": Bool, "producerApiDoc": Scalar, "contextPath": Scalar, "apiContextPath": Scalar,
            "apiContextRouteId": Scalar, "apiVendorExtension": Bool, "hostNameResolver": Scalar, "bindingMode": Scalar,
            "bindingPackageScan": Scalar, "skipBindingOnErrorCode": Bool, "clientRequestValidation": Bool,
            "clientResponseValidation": Bool, "enableCORS": Bool, "enableCors": Bool, "enableNoContentResponse": Bool,
            "inlineRoutes": Bool, "jsonDataFormat": Scalar, "xmlDataFormat": Scalar, "componentProperty": Any, "endpointProperty": Any,
            "endpointProperties": Any, "consumerProperty": Any, "consumerProperties": Any, "dataFormatProperty": Any,
            "apiProperty": Any, "apiProperties": Any, "corsHeaders": Any, "apiDocs": Bool, "validationLevels": Any,
        },
        ..MAPPING
    },
    Model {
        name: "rest",
        doc: "REST services",
        keys: keys! {
            "id": Scalar, "path": Scalar, "description": Scalar, "consumes": Scalar, "produces": Scalar, "tag": Scalar, "bindingMode": Scalar,
            "get": List("verb"), "post": List("verb"), "put": List("verb"), "delete": List("verb"), "patch": List("verb"),
            "head": List("verb"), "options": List("verb"), "verb": List("verb"), "apiDocs": Bool, "disabled": Bool, "enableCORS": Bool,
            "enableNoContentResponse": Bool, "skipBindingOnErrorCode": Bool, "clientRequestValidation": Bool,
            "clientResponseValidation": Bool, "securityDefinitions": Any, "securityRequirements": Any, "openApi": Any,
        },
        ..MAPPING
    },
    Model {
        name: "verb",
        doc: "A REST operation",
        keys: keys! {
            "method": Scalar, "path": Scalar, "uri": Scalar, "to": Endpoint, "toD": Any, "steps": Any, "id": Scalar, "routeId": Scalar,
            "description": Scalar, "consumes": Scalar, "produces": Scalar, "bindingMode": Scalar, "param": List("param"),
            "responseMessage": List("responseMessage"), "type": Scalar, "outType": Scalar, "apiDocs": Bool, "deprecated": Bool,
            "disabled": Bool, "streamCache": Bool, "skipBindingOnErrorCode": Bool, "clientRequestValidation": Bool,
            "clientResponseValidation": Bool, "enableCORS": Bool, "enableNoContentResponse": Bool, "security": Any,
        },
        ..MAPPING
    },
    Model {
        name: "param",
        doc: "A REST parameter",
        keys: keys! {
            "name": Scalar, "type": Scalar, "required": Bool, "description": Scalar, "dataType": Scalar, "defaultValue": Scalar,
            "allowableValues": Any, "collectionFormat": Scalar, "arrayType": Scalar, "dataFormat": Scalar, "examples": Any,
        },
        required: &[&["name"]],
        ..MAPPING
    },
    Model {
        name: "responseMessage",
        doc: "A REST response",
        keys: keys! {"code": Scalar, "message": Scalar, "responseModel": Scalar, "header": Any, "examples": Any, "contentType": Scalar},
        ..MAPPING
    },
];

/// Top-level elements of a route file and what they hold.
pub static TOP_LEVEL: &[Key] = &[
    Key { name: "route", kind: Object("route") },
    Key { name: "from", kind: Object("from") },
    Key { name: "beans", kind: List("bean") },
    Key { name: "routeTemplate", kind: Object("routeTemplate") },
    Key { name: "routeTemplates", kind: List("routeTemplate") },
    Key { name: "templatedRoute", kind: Object("templatedRoute") },
    Key { name: "templatedRoutes", kind: List("templatedRoute") },
    Key { name: "routeConfiguration", kind: Object("routeConfiguration") },
    Key { name: "errorHandler", kind: Object("errorHandler") },
    Key { name: "restConfiguration", kind: Object("restConfiguration") },
    Key { name: "rest", kind: Object("rest") },
    Key { name: "dataFormats", kind: Any },
    Key { name: "onException", kind: Object("onException") },
    Key { name: "onCompletion", kind: Object("onCompletion") },
    Key { name: "intercept", kind: Object("intercept") },
    Key { name: "interceptFrom", kind: Object("interceptFrom") },
    Key { name: "interceptSendToEndpoint", kind: Object("interceptSendToEndpoint") },
];

/// Elements that are only valid inside another one: (element, parent, parent key).
pub const NESTED_ONLY: &[(&str, &str)] = &[
    ("when", "choice"),
    ("otherwise", "choice"),
    ("doCatch", "doTry"),
    ("doFinally", "doTry"),
    ("onFallback", "circuitBreaker"),
    ("deadLetterChannel", "errorHandler"),
    ("defaultErrorHandler", "errorHandler"),
    ("noErrorHandler", "errorHandler"),
    ("refErrorHandler", "errorHandler"),
    ("redeliveryPolicy", "onException"),
    ("beans", "a top-level element"),
    ("route", "a top-level element"),
    ("routeTemplate", "a top-level element"),
    ("templatedRoute", "a top-level element"),
    ("routeConfiguration", "a top-level element"),
    ("rest", "a top-level element"),
    ("restConfiguration", "a top-level element"),
];

/// The expression languages recognised as keys (`simple: ...`).
pub fn languages() -> &'static [&'static str] {
    LANGUAGES
}

/// The model of a step.
pub fn step(name: &str) -> Option<&'static Model> {
    STEPS.iter().find(|m| m.name == name)
}

/// A model by name: nested mappings first, then steps (a `setHeaders` item is a `setHeader`).
pub fn model(name: &str) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.name == name).or_else(|| step(name))
}

/// The key of a model (including [`COMMON_STEP_KEYS`] for steps).
pub fn key(model: &Model, name: &str) -> Option<Key> {
    model.keys.iter().find(|k| k.name == name).copied().or_else(|| if model.step { COMMON_STEP_KEYS.iter().find(|k| k.name == name).copied() } else { None })
}

/// All key names of a model (including [`COMMON_STEP_KEYS`] for steps).
pub fn key_names(model: &Model) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = model.keys.iter().map(|k| k.name).collect();
    if model.step {
        v.extend(COMMON_STEP_KEYS.iter().map(|k| k.name));
    }
    v
}

/// Where an element that is only valid inside another one belongs.
pub fn nested_only_parent(name: &str) -> Option<&'static str> {
    NESTED_ONLY.iter().find(|(n, _)| *n == name).map(|(_, p)| *p)
}

/// The closest candidate to `name` (case-insensitive edit distance), if it is close enough
/// to be a likely typo.
pub fn closest<'a, I: IntoIterator<Item = &'a str>>(name: &str, candidates: I) -> Option<&'a str> {
    let lower = name.to_ascii_lowercase();
    let mut best: Option<(usize, &'a str)> = None;
    for c in candidates {
        let d = edit_distance(&lower, &c.to_ascii_lowercase());
        if best.map(|(bd, _)| d < bd).unwrap_or(true) {
            best = Some((d, c));
        }
    }
    let (d, c) = best?;
    let limit = match name.chars().count() {
        0..=3 => 1,
        4..=6 => 2,
        _ => 3,
    };
    if d <= limit {
        Some(c)
    } else {
        None
    }
}

/// Levenshtein distance (with adjacent transpositions counted as one edit).
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev2: Vec<usize> = vec![0; b.len() + 1];
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                cur[j] = cur[j].min(prev2[j - 2] + 1);
            }
        }
        prev2 = std::mem::replace(&mut prev, cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::StepRegistry;

    #[test]
    fn every_registered_step_has_a_model_and_back() {
        let registered = StepRegistry::with_defaults().names();
        for name in &registered {
            assert!(step(name).is_some(), "step '{}' is registered in the loader but has no model in model.rs", name);
        }
        for m in STEPS {
            assert!(registered.iter().any(|n| n == m.name), "model '{}' has no step handler in the loader", m.name);
        }
    }

    #[test]
    fn referenced_models_exist() {
        let check = |owner: &str, k: &Key| match k.kind {
            Object(n) | List(n) | Section(n) => assert!(model(n).is_some(), "{}.{} refers to unknown model '{}'", owner, k.name, n),
            _ => {}
        };
        for m in STEPS.iter().chain(MODELS.iter()) {
            for k in m.keys {
                check(m.name, k);
            }
            for alts in m.required {
                for a in *alts {
                    assert!(key(m, a).is_some(), "required key '{}' of '{}' is not a key", a, m.name);
                }
            }
        }
        for k in TOP_LEVEL {
            check("<top>", k);
        }
    }

    /// The literal keys a handler reads from its configuration, in the handler's source
    /// (`cfg.get("x")`, `opt_str(cfg, "x")`, `flag(cfg, "x", ..)`, `required_str(cfg, &["x", ..])`).
    fn handler_keys(src: &str) -> Vec<(String, Vec<String>)> {
        let mut out: Vec<(String, Vec<String>)> = Vec::new();
        for chunk in src.split("r.register(\"").skip(1) {
            let Some(end) = chunk.find('"') else { continue };
            let name = chunk[..end].to_string();
            let body = match chunk.find("\n    r.register(").or_else(|| chunk.find("\n}\n")) {
                Some(e) => &chunk[..e],
                None => chunk,
            };
            let mut keys = Vec::new();
            for pat in ["cfg.get(\"", "(cfg, \"", "(cfg, &[\"", ", cfg, &[\""] {
                let mut rest = body;
                while let Some(p) = rest.find(pat) {
                    rest = &rest[p + pat.len()..];
                    let list_end = if pat.ends_with("[\"") { rest.find(']').unwrap_or(0) } else { rest.find('"').unwrap_or(0) };
                    for k in rest[..list_end].split(',') {
                        let k = k.trim().trim_matches('"').trim_end_matches(']');
                        if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                            keys.push(k.to_string());
                        }
                    }
                }
            }
            out.push((name, keys));
        }
        out
    }

    #[test]
    fn handler_keys_are_in_the_models() {
        let sources = [include_str!("dsl.rs"), include_str!("dsl/eips.rs"), include_str!("dsl/eips_extra.rs")];
        let mut checked = 0;
        for src in sources {
            for (name, keys) in handler_keys(src) {
                let m = step(&name).unwrap_or_else(|| panic!("no model for step '{}'", name));
                for k in keys {
                    checked += 1;
                    assert!(key(m, &k).is_some() || languages().contains(&k.as_str()) || k == "expression", "step '{}' reads key '{}' but its model does not list it", name, k);
                }
            }
        }
        assert!(checked > 40, "the source scan found only {} keys", checked);
    }

    #[test]
    fn closest_finds_typos() {
        assert_eq!(closest("setHeadr", STEPS.iter().map(|m| m.name)), Some("setHeader"));
        assert_eq!(closest("ot", ["to", "toD"]), Some("to"));
        assert_eq!(closest("completely-different", ["to"]), None);
        assert_eq!(edit_distance("abc", "acb"), 1);
    }
}
