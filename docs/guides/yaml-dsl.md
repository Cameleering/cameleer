# YAML DSL reference

> The YAML DSL describes Cameleer routes in `*.camel.yaml` files: one input (`from`), a list
> of steps (EIPs), endpoints written as URIs, and expressions in Simple or another language,
> with the same names as the Apache Camel YAML DSL.

This page lists everything the YAML loader (`crates/cameleer-yaml`) accepts: the file layout,
every top-level element, the endpoint and expression forms, every step with its keys and
defaults, and the mistakes people make most. Every route on this page runs in the test suite
(`crates/cameleer-main/tests/docs_yaml.rs`): the test sends messages through it and checks the
result, and resolves every endpoint URI the way Apache Camel does, so an unknown option fails
the build.

If you generate routes with a coding assistant, read [YAML for coding agents](yaml-for-coding-agents.md)
too, and check every file with [`cameleer validate`](validate.md).

## A first look

<!-- example: yaml-first-look -->
```yaml
# hello.camel.yaml: a list of top-level elements
- route:
    id: hello                      # optional; generated when missing
    from:
      uri: timer:hello             # the input endpoint
      parameters:
        repeatCount: 1
      steps:                       # the steps, in order
        - setBody:
            simple: "Hello from ${routeId}"
        - log: "${body}"
        - to: mock:greetings
```

Three things to notice:

- The file is a YAML **list**. Each item has exactly one key: the element type (`route`,
  `rest`, `routeConfiguration`, ...).
- `steps` belongs to `from` (Camel 4 style). A `steps` list next to `from`, at route level,
  is accepted too and runs after the steps under `from`.
- Every step is a list item with one key: the step name (`setBody`, `log`, `to`, ...).

## Files: naming, finding and loading

### File names

| file name | what it holds |
|---|---|
| `*.camel.yaml` (recommended), any `*.yaml` or `*.yml` | routes and other top-level elements |
| `*.kamelet.yaml` | one Kamelet resource (`kind: Kamelet`) |
| `*.pipe.yaml` | one Pipe (`kind: Pipe` or the older `KameletBinding`) |
| `*.integration.yaml` | one Camel K `Integration` resource (its `spec.flows` are routes) |
| `*.xml` | routes in the XML DSL (same steps, same option names) |
| `application.properties`, `application.yaml` | configuration, never routes |

The `.camel.yaml` suffix is a convention from Camel JBang that makes route files easy to find
and lets editors pick the right schema; the loader only looks at the last extension. Use one
route per file for small services, or group related routes; there is no limit.

`cameleer init orders.camel.yaml` writes a starter route; `cameleer init x.kamelet.yaml`,
`x.pipe.yaml` and `x.integration.yaml` write starters for the other kinds.

### Running files

```sh
cameleer run orders.camel.yaml                    # one file
cameleer run orders.camel.yaml billing.camel.yaml # several files, one context
cameleer run routes/*.camel.yaml                  # a shell glob
cameleer run                                      # every .yaml/.yml/.xml file of the current folder
cameleer run --dev orders.camel.yaml              # reload when a file changes
docker run --rm -v "$PWD:/routes:ro" cameleering/cameleer run /routes/orders.camel.yaml
```

- `cameleer run` with no file loads every `.yaml`, `.yml` and `.xml` file of the current
  folder whose name does not contain `application`.
- A directory is not accepted as a file argument: use a shell glob (`routes/*.camel.yaml`) or
  the include pattern below.
- `camel.main.routes-include-pattern` (in `application.properties`, as `--prop`, or the
  environment variable `CAMEL_MAIN_ROUTES_INCLUDE_PATTERN`) adds files by pattern. It is a comma
  separated list of Ant-style patterns: `*` matches within a folder, `**` matches any number
  of folders, and a `file:` or `classpath:` prefix is ignored, so Camel values work:

  ```sh
  cameleer run --prop camel.main.routes-include-pattern=routes/**/*.camel.yaml
  ```

- `--dev` checks the loaded route files about three times a second and, when one changed,
  removes every route that came from a file and loads all the files again. It also selects the
  `dev` profile (`application-dev.properties`). A file that fails to load is logged, and its
  routes stay missing until you fix the file and save it again.
- All files are loaded into one context. Route ids must be unique across files; `direct:`
  endpoints connect routes from different files.
- Kamelets in `./kamelets/` (or the folders of `camel.main.kamelet-locations`) are loaded
  before the route files.

### Inside a file

- Comments start with `#` (at the start of a line, or after a space).
- Several YAML documents in one file are allowed, separated by `---`; each is a list of
  top-level elements, loaded in order.
- A document may also be a single element written as a mapping (`route: ...` without the
  leading `-`), but lists are the norm.
- Anchors (`&name`), aliases (`*name`) and merge keys (`<<`) work, so a repeated block can
  be written once.
- Errors name the place: `at [0].route.from.steps[1].setBody (line 7, column 13 in orders.camel.yaml)`.

<!-- example: yaml-multi-document -->
```yaml
# first document: the HTTP-facing route
- route:
    id: intake
    from:
      uri: direct:intake
      steps:
        - to: direct:store
---
# second document: the route it calls
- route:
    id: store
    from:
      uri: direct:store
      steps:
        - setBody:
            simple: "stored ${body}"
        - to: mock:stored
```

## Top-level elements

