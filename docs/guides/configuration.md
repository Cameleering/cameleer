# Configuration: properties, profiles and environment variables

> Cameleer reads its configuration the way Camel Main and Spring Boot do: `application.properties` or `application.yaml`, profile files such as `application-prod.properties`, `CAMEL_*` environment variables, and `--prop` on the command line, with later sources winning. Every key is a placeholder routes can use (`{{orders.target}}`), and `camel.main.*`, `camel.server.*`, `camel.ssl.*`, `camel.datasource.*` and `camel.component.*` keys configure the runtime without code.

**Applies to:** the `cameleer` CLI, the Docker image and `cameleer_main::Main` in Rust · **Apache Camel:** [Camel Main configuration](https://camel.apache.org/components/next/others/main.html)

## Where configuration comes from

Sources are merged in this order; a later source overrides an earlier one:

| order | source | notes |
|---|---|---|
| 1 | `application.properties`, `application.yaml`, `application.yml` | from the current directory, or the file given with `--properties` (which replaces them) |
| 2 | `application-<profile>.properties` / `.yaml` / `.yml` | next to the first file, for each active profile |
| 3 | `%<profile>.key=value` entries and YAML documents with `spring.config.activate.on-profile` | inside the files above |
| 4 | environment variables starting with `CAMEL_` | see [Environment variables](#environment-variables) |
| 5 | `--prop key=value` and the CLI flags (`--port`, `--name`, `--log-level`, ...) | also `Main::set_property` in Rust |
| 6 | defaults for keys still unset | for example `--mcp` sets `camel.server.mcp-tags=*,untagged` only if you did not |

Keys match without regard to case, dashes and underscores: `camel.main.durationMaxSeconds`,
`camel.main.duration-max-seconds` and `CAMEL_MAIN_DURATION_MAX_SECONDS` are the same key. The
Spring Boot and Quarkus spellings work too: `camel.springboot.*` and `quarkus.camel.*` are read as
`camel.main.*`, and `server.port` or `quarkus.http.port` as `camel.server.port` when that is not
set.

In the Docker image the working directory is `/routes`, so an `application.properties` in the
mounted folder is read:

```bash
docker run --rm -p 8080:8080 -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/orders.camel.yaml
```

## application.properties and application.yaml

Either format works; YAML maps are flattened to dotted keys, lists to `key[0]` (and a list of
plain values also to one comma-separated value):

```properties
camel.main.name = orders
camel.server.port = 8080
orders.batch-size = 100
```

<!-- not-camel -->
```yaml
camel:
  main:
    name: orders
  server:
    port: 8080
orders:
  batch-size: 100
```

## Profiles

A profile switches settings per environment. Activate one with `cameleer run --profile prod`,
`--dev` (profile `dev`), the property `camel.main.profile` (also `spring.profiles.active`,
`quarkus.profile`), the environment variable `CAMEL_MAIN_PROFILE` (also
`SPRING_PROFILES_ACTIVE`, `QUARKUS_PROFILE`), or `Main::set_profile` in Rust. Several profiles
are separated by commas. An active profile adds:

- `application-<profile>.properties` (or `.yaml`, `.yml`) from the same folder;
- Quarkus-style entries `%<profile>.key=value` in any file;
- YAML documents with `spring.config.activate.on-profile: <profile>` (or `spring.profiles`).

The log says `The following profiles are active: prod`. A worked example is
[below](#a-worked-example).

## Environment variables

Variables that start with `CAMEL_` become properties and override the files:

| variable | property |
|---|---|
| `CAMEL_MAIN_NAME` | `camel.main.name` |
| `CAMEL_MAIN_DURATION_MAX_SECONDS` | `camel.main.durationMaxSeconds` (the same as `camel.main.duration-max-seconds`) |
| `CAMEL_SERVER_PORT` | `camel.server.port` |
| `CAMEL_SERVER_MCP_REQUEST_STATE_KEY` | `camel.server.mcpRequestStateKey` |
| `CAMEL_COMPONENT_KAFKA_BROKERS` | `camel.component.kafka.brokers` |
| `CAMEL_COMPONENT_PLATFORM_HTTP_PORT` | `camel.component.platform-http.port` (a known scheme with a dash is matched) |

The rule: drop `CAMEL_`, lowercase, the first part after `camel.` becomes a segment, and the rest
is joined in camelCase; for `CAMEL_COMPONENT_*` the longest known component scheme is matched
first. Because keys match loosely, `camel.server.mcpRequestStateKey` and
`camel.server.mcp-request-state-key` are the same.

Any value can also read an environment variable directly with a placeholder,
`{{env:NAME}}` or `{{env:NAME:default}}`, which is the usual way to pass secrets:

```properties
camel.server.port = {{env:PORT:8080}}
camel.datasource.url = {{env:DB_URL}}
```

Other variables Cameleer reads:

| variable | effect |
|---|---|
| `CAMEL_LOG_LEVEL` | log level when nothing else sets one (`TRACE`, `DEBUG`, `INFO`, `WARN`, `ERROR`) |
| `CAMEL_HOME` | where `cameleer ps`, `stop`, `log` and the other local commands keep their status files |
| `CAMEL_LOCAL_STATUS` | `false` turns those status files off (the Docker image sets it) |
| `SSL_CERT_FILE` | the CA bundle used as the system trust store; needed on Windows ([FAQ](faq.md#which-ca-certificates-does-cameleer-trust)) |
| `TZ`, `TZDIR` | the default time zone and an alternative zoneinfo directory |
| `CAMEL_PROPERTY_<KEY>` | the value of a placeholder `{{key}}` that no source defines (`{{orders.target}}` reads `CAMEL_PROPERTY_ORDERS_TARGET`) |

## Placeholders

Every property can be used in routes and in other properties:

| placeholder | value |
|---|---|
| `{{key}}` | the property `key`; startup fails if it is not defined |
| `{{key:default}}` | the property, else `default` |
| `{{env:NAME}}`, `{{env:NAME:default}}` | the environment variable |
| `{{sys:NAME}}` | the environment variable (Java system properties don't exist here) |
| `{{aws:secret}}`, `{{azure:secret}}` and other functions | secrets from a vault: see [cloud credentials](cloud-credentials.md#secrets-in-routes) |

Placeholders inside `camel.main.*` and `camel.server.*` values are resolved before the runtime
reads them, so `camel.server.port={{env:PORT:8080}}` works on Cloud Run, Knative and anywhere
else that passes the port in `PORT`. `cameleer validate` reports a placeholder no source defines
as `unresolved-placeholder`.

## The keys

### `camel.main.*`: the runtime

| key | default | effect |
|---|---|---|
| `camel.main.name` | the context's name | the integration's name in logs, `cameleer ps` and MCP `serverInfo` |
| `camel.main.profile` | none | the active profile(s) |
| `camel.main.routes-include-pattern` | route files on the command line | more route files, comma-separated globs such as `routes/*.yaml` |
| `camel.main.kamelet-locations` | `./kamelets` | folders with Kamelets to load (alias `camel.component.kamelet.location`) |
| `camel.main.duration-max-seconds` | none | stop after this many seconds |
| `camel.main.duration-max-messages` | none | stop after this many messages have completed (a run-once job, or a Kubernetes CronJob) |
| `camel.main.duration-max-idle-seconds` | none | stop after this many seconds without a message |
| `camel.main.shutdown-timeout` | `45` | seconds to wait for in-flight messages at shutdown |
| `camel.main.log-level` | `INFO` | `TRACE`, `DEBUG`, `INFO`, `WARN`, `ERROR` (also `logging.level.root`) |
| `camel.main.dev-reload` | `false` | reload route files when they change (`--dev`) |
| `camel.main.tracing` | `false` | log every message at every step (`--trace`) |
| `camel.main.backlog-tracing` | `false` | keep recent messages for `cameleer trace` and `/q/dev/trace` |
| `camel.main.message-history` | `false` | record the steps each message passed |
| `camel.main.jmxManagementStatisticsLevel` | `RoutesOnly` | `Extended`, `Default`, `RoutesOnly` or `Off`: how much per-step statistics to keep |
| `camel.main.startup-recorder` | none | `logging` logs how long each part of startup took |
| `camel.main.route-controller-supervise-enabled` | `false` | restart routes that fail to start, with back-off (also `camel.routecontroller.enabled`) |
| `camel.main.route-controller-initial-delay`, `-back-off-delay`, `-back-off-max-attempts` | | the supervising route controller's timing (also `camel.routecontroller.*`) |
| `camel.main.lambda-handler` | `direct:lambda` | on AWS Lambda, the endpoint non-HTTP events go to ([deployment](../DEPLOYMENT.md#aws-lambda)) |

### `camel.server.*`: the shared HTTP server, health, metrics and consoles

`platform-http` and REST routes, the management endpoints and the MCP server share one HTTP
server.

| key | default | effect |
|---|---|---|
| `camel.server.port` | `8080` | the port (`0` picks a free one) |
| `camel.server.host` | `0.0.0.0` | the bind address |
| `camel.server.enabled` | `false` | start the management endpoints below (needed with any of the next three) |
| `camel.server.healthCheckEnabled` | `false` | `GET /q/health`, `/q/health/ready`, `/q/health/live` (`200` when up, `503` when down) |
| `camel.server.metricsEnabled` | `false` | `GET /q/metrics` in Prometheus text format |
| `camel.server.devConsoleEnabled` | `false` | `GET /q/dev/*` consoles and `POST /q/dev/route/{id}/start\|stop\|suspend\|resume\|reset-stats` |
| `camel.server.sslEnabled` | `false` | serve every `platform-http` endpoint over HTTPS |
| `camel.server.ssl.keyStore`, `.certFile` and `.keyFile`, `.keystorePassword`, ... | | the server certificate (the same keys as `camel.ssl.*` below) |
| `camel.server.useGlobalSslContextParameters` | `false` | take the server certificate from `camel.ssl.*` instead |
| `camel.server.mcp-*` | | the MCP server: see [MCP server configuration](mcp-server.md#configuration) |

`cameleer run --console` turns on `camel.server.enabled`, health, metrics and the dev console in
one flag.

### `camel.ssl.*`: global TLS

Endpoints with `useGlobalSslContextParameters=true`, and components that fall back to the global
parameters, use these. **They apply only with `camel.ssl.enabled=true`.**

| key | effect |
|---|---|
| `camel.ssl.enabled` | turn the global parameters on |
| `camel.ssl.keyStore` | our certificate and key: a PKCS12 file, a Java keystore (JKS) or a PEM file with the chain and key |
| `camel.ssl.certFile` and `camel.ssl.keyFile` | the same as two PEM files |
| `camel.ssl.keystorePassword`, `camel.ssl.keyPassword`, `camel.ssl.keyStoreType`, `camel.ssl.certAlias` | key store password, key password, type (`PKCS12`, `JKS`, `PEM`) and which entry to use |
| `camel.ssl.trustStore` | trusted CA certificates (PEM files, comma-separated, or a PKCS12/JKS file with `trustStorePassword`, `trustStoreType`) |
| `camel.ssl.useSystemTrustStore` | also trust the system store |
| `camel.ssl.clientAuthentication` | servers: `NONE`, `WANT` or `REQUIRE` client certificates |
| `camel.ssl.secureSocketProtocol`, `minVersion`, `maxVersion` | TLS versions (`TLSv1.2`, `TLSv1.3`) |
| `camel.ssl.hostnameVerification` | check the server's name (on by default) |
| `camel.ssl.crlFile`, `camel.ssl.sessionTimeout`, `camel.ssl.serverName` | revocation list, session lifetime, SNI name |
| `camel.ssl.trustAllCertificates` | accept any certificate; for tests only |

### `camel.datasource.*`: databases for `sql`, `jdbc` and `sql-stored`

```properties
# one data source, bound as "dataSource" (or the name in camel.datasource.name)
camel.datasource.url = jdbc:postgresql://db:5432/shop
camel.datasource.username = app
camel.datasource.password = {{env:DB_PASSWORD}}

# more, each bound under its own name
camel.datasource.reports.url = jdbc:postgresql://reports:5432/bi
```

Routes refer to them as `sql:select ...?dataSource=#reports`. See [sql](../components/sql.md).

### `camel.component.<scheme>.<option>`: defaults for every endpoint of a component

```properties
camel.component.kafka.brokers = {{env:KAFKA_BROKERS:localhost:9092}}
camel.component.seda.queueSize = 10000
```

An option on the endpoint URI still wins over the component default.

### `camel.beans.*`: beans

`camel.beans.<name>=#class:<type>` plus `camel.beans.<name>.<property>=value` create a bean
through a factory registered for that type in Rust (`cameleer_yaml::register_bean_factory`). The
`cameleer` binary registers no factories, so with the CLI and the Docker image these entries
are logged and skipped; see [what works without Rust](#what-works-without-rust).

## What works without Rust

Everything in a route URI, and everything on this page except beans, works from the CLI and the
Docker image. Some options take a reference to an object in the registry (`#name`):

| you need | without Rust | with Rust |
|---|---|---|
| a database for `sql` / `jdbc` | `camel.datasource.*` | bind your own |
| TLS settings | URI options (`sslTruststoreLocation`, ...), `camel.ssl.*`, `camel.server.ssl.*` | an `SSLContextParameters` bean |
| a component default | `camel.component.<scheme>.<option>` | the same, or configure the component |
| an aggregation strategy | the built-in names on `aggregate`, `split` and `enrich` | bind your own |
| an idempotent or aggregation repository | the in-memory default | bind a JDBC, Redis or file repository |
| a processor or bean | a script step (`js`, `groovy`, `python`, ...) | bind a `Processor` or bean factory |

## A worked example

One route, three environments. The route sends orders to a target chosen by configuration:

<!-- example: configuration-orders -->
```yaml
- route:
    id: orders
    from:
      uri: direct:order
      steps:
        - to: "{{orders.target}}"
```

<!-- file: application.properties -->
```properties
camel.main.name = orders
orders.target = log:orders?level=INFO
%dev.orders.target = log:orders?level=DEBUG
```

<!-- file: application-prod.properties -->
```properties
camel.server.port = {{env:PORT:9090}}
orders.target = log:orders?level=WARN
```

| run | `orders.target` | `camel.main.name` | `camel.server.port` |
|---|---|---|---|
| `cameleer run orders.camel.yaml` | `log:orders?level=INFO` | `orders` | `8080` (default) |
| `cameleer run orders.camel.yaml --profile dev` (or `--dev`) | `log:orders?level=DEBUG` | `orders` | `8080` |
| `PORT=7070 cameleer run orders.camel.yaml --profile prod` | `log:orders?level=WARN` | `orders` | `7070` |
| `CAMEL_MAIN_NAME=orders-eu cameleer run orders.camel.yaml` | `log:orders?level=INFO` | `orders-eu` | `8080` |
| `CAMEL_MAIN_NAME=orders-eu cameleer run orders.camel.yaml --name orders-test` | `log:orders?level=INFO` | `orders-test` | `8080` |

The test `configuration_guide_worked_example` (`crates/cameleer-main/tests/docs_reference.rs`)
writes these files from this page and checks every row of the table.

## In Rust

`cameleer_main::Main` reads the same sources:

```rust
let mut main = cameleer_main::new_main();
main.load_application_config(".")?;          // application.properties / .yaml
main.set_profile("prod");                    // or CAMEL_MAIN_PROFILE
main.set_property("camel.server.port", "9090"); // like --prop
main.add_route_file("orders.camel.yaml");
main.run()?;
```

## Common questions

### How do I use different settings for development and production?

Put the shared settings in `application.properties`, the production ones in
`application-prod.properties` (or `%prod.key=value` lines), and start with `--profile prod` or
`CAMEL_MAIN_PROFILE=prod`. See [Profiles](#profiles).

### How do I pass a secret without putting it in a file?

Use `{{env:NAME}}` in the property or route (`camel.datasource.password={{env:DB_PASSWORD}}`),
or a vault function such as `{{aws:orders/db-password}}`
([cloud credentials](cloud-credentials.md#secrets-in-routes)).

### How do I change the port?

`--port 9090`, `camel.server.port=9090`, `CAMEL_SERVER_PORT=9090`, or
`camel.server.port={{env:PORT:8080}}` to follow the platform's `PORT`.

### Why is my `application.properties` ignored?

`cameleer run` reads it from the current directory; run from that folder, or pass
`--properties path/to/application.properties` (which then replaces the files of the current
directory). In Docker, mount the folder at `/routes`, the image's working directory.

### Why does a `camel.ssl.*` setting have no effect?

Set `camel.ssl.enabled=true`; without it the global TLS parameters are ignored.

### Which keys does a component read from configuration?

Any endpoint option, as `camel.component.<scheme>.<option>`; the options are listed on each
[component page](../components/).

## Related

- [CLI reference](cli.md), [Deployment](../DEPLOYMENT.md), [Cloud credentials](cloud-credentials.md)
- [MCP server configuration](mcp-server.md#configuration)
- [TLS and security](tls-and-security.md) for every `camel.ssl.*` and `camel.server.ssl.*` key,
  [observability](observability.md) for the management server

## Found a problem?

If something on this page doesn't match what Cameleer does, please
[open an issue](https://github.com/pmsudhi/cameleer/issues/new), or fork the repository, fix
it, add a test, run `cargo test --workspace` and open a pull request.