| element | what it does | section |
|---|---|---|
| `route` | a route: `from` plus steps | [route](#route) |
| `from` | a route without the `route:` wrapper | [from](#from-the-short-form) |
| `routeConfiguration` | `onException`, `onCompletion`, intercepts and an error handler shared by routes | [routeConfiguration](#routeconfiguration) |
| `onException`, `onCompletion`, `intercept`, `interceptFrom`, `interceptSendToEndpoint` | the same, written directly at top level (an anonymous route configuration that applies to every route without `routeConfigurationId`) | [routeConfiguration](#routeconfiguration) |
| `errorHandler` | the error handler of every route in the context | [errorHandler](#errorhandler) |
| `routeTemplate` (or `routeTemplates`) | a parameterized route | [templates](#routetemplate-and-templatedroute) |
| `templatedRoute` (or `templatedRoutes`) | a route created from a template | [templates](#routetemplate-and-templatedroute) |
| `rest` | REST endpoints (the REST DSL) | [REST DSL](#rest-and-restconfiguration) |
| `restConfiguration` | the HTTP server and binding for `rest` | [REST DSL](#rest-and-restconfiguration) |
| `beans` | named beans created by registered bean factories | [beans](#beans) |
| `dataFormats` | accepted and stored, but not used: configure data formats on the `marshal`/`unmarshal` step | |

A document whose top level is a mapping with `apiVersion` and `kind` is a Kubernetes-style
resource instead: `kind: Kamelet`, `kind: Pipe`, `kind: KameletBinding` or
`kind: Integration` (see [Kamelets and Pipes](#kamelets-and-pipes)).

Any other element name fails with `Unknown top-level element '...'` and the list of supported
names.

### route

| key | default | description |
|---|---|---|
| `from` | required | the input endpoint: a URI string, or a mapping with `uri`, `parameters` and `steps` |
| `steps` | | more steps, run after the steps under `from` |
| `id` (or `routeId`) | `route1`, `route2`, ... | the route id, used by management, logs and `cameleer get routes` |
| `description` | | free text |
| `group` | | a group name for management |
| `autoStartup` | `true` | `false` creates the route stopped; start it with `cameleer cmd start-route --id ID` |
| `startupOrder` | | an integer; lower numbers start first and stop last |
| `routeConfigurationId` | | the ids of the `routeConfiguration`s to apply, comma separated; `*` patterns match |
| `errorHandler` | the context's | an error handler for this route only (same shape as the top-level [errorHandler](#errorhandler)) |
| `errorHandlerRef` | | the name of an error handler bound in the registry |
| `routePolicy` (or `routePolicyRef`) | | names of `RoutePolicy` beans in the registry, comma separated |
| `inputType`, `outputType` | | `{urn, validate}` data type contracts, see [transformDataType](#message-content) |

Other keys are kept as route properties and do not change behaviour, so a misspelled option
(for example `autostartup`) is silently ignored by the loader. `cameleer validate` reports it.

<!-- example: yaml-route-options -->
```yaml
- route:
    id: nightly-report
    description: Builds the nightly report; started by an operator
    group: reports
    autoStartup: false
    from:
      uri: direct:report
      steps:
        - to: mock:report
- route:
    id: always-on
    startupOrder: 10
    from:
      uri: direct:ping
      steps:
        - setBody:
            constant: pong
```

### from: the short form

A route can be written without the `route:` wrapper when it needs no route options:

<!-- example: yaml-from-short-form -->
```yaml
- from:
    uri: direct:short
    steps:
      - setBody:
          simple: "short form: ${body}"
      - to: mock:short
```

### routeConfiguration

A `routeConfiguration` holds error handling and interceptors that several routes share, as
in Camel 4. Its keys:

| key | description |
|---|---|
| `id` | the id routes refer to with `routeConfigurationId` |
| `errorHandler` | an error handler for the routes it applies to |
| `onException` | a list of `- onException:` clauses (see [error handling steps](#error-handling)) |
| `onCompletion` | a list of `- onCompletion:` clauses |
| `intercept`, `interceptFrom`, `interceptSendToEndpoint` | lists of interceptors |
| `precondition`, `description` | accepted and ignored |

Which routes get it:

- a configuration **with** an `id` applies to the routes that list it in
  `routeConfigurationId`;
- a configuration **without** an `id` applies to every route that has no
  `routeConfigurationId`;
- `onException` and the other clauses written directly at top level count as a configuration
  without an `id`.

It does not matter whether the configuration comes before or after the routes in the file.
A `routeConfigurationId` that matches nothing fails at load time.

<!-- example: yaml-route-configuration -->
```yaml
- routeConfiguration:
    id: api-errors
    onException:
      - onException:
          exception:
            - java.lang.IllegalArgumentException
          handled:
            constant: true
          steps:
            - setBody:
                simple: "rejected: ${exception.message}"
            - to: mock:rejected
    onCompletion:
      - onCompletion:
          onCompleteOnly: true
          steps:
            - to: mock:audit
- route:
    id: create-order
    routeConfigurationId: api-errors
    from:
      uri: direct:create-order
      steps:
        - filter:
            simple: "${body} == ''"
            steps:
              - throwException:
                  exceptionType: java.lang.IllegalArgumentException
                  message: empty order
        - setBody:
            simple: "created ${body}"
```

### errorHandler

The top-level `errorHandler` applies to every route of the context; the `errorHandler` key of a
route or a `routeConfiguration` overrides it. It holds exactly one of:

| handler | keys |
|---|---|
| `defaultErrorHandler` | `redeliveryPolicy`, `redeliveryPolicyRef`, `useOriginalMessage`, `useOriginalBody`, `retryWhile`, `onRedeliveryRef`, `onExceptionOccurredRef`, `onPrepareFailureRef` |
| `deadLetterChannel` | `deadLetterUri` (required) plus the keys above and `deadLetterHandleNewException` |
| `noErrorHandler` | none: failures go straight back to the caller |
| `refErrorHandler` | the name of an error handler bound in the registry (or `{ref: name}`) |

`redeliveryPolicy` keys: `maximumRedeliveries` (default 0, `-1` = forever), `redeliveryDelay`
(default 1000 ms), `useExponentialBackOff`, `backOffMultiplier` (default 2),
`maximumRedeliveryDelay`, `useCollisionAvoidance`, `collisionAvoidanceFactor`, `delayPattern`
(`0:1000;5:5000`), `retryAttemptedLogLevel`, `retriesExhaustedLogLevel`, `logStackTrace`,
`logExhausted`, `logRetryAttempted`, `asyncDelayedRedelivery`. Durations take milliseconds or a
unit (`250ms`, `5s`, `PT1M`). Other Camel logging options are accepted and ignored. The
[error handling guide](error-handling.md) explains the behaviour.

<!-- example: yaml-error-handler -->
```yaml
- errorHandler:
    deadLetterChannel:
      deadLetterUri: mock:dead-letters
      useOriginalMessage: true
      redeliveryPolicy:
        maximumRedeliveries: 2
        redeliveryDelay: 10ms
- route:
    id: charge
    from:
      uri: direct:charge
      steps:
        - setBody:
            simple: "charging ${body}"
        - throwException:
            exceptionType: java.io.IOException
            message: payment gateway down
```

### routeTemplate and templatedRoute

A `routeTemplate` is a route with `{{parameter}}` placeholders; a `templatedRoute` creates a
route from it. Templates are read before the other elements, so the order in the file does not
matter.

| `routeTemplate` key | description |
|---|---|
| `id` | required: the template id |
| `parameters` | a list of `{name, defaultValue, required, description}`; a parameter without `defaultValue` is required unless `required: false` |
| `from` (or `route: {from: ...}`) | the route; `{{name}}` and `{{name:default}}` are replaced in every string |
| `beans` | template-local beans, created when the template is used |
| `description` | free text |

| `templatedRoute` key | description |
|---|---|
| `routeTemplateRef` | required: the template id |
| `routeId` | the id of the new route |
| `prefixId` | a prefix added to the `id` of every step |
| `parameters` | a list of `{name, value}` or a plain mapping `{name: value}` |
| `beans` | beans to create for this route |

Placeholders that are not template parameters (`{{env:...}}`, application properties) are left
for the normal property resolution. A missing required parameter fails at load time:
`Route template sensor the following mandatory parameters are missing: room`.

<!-- example: yaml-route-template -->
```yaml
- routeTemplate:
    id: sensor
    parameters:
      - name: room
      - name: unit
        defaultValue: C
    from:
      uri: "direct:{{room}}"
      steps:
        - setBody:
            simple: "{{room}}: ${body} {{unit}}"
        - to: mock:readings
- templatedRoute:
    routeTemplateRef: sensor
    routeId: kitchen-sensor
    parameters:
      - name: room
        value: kitchen
- templatedRoute:
    routeTemplateRef: sensor
    routeId: lab-sensor
    parameters:
      room: lab
      unit: K
```

### rest and restConfiguration

The REST DSL declares HTTP operations and hands each request to a route, usually through
`direct:`. `restConfiguration` may appear in any file, before or after the `rest` elements: like
in Camel it is global, and REST routes are built when the context starts.

| `restConfiguration` key | default | description |
|---|---|---|
| `component` | `platform-http` | the HTTP server component |
| `host`, `port` | `camel.server.port` (8080) | where to listen |
| `contextPath` | | a prefix for every path, for example `/api` |
| `bindingMode` | `off` | `json`, `xml`, `json_xml`, `auto` or `off` |
| `scheme` | `http` | `http` or `https` |
| `apiContextPath` | | serve the OpenAPI document at this path (turns `apiDocs` on) |
| `apiDocs`, `enableCORS`, `skipBindingOnErrorCode` | | booleans |
| `corsHeaders`, `apiProperty`, `endpointProperty`, `consumerProperty` | | lists of `{key, value}` |

| `rest` key | description |
|---|---|
| `path` | the base path of the operations |
| `get`, `post`, `put`, `delete`, `patch`, `head`, `options` | lists of operations |
| `verb` | operations with an explicit `method` |
| `consumes`, `produces`, `bindingMode`, `description`, `tag` | defaults for the operations |

Each operation takes `path`, `to` (a URI, or `{uri, parameters}`; `toD` is accepted),
`id` (the route id), `description`, `consumes`, `produces`, `bindingMode`, `type`, `outType`,
`param` (a list of `{name, type: path|query|header|body|formData, required, dataType,
defaultValue, description, allowableValues}`), `responseMessage` (a list of `{code, message}`),
`apiDocs` and `deprecated`. Path parameters such as `{id}` become headers. Operations route
with `to`; they do not take inline steps.

<!-- example: yaml-rest -->
```yaml
- restConfiguration:
    contextPath: /api
- rest:
    path: /orders
    get:
      - path: /{id}
        id: get-order
        produces: application/json
        to: direct:get-order
    post:
      - to: direct:create-order
- route:
    from:
      uri: direct:get-order
      steps:
        - setBody:
            simple: '{"id": "${header.id}", "status": "shipped"}'
- route:
    from:
      uri: direct:create-order
      steps:
        - setHeader:
            name: CamelHttpResponseCode
            constant: 201
        - setBody:
            simple: "created ${body}"
```

`GET /api/orders/42` returns `{"id": "42", "status": "shipped"}`; `POST /api/orders` returns
status 201. See the [REST component](../components/rest.md) for binding and OpenAPI.

### beans

A `beans` element is a list of `{name, type, properties}`. Rust cannot create Java classes by name, so a bean is created only when a bean factory is
registered for its `type` (`cameleer_yaml::register_bean_factory`) by a program that embeds
Cameleer. The `cameleer` binary registers no factories: it records the definition and logs
`No bean factory registered for bean 'greeter'`. To use processors, aggregation strategies or
repositories from YAML, bind them from Rust (`ctx.registry().bind_processor(...)`) and refer to
them by name; built-in aggregation strategies work by name without any bean (see
[split](#split-aggregate-and-reorder)).

<!-- example: yaml-beans -->
```yaml
- beans:
    - name: greeter
      type: com.acme.Greeter
      properties:
        prefix: Hello
- route:
    from:
      uri: direct:beans
      steps:
        - to: mock:beans
```

### Kamelets and Pipes

A Kamelet is a reusable route snippet with typed properties. Use one as a source
(`from: kamelet:name?...`), a sink or an action (`to: kamelet:name?...`, or the `kamelet`
step). Built-in Kamelets: `timer-source`, `log-sink`, `log-action`, `insert-header-action` and
`set-body-action`; files in `./kamelets/` add more.

<!-- example: yaml-kamelet-source -->
```yaml
- route:
    from:
      uri: kamelet:timer-source
      parameters:
        message: Hello from a Kamelet
        period: 100
        repeatCount: 1
      steps:
        - kamelet:
            name: insert-header-action
            parameters:
              name: origin
              value: kamelet
        - to: mock:from-kamelet
```

A Kamelet file (`greeting-source.kamelet.yaml`) is a resource, not a list of routes. Its
template uses `{{property}}` placeholders, filled from the URI parameters, then
`camel.kamelet.<name>.<property>` properties, then the declared `default`; `{{?property}}`
drops the option when it has no value:

<!-- not-camel -->
```yaml
apiVersion: camel.apache.org/v1
kind: Kamelet
metadata:
  name: greeting-source
  labels:
    camel.apache.org/kamelet.type: source
spec:
  definition:
    required:
      - message
    properties:
      message:
        type: string
      period:
        type: integer
        default: 1000
  template:
    from:
      uri: timer:greeting
      parameters:
        period: "{{period}}"
      steps:
        - setBody:
            simple: "{{message}}"
        - to: kamelet:sink
```

A Pipe (`orders.pipe.yaml`) binds a source to a sink, with optional steps in between, and
becomes a route whose id is `metadata.name`. Each endpoint is a Kamelet `ref` with
`properties` or a plain `uri`; `spec.errorHandler` takes `sink`, `log` or `none`:

<!-- not-camel -->
```yaml
apiVersion: camel.apache.org/v1
kind: Pipe
metadata:
  name: greetings-pipe
spec:
  source:
    ref:
      kind: Kamelet
      apiVersion: camel.apache.org/v1
      name: timer-source
    properties:
      message: Hello from a Pipe
      period: 100
      repeatCount: 1
  steps:
    - ref:
        kind: Kamelet
        apiVersion: camel.apache.org/v1
        name: set-body-action
      properties:
        value: "piped: ${body}"
  sink:
    uri: mock:piped
```

`cameleer bind my-pipe --source timer-source --sink log-sink` writes the Pipe file `my-pipe.yaml`
(`--output FILE` picks another name). The
[Kamelets section](eips/system.md#kamelets) of the EIP guide has more.

## Endpoints

An endpoint is a URI: `scheme:path?option=value&option=value`. Each component's page lists its
options: [docs/components/](../components/) (for example [kafka](../components/kafka.md)), and
`cameleer catalog components` lists every scheme. Unknown options fail when the route starts,
exactly like Apache Camel: `There are 1 parameters that couldn't be set on the endpoint ...
Unknown parameters=[perod]`.

Two equivalent ways to write options:

<!-- example: yaml-endpoint-forms -->
```yaml
- route:
    from:
      uri: direct:forms
      steps:
        # inline: the whole URI in one string
        - to: "seda:inline?size=100&blockWhenFull=true"
        # uri + parameters: easier to read, values are URL-encoded for you
        - to:
            uri: seda:mapped
            parameters:
              size: 100
              blockWhenFull: true
        - to: mock:forms
```

- With `parameters`, values are URL-encoded (spaces, `&`, `+`, `=` are safe), except values
  that contain `{{`, `${` or start with `RAW(`, which are passed as they are. A list value is
  joined with commas.
- In an inline URI you encode values yourself; `+` means a space. To pass a value exactly as
  written (passwords, regular expressions), wrap it in `RAW(...)`:
  `password=RAW(se+cr&t)`. `RAW{...}` works too when the value contains `)`.
- `from` takes the same forms: `from: {uri: ..., parameters: {...}, steps: [...]}`.
- Quote a URI when it contains `#` after a space, `: ` (colon and space), or starts with `{`,
  `*`, `&`, `!`, `%` or `@`; quoting is always safe.

<!-- example: yaml-endpoint-raw -->
```yaml
- route:
    from:
      uri: direct:raw
      steps:
        - to: "log:audit?marker=RAW(a+b&c)"
        - to:
            uri: log:audit-mapped
            parameters:
              marker: "a+b&c"
        - to: mock:raw
```

### Property placeholders

Any string (URIs, step options, expressions) can contain placeholders, resolved when the route
is created:

| placeholder | value |
|---|---|
| `{{name}}` | the property `name` from `application.properties`/`application.yaml`, `--prop`, `CAMEL_*` environment variables, or `CAMEL_PROPERTY_NAME`; fails when missing |
| `{{name:default}}` | the same, with a default (everything after the first `:`, so `{{broker:localhost:9092}}` works) |
| `{{env:NAME}}`, `{{env:NAME:default}}` | the environment variable `NAME` |
| `{{sys:NAME}}` | the environment variable `NAME` (there are no JVM system properties) |
| `{{aws:...}}`, `{{azure:...}}`, `{{gcp:...}}` | secrets from a cloud vault, see [cloud credentials](cloud-credentials.md) |

<!-- example: yaml-placeholders -->
```yaml
- route:
    from:
      uri: "timer:config?period={{poll.period:200}}&repeatCount=1"
      steps:
        - setBody:
            simple: "{{greeting:Hello}} from {{env:SITE_NAME:the lab}}"
        - to: mock:configured
```

A value that **starts** with `{{` must be quoted (`"{{broker}}"`), otherwise YAML reads it as a
flow mapping. Never write secrets in route files: use `{{env:...}}` or a vault.

### Sending to endpoints

| step | keys | description |
|---|---|---|
| `to` | a URI string, or `uri`, `parameters`, `pattern` (`InOnly`/`InOut`) | send the message and continue with the reply |
| `toD` | the same plus `ignoreInvalidEndpoint` (default `false`) | a dynamic URI, evaluated with Simple per message |
| `inOnly`, `inOut` | like `to` | `to` with a fixed exchange pattern (deprecated in Camel 4, kept for old routes) |
| `wireTap` | URI or `uri`/`parameters`, `copy` (default `true`), `body`, `setHeader` (list of `{name, <expression>}`), `onPrepare`, `ignoreInvalidEndpoint` | send a copy asynchronously and continue at once |
| `enrich` | URI, `uri`/`parameters`, or an expression; `aggregationStrategy`, `aggregateOnException`, `ignoreInvalidEndpoint`, `shareUnitOfWork` | call an endpoint and merge its reply (default: the reply replaces the message) |
| `pollEnrich` | like `enrich`, plus `timeout` (ms; default `-1` = wait) | consume one message from an endpoint (file, seda, ...) and merge it |
| `poll` | URI or `uri`/`parameters`, `timeout` (default 20000; whole milliseconds only, a unit such as `2s` is rejected) | consume one message and make it the message |
| `kamelet` | a name, or `name` and `parameters` | send to `kamelet:name` |
| `bean` | `ref` (or `beanType`), `method`, `scope` | call a bean bound in the registry (`bean:` endpoint) |
| `process` | a name, or `ref` | run a processor bound in the registry |
| `serviceCall` | `name`, `uri`, `component`, `pattern`, `expression`, `staticServiceDiscovery`, `serviceDiscoveryRef`, filters, `randomLoadBalancer`/`roundRobinLoadBalancer` | call a named service through a load balancer |

`uri` values with `${...}` need `toD`; `to` treats the URI as fixed text.

`pollEnrich` and `poll` consume from an endpoint in the middle of a route, for example a
message waiting on a queue or a file in a folder:

<!-- example: yaml-polling -->
```yaml
- route:
    from:
      uri: direct:with-config
      steps:
        - pollEnrich:
            uri: seda:latest-config
            timeout: 2000
        - setBody:
            simple: "config is ${body}"
        - to: mock:configured-call
- route:
    from:
      uri: direct:next-job
      steps:
        - poll:
            uri: seda:jobs
            timeout: 2000
        - to: mock:job
```

<!-- example: yaml-sending -->
```yaml
- route:
    from:
      uri: direct:dispatch
      steps:
        - wireTap: mock:audit-copy
        - enrich:
            uri: direct:price-lookup
        - toD: "mock:region-${header.region}"
- route:
    from:
      uri: direct:price-lookup
      steps:
        - setBody:
            simple: "${body} costs 42"
```

## Expressions and predicates

Steps such as `setBody`, `filter`, `split` and `choice`/`when` take an expression (a value) or
a predicate (an expression whose result is read as true or false). Every form below works
wherever a step takes one.

| form | example | meaning |
|---|---|---|
| short | `simple: "${body.length()}"` | a language key and the expression text |
| long | `simple: {expression: "${header.n}", resultType: int}` | the text plus language options |
| wrapped | `expression: {simple: "${body}"}` | the Camel schema's explicit form (also `expression: {constant: x}`) |
| plain string | `setBody: "Hello ${body}"` | a Simple expression |
| plain number or boolean | `setBody: 42` | a constant |
| generic language | `language: {language: groovy, expression: "..."}` | any registered language by name |

A step may name only one language key; two (for example `simple` and `constant`) fail with
`only one expression language may be specified`. Language options in the long form (for
example `resultType`, `trim`, `headerName`, `source`, `suppressExceptions`) are passed to the
language.

<!-- example: yaml-expression-forms -->
```yaml
- route:
    from:
      uri: direct:forms-of-expressions
      steps:
        - setHeader:
            name: short
            simple: "${body.toUpperCase()}"
        - setHeader:
            name: typed
            simple:
              expression: "${body.length()}"
              resultType: java.lang.Integer
        - setHeader:
            name: wrapped
            expression:
              constant: fixed
        - setHeader:
            name: plain
            expression: "plain ${body}"
        - setBody: "Hello ${body}"
        - to: mock:expressions
```

### Language keys

| key | language | notes |
|---|---|---|
| `simple` | [Simple](../languages/simple.md) | the default; `${body}`, `${header.x}`, operators, functions |
| `constant` | [Constant](../languages/constant.md) | a fixed value |
| `header` | [Header](../languages/header.md) | the value of a header: `header: orderId` |
| `exchangeProperty` | [Exchange property](../languages/exchangeProperty.md) | the value of an exchange property |
| `variable` | [Variable](../languages/variable.md) | the value of a variable |
| `jsonpath` | [JSONPath](../languages/jsonpath.md) | `jsonpath: "$.items[*].sku"` |
| `jq` | [jq](../languages/jq.md) | `jq: ".items \| length"` |
| `xpath` | [XPath](../languages/xpath.md) | XPath 1.0 |
| `xquery` | [XQuery](../languages/xquery.md) | |
| `tokenize` | [Tokenize](../languages/tokenize.md) | `tokenize: ","` or `tokenize: {token: ",", group: 10}` |
| `method` | [Bean method](../languages/bean.md) | a bean in the registry |
| `ref` | [Ref](../languages/ref.md) | an expression bound in the registry |
| `groovy`, `js`/`javaScript`, `python`, `mvel`, `ognl`, `spel`, `datasonnet`, `csimple`, `joor`/`java` | script languages, for example [groovy](../languages/groovy.md) and [python](../languages/python.md) | interpreters from `cameleer-script` |
| `hl7terser` | [HL7 terser](../packs/health/languages/hl7terser.md) | |
| `language` | any | `language: {language: <name>, expression: ...}` |

`cameleer catalog languages` lists the languages of your build. The loader also accepts the
keys `xtokenize` and `wasm`, but no language of those names is built in, so a route that uses
them fails when it starts.

<!-- example: yaml-languages -->
```yaml
- route:
    from:
      uri: direct:languages
      steps:
        - setProperty:
            name: source
            constant: sensor-7
        - setHeader:
            name: firstSku
            jsonpath: "$.items[0].sku"
        - setHeader:
            name: count
            jq: ".items | length"
        - setHeader:
            name: from
            exchangeProperty: source
        - setHeader:
            name: copy
            header: firstSku
        - to: mock:languages
```

### Predicates

A predicate is an expression read as a boolean. Simple predicates use operators: `==`, `!=`,
`>`, `>=`, `<`, `<=`, `contains`, `not contains`, `startsWith`, `endsWith`, `regex`, `in`,
`range`, `is`, and `&&` / `||` to combine them. Quote string literals inside Simple with single
quotes: `${header.type} == 'gold'`. Any language works as a predicate (`jsonpath`, `jq`,
`xpath`, ...). The result is read as in Camel: a boolean is its value; a string is false only
when it is `false` (any case), so even an empty string is true; a list is true when it is not
empty; `null` is false; anything else (numbers, including `0`) is true.

## Steps

Every step is a list item with one key. Keys every step accepts:

- `id`: names the step (shown by `cameleer get routes` and in statistics; also applies to
  `when` and `otherwise`).
- `disabled: true`: skips the step, as if it were not there.

A step with no options can be written as a bare name (`- stop`) or with an empty mapping
(`- stop: {}`). Steps that contain other steps (`filter`, `split`, `doTry`, ...) take a
`steps` list, indented under the step's own keys. Unknown keys of a step are ignored by the
loader; [`cameleer validate`](validate.md) reports them.

The tables below give each step's keys and defaults. The EIP guides explain the patterns in
depth: [routing](eips/routing.md), [split and aggregate](eips/split-aggregate.md),
[transformation](eips/transformation.md), [flow control](eips/flow-control.md),
[interceptors, completions and templates](eips/system.md), and
[error handling](error-handling.md).

### Message content

| step | keys | description |
|---|---|---|
| `setBody` | an expression | replace the body |
| `transform` | an expression | the same as `setBody` (the Message Translator name) |
| `setHeader` | `name`, an expression | set one header |
| `setHeaders` | `headers`: a list of `{name, <expression>}` | set several headers in order |
| `setProperty` | `name`, an expression | set an exchange property |
| `setVariable` | `name`, an expression | set a variable |
| `setVariables` | `variables`: a list of `{name, <expression>}` | set several variables |
| `removeHeader` | a name, or `name` | remove one header |
| `removeHeaders` | a pattern, or `pattern` and `excludePattern` (list or comma separated) | remove matching headers (`*` wildcards or a regex) |
| `removeProperty`, `removeProperties` | like the header steps | remove exchange properties |
| `removeVariable` | a name, or `name` | remove a variable |
| `convertBodyTo` | a type, or `type`, `mandatory` (default `false`) | convert the body: `String`, `Integer`, `Long`, `Double`, `Boolean`, `byte[]`, Java names like `java.lang.String` |
| `convertHeaderTo` | `name`, `type`, `mandatory` | convert a header |
| `marshal`, `unmarshal` | a data format: `json: {}`, `csv: {delimiter: ";"}`, `custom: {ref: name}`, or a plain name (`- marshal: json`) | object to text/bytes and back; see [data formats](../dataformats/) |
| `transformDataType` | `toType`, `fromType` | apply a registered data type transformer |
| `script` | an expression | run a script; its result is discarded, its changes to the message are kept |
| `log` | a Simple message, or `message`, `loggingLevel` (default `INFO`), `logName`, `marker` | log a line |
| `setExchangePattern` | `InOnly` or `InOut` (or `pattern`) | change the exchange pattern |
| `claimCheck` | `operation` (`Set`, `Get`, `GetAndRemove`, `Push`, `Pop`), `key`, `filter` | store part of the message and get it back later |
| `sort` | an expression, `comparator` | sort a list body |

<!-- example: yaml-message-steps -->
```yaml
- route:
    from:
      uri: direct:message
      steps:
        - setHeaders:
            headers:
              - name: customer
                simple: "${body[customer]}"
              - name: tmpTrace
                constant: "abc"
        - setProperty:
            name: total
            simple: "${body[total]}"
        - setVariable:
            name: greeting
            constant: Dear
        - removeHeaders:
            pattern: "tmp*"
        - marshal:
            json: {}
        - convertBodyTo: String
        - log:
            message: "order JSON: ${body}"
            loggingLevel: DEBUG
            logName: orders
        - setBody:
            simple: "${variable.greeting} ${header.customer}, total ${exchangeProperty.total}"
        - to: mock:message
```

<!-- example: yaml-message-more -->
```yaml
- route:
    from:
      uri: direct:more-message-steps
      steps:
        - setVariables:
            variables:
              - name: unit
                constant: kg
              - name: tmp
                constant: scratch
        - removeVariable: tmp
        - convertHeaderTo:
            name: weight
            type: Integer
        - claimCheck:
            operation: Push
        - transform:
            simple: "${header.weight} ${variable.unit}"
        - to: mock:weight
        - claimCheck:
            operation: Pop
        - sort:
            simple: "${body}"
        - to: mock:sorted
```

### Routing

| step | keys | description |
|---|---|---|
| `filter` | a predicate, `steps` | run the steps only for matching messages; others continue after the filter |
| `choice` | `when` (list of `{<predicate>, steps}`), `otherwise` (`{steps}` or a list), `precondition` | the first matching `when` runs; otherwise `otherwise` |
| `multicast` | `steps`, `parallelProcessing`, `aggregationStrategy`, `stopOnException`, `streaming`, `timeout`, `shareUnitOfWork` | send a copy of the message to each step |
| `recipientList` | an expression, `delimiter` (default `,`), `ignoreInvalidEndpoints`, plus the multicast keys | send to the endpoints the expression returns |
| `routingSlip` | an expression, `uriDelimiter` (default `,`), `ignoreInvalidEndpoints` | send through the endpoints one after another |
| `dynamicRouter` | an expression, `uriDelimiter`, `ignoreInvalidEndpoints` | ask the expression for the next endpoint until it returns nothing |
| `loadBalance` | one policy (`roundRobin`, `random`, `topic`, `sticky: {correlationExpression}`, `weighted: {distributionRatio, roundRobin}`, `failover: {exception, maximumFailoverAttempts, roundRobin, sticky}`, `custom: {ref}`), `steps` | send each message to one of the steps (default `roundRobin`) |
| `loop` | an expression (a count, or a predicate with `doWhile: true`), `copy` (default `false`), `steps` | repeat the steps; `CamelLoopIndex` holds the index |
| `pipeline` | `steps` | run steps in sequence (useful inside `multicast`) |
| `step` | `steps`, `id` | group steps under one id for statistics |
| `stop` | none | stop routing this message |

<!-- example: yaml-choice -->
```yaml
- route:
    from:
      uri: direct:tickets
      steps:
        - choice:
            when:
              - simple: "${header.priority} == 'P1'"
                steps:
                  - to: mock:page-oncall
              - simple: "${header.priority} == 'P2'"
                steps:
                  - to: mock:team-queue
            otherwise:
              steps:
                - to: mock:backlog
        - filter:
            simple: "${header.priority} == 'P3'"
            steps:
              - stop
        - to: mock:escalated
```

<!-- example: yaml-multicast-loop -->
```yaml
- route:
    from:
      uri: direct:fan-out
      steps:
        - multicast:
            aggregationStrategy: groupedBody
            steps:
              - pipeline:
                  steps:
                    - setBody:
                        simple: "email:${body}"
              - pipeline:
                  steps:
                    - setBody:
                        simple: "sms:${body}"
        - to: mock:fanned-out
        - loop:
            constant: 3
            steps:
              - setBody:
                  simple: "attempt ${exchangeProperty.CamelLoopIndex}"
              - to: mock:loop
        - loadBalance:
            roundRobin: {}
            steps:
              - to: mock:node-a
              - to: mock:node-b
```

<!-- example: yaml-dynamic-routing -->
```yaml
- route:
    from:
      uri: direct:notify
      steps:
        # endpoints listed in a header, comma separated
        - recipientList:
            simple: "${header.channels}"
- route:
    from:
      uri: direct:photo
      steps:
        # the endpoints in turn, each one's output feeding the next
        - routingSlip:
            header: slip
- route:
    from:
      uri: direct:resize
      steps:
        - setBody:
            simple: "${body}-resized"
```

### Split, aggregate and reorder

| step | keys | description |
|---|---|---|
| `split` | an expression, `steps`, `aggregationStrategy`, `parallelProcessing`, `streaming`, `stopOnException`, `timeout`, `shareUnitOfWork`, `delimiter` (default `,`, `"false"` to keep a string whole) | run the steps once per part; parts get `CamelSplitIndex`, `CamelSplitSize`, `CamelSplitComplete` |
| `aggregate` | `correlationExpression` (required), `aggregationStrategy` (required), `completionSize`, `completionTimeout`, `completionInterval`, `completionPredicate`, `completionSizeExpression`, `completionTimeoutExpression`, `completionFromBatchConsumer`, `completionOnNewCorrelationGroup`, `eagerCheckCompletion`, `forceCompletionOnStop`, `discardOnCompletionTimeout`, `ignoreInvalidCorrelationKeys`, `closeCorrelationKeyOnCompletion`, `aggregationRepository`, `steps` | combine related messages into one |
| `resequence` | an expression, `batchConfig: {batchSize: 100, batchTimeout: 1000, allowDuplicates, reverse, ignoreInvalidExchanges}` or `streamConfig: {capacity: 1000, timeout: 1000, rejectOld, ignoreInvalidExchanges}`, `steps` | put messages back in order |

`aggregationStrategy` takes the name of a strategy bound in the registry, or a built-in one:
`groupedBody` (a list of bodies), `groupedExchange`, `useLatest`, `useOriginal`, `string`
(concatenate), also written with Camel class names such as
`#class:org.apache.camel.processor.aggregate.GroupedBodyAggregationStrategy`. Without a
strategy, `split` continues with the original message, as in Camel.

<!-- example: yaml-split-aggregate -->
```yaml
- route:
    from:
      uri: direct:csv-lines
      steps:
        - split:
            tokenize: "\n"
            aggregationStrategy: groupedBody
            steps:
              - setBody:
                  simple: "${body.toUpperCase()}"
              - to: mock:each-line
        - to: mock:all-lines
- route:
    from:
      uri: direct:readings
      steps:
        - aggregate:
            correlationExpression:
              header: device
            aggregationStrategy: groupedBody
            completionSize: 2
            completionTimeout: 5s
            steps:
              - to: mock:device-batch
```

<!-- example: yaml-resequence -->
```yaml
- route:
    from:
      uri: direct:out-of-order
      steps:
        - resequence:
            header: seq
            batchConfig:
              batchSize: 3
              batchTimeout: 200
            steps:
              - to: mock:in-order
```

### Error handling

| step | keys | description |
|---|---|---|
| `doTry` | `steps`, `doCatch` (list of `{exception: [...], onWhen, steps}`), `doFinally` (`{steps}`) | try/catch/finally inside a route |
| `onException` | `exception` (list of kinds), `handled`, `continued`, `onWhen`, `retryWhile`, `redeliveryPolicy`, `redeliveryPolicyRef`, `useOriginalMessage`, `useOriginalBody`, `onRedeliveryRef`, `onExceptionOccurredRef`, `steps` | react to a kind of failure; as a route step it applies to that route, in a `routeConfiguration` to many |
| `onCompletion` | `onCompleteOnly`, `onFailureOnly`, `mode` (`AfterConsumer` default, `BeforeConsumer`), `onWhen`, `steps` | run steps when the exchange finishes |
| `circuitBreaker` | `steps`, `onFallback` (`{steps}`), `resilience4jConfiguration` (`failureRateThreshold`, `minimumNumberOfCalls`, `slidingWindowSize`, `waitDurationInOpenState` in seconds, `permittedNumberOfCallsInHalfOpenState`, `timeoutEnabled`, `timeoutDuration`), `faultToleranceConfiguration`, `configuration` (a registry name) | stop calling a failing service for a while |
| `throwException` | `exceptionType` (required), `message` | fail the exchange with that kind of exception |
| `validate` | a predicate | fail with `PredicateValidationException` when it is false |
| `rollback` | a message, or `message`, `markRollbackOnly`, `markRollbackOnlyLast` | roll back the current transaction |
| `transacted` | a policy name, or `ref`; `steps` | run in a transaction (without `steps`: the rest of the route) |
| `policy` | `ref`, `steps` | wrap steps in a registered policy (without `steps`: the rest of the route) |
| `saga` | `propagation`, `completionMode`, `timeout`, `compensation`, `completion`, `option` (list of `{key, <expression>}`), `sagaService`, `steps` | a long-running transaction with compensation |
| `intercept`, `interceptFrom`, `interceptSendToEndpoint` | `onWhen`, `steps`; `interceptFrom` takes `uri` (default `*`); `interceptSendToEndpoint` takes `uri` (required) and `skipSendToOriginalEndpoint` | run steps before every step, every input, or every send to matching endpoints (usually in a `routeConfiguration`) |

Exception kinds are Camel's Java class names; the package is ignored, so
`java.lang.IllegalArgumentException` and `IllegalArgumentException` are the same. A kind matches
its subkinds (`java.lang.Exception` catches everything). `handled` and `continued` take `true`,
`false`, `{constant: true}` or a predicate.

<!-- example: yaml-error-steps -->
```yaml
- route:
    from:
      uri: direct:fragile
      steps:
        - onException:
            exception:
              - java.io.IOException
            redeliveryPolicy:
              maximumRedeliveries: 1
              redeliveryDelay: 0
            handled:
              constant: true
            steps:
              - setBody:
                  constant: "service unavailable, queued"
              - to: mock:retry-later
        - doTry:
            steps:
              - validate:
                  simple: "${body} != 'bad'"
            doCatch:
              - exception:
                  - org.apache.camel.support.processor.validation.PredicateValidationException
                steps:
                  - setBody:
                      constant: "replaced a bad message"
            doFinally:
              steps:
                - to: mock:checked
        - circuitBreaker:
            steps:
              - filter:
                  simple: "${body} == 'boom'"
                  steps:
                    - throwException:
                        exceptionType: java.lang.IllegalStateException
                        message: downstream failed
            onFallback:
              steps:
                - setBody:
                    constant: "fallback answer"
        - filter:
            simple: "${body} == 'offline'"
            steps:
              - throwException:
                  exceptionType: java.io.IOException
                  message: disk offline
        - to: mock:fragile-done
```

Interceptors and completions are usually written once, at top level or in a
`routeConfiguration`, and apply to every route without `routeConfigurationId`:

<!-- example: yaml-intercept -->
```yaml
- interceptSendToEndpoint:
    uri: "mock:legacy*"
    skipSendToOriginalEndpoint: true
    steps:
      - setBody:
          simple: "redirected ${body}"
      - to: mock:modern
- onCompletion:
    onFailureOnly: true
    steps:
      - to: mock:failures
- route:
    from:
      uri: direct:intercepted
      steps:
        - to: mock:legacy-system
        - filter:
            simple: "${body} contains 'fail'"
            steps:
              - throwException:
                  exceptionType: java.lang.IllegalStateException
                  message: failed on purpose
```

### Flow control

| step | keys | description |
|---|---|---|
| `throttle` | a constant expression or `maximumRequests`, `timePeriodMillis` (default 1000), `correlationExpression`, `rejectExecution`, `asyncDelayed` | at most N messages per period |
| `delay` | an expression (milliseconds), `asyncDelayed` (default `true`) | wait before continuing |
| `sample` | a period, or `samplePeriod` (default 1 s) or `messageFrequency` | let one message through per period or per N messages |
| `idempotentConsumer` | an expression (the message id), `steps`, `idempotentRepository` (default: in memory, 1000 ids), `eager` (`true`), `skipDuplicate` (`true`), `removeOnFailure` (`true`), `completionEager` (`false`) | process each id once |
| `threads` | `poolSize`, `maxPoolSize`, `keepAliveTime` (+ `timeUnit`, default seconds), `maxQueueSize`, `allowCoreThreadTimeOut`, `rejectedPolicy`, `callerRunsWhenRejected`, `threadName`, `executorService`, `steps` | continue on a thread pool (without `steps`: the rest of the list) |

<!-- example: yaml-flow-control -->
```yaml
- route:
    from:
      uri: direct:events
      steps:
        - idempotentConsumer:
            header: eventId
            steps:
              - throttle:
                  constant: 100
                  timePeriodMillis: 1000
              - delay:
                  constant: 10
              - to: mock:unique-events
```

## Values: booleans, numbers and durations

- Booleans are `true` and `false` (any case). YAML 1.2 is used, so `yes`, `no`, `on` and
  `off` are strings: a boolean option set to `yes` fails with `'yes' is not a boolean`, and
  `disabled: yes` does not disable the step. Placeholders work: `autoStartup: "{{orders.enabled:true}}"`.
- Numbers can be written plain (`completionSize: 10`) or quoted (`"10"`).
- Durations (`completionTimeout`, `redeliveryDelay`, `timePeriodMillis`, `timeout`, ...) are
  milliseconds when plain (`5000`), or take a unit: `250ms`, `5s`, `1m`, `PT1M`. The
  exception is the `timeout` of `poll`, which takes whole milliseconds only.
  `waitDurationInOpenState` of `resilience4jConfiguration` and `keepAliveTime` of `threads`
  are in seconds, as in Camel.
- Endpoint option values are text: `period: 1000` and `period: "1000"` are the same.

## Ids and descriptions

- Route ids: `id` on the route; a missing id becomes `route1`, `route2`, ... Ids must be unique
  in the context, across all files.
- Step ids: `id` on any step, `when` or `otherwise`. With `prefixId`, a `templatedRoute` adds a
  prefix to each.
- `description` on a route is kept for management; on a step it is ignored.

## Common mistakes

**`steps` at the wrong indentation.** `steps` belongs under `from` (at the same level as
`uri`) or under `route`; nested `steps` belong under their own step, at the level of its other
keys. Indentation decides which mapping a key belongs to:

```text
- route:
    from:
      uri: direct:a
      steps:           # right: under from, aligned with uri
        - to: mock:b
- route:
    from:
      uri: direct:c
  steps:               # wrong: a second key of the list item, rejected with
    - to: mock:d       # "Each top-level element must have exactly one key"
- route:
    from:
      uri: direct:e
      steps:
        - filter:
            simple: "${body} == 'x'"
          steps:       # wrong: a second key of the step, rejected with
            - to: mock:f   # "A step must have exactly one key"
```

**Two keys in one step.** Each list item is one step. Writing an option at the step level
fails with `A step must have exactly one key`:

```text
- to: mock:a
  pattern: InOut       # wrong: belongs under `to`
- to:
    uri: mock:a
    pattern: InOut     # right
```

**Unquoted special characters.** These need quotes:

| value | why | write |
|---|---|---|
| `log: Status: ${body}` | `: ` starts a mapping | `log: "Status: ${body}"` |
| `simple: Order #${body}` | ` #` starts a comment | `simple: "Order #${body}"` |
| `simple: {"id": 1}` | `{` starts a flow mapping | `simple: '{"id": 1}'` |
| `uri: {{broker}}` | `{{` starts a flow mapping | `uri: "{{broker}}"` |
| `constant: 007` | a number (7) | `constant: "007"` |
| `constant: true` | a boolean, not text | `constant: "true"` |

Single quotes keep backslashes and `${...}` literally (`'a\nb'` is four characters); double
quotes process escapes (`"a\nb"` has a line break). Use double quotes for `tokenize: "\n"`.

**`${...}` in a `to` URI.** `to` sends to the URI as written; use `toD` for a URI built from the
message.

**Booleans as words.** `yes`, `on` and `off` are not booleans (see [values](#values-booleans-numbers-and-durations)).

**`:` in URIs.** Plain URIs like `timer:tick?period=1000` need no quotes: a colon only matters
when a space follows it. Quote a URI that contains `: ` or `#`.

**Expecting `split` to return the parts.** Without `aggregationStrategy`, the message after a
`split` is the original one.

**Two expression languages in one step.** `setBody: {simple: ..., constant: ...}` fails; pick
one.

## Differences from Apache Camel

- Same element names, step names and option names as the Camel 4 YAML DSL; old Camel 3 shapes
  that the loader still reads: `expression: "text"` (Simple), `routeTemplates`/`templatedRoutes`,
  `inOnly`/`inOut`.
- Extra short forms: `setBody: "text"` (a Simple expression), `tokenize: ","`, steps as bare
  names (`- stop`), and a route mapping with `from` but without the `route:` wrapper.
- Beans of Java classes are not instantiated (see [beans](#beans)); processors and strategies are
  bound from Rust.
- `throttle` needs a constant maximum; `interceptSendToEndpoint` `afterUri` is ignored with a
  warning; `dataFormats` at top level is stored but not used; unknown step keys are ignored
  (Camel's schema rejects them); `cameleer validate` reports them.
- The loader uses YAML 1.2 (like Camel's snakeyaml-engine), so `yes`/`no` are strings.

## Common questions

### How do I write a Camel route in YAML?

Create a file ending in `.camel.yaml` with a list containing one `- route:`, give it a `from`
with a `uri` and a `steps` list, and run `cameleer run file.camel.yaml`. Start from the
[first look](#a-first-look) example.

### Can I run my Apache Camel YAML routes unchanged?

Yes when they use components and options Cameleer supports: the element, step and option names
are the same. Java beans and processors must be rewritten in Rust. `cameleer validate` tells
you what does not resolve; see [migrating from Apache Camel](migrating-from-apache-camel.md).

### How do I load all route files of a folder?

`cameleer run folder/*.camel.yaml`, or set `camel.main.routes-include-pattern=folder/**/*.camel.yaml`.
With no arguments, `cameleer run` loads the route files of the current folder.

### How do I share error handling between routes?

Put `onException` clauses and an `errorHandler` in a `routeConfiguration`. Without an `id` it
applies to every route; with an `id`, list it in each route's `routeConfigurationId`.

### Is there a JSON Schema for editors?

Yes: `cameleer schema > cameleer.schema.json` prints a JSON Schema of the YAML DSL that editors
(VS Code with the YAML extension, IntelliJ) use for completion and checks. Setup steps are in
[editor support](validate.md#editor-support); `cameleer validate` covers what a schema cannot,
such as endpoint options and expressions.

## Related

- [YAML for coding agents](yaml-for-coding-agents.md) and [`cameleer validate`](validate.md)
- [Getting started](getting-started.md), [concepts](concepts.md), [EIP guide](eips.md),
  [error handling](error-handling.md)
- [Components](../components/), [languages](../languages/), [data formats](../dataformats/)
- Apache Camel: [YAML DSL](https://camel.apache.org/components/next/others/yaml-dsl.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
