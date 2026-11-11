# Troubleshooting: start-up errors, TLS failures and exception kinds

> What Cameleer's error messages mean and how to fix them: routes that fail to load or start (an unknown step, component or option, a missing property or bean, a port in use), errors while messages flow (no consumer, connection refused, HTTP status codes), TLS handshake failures, and the exception kinds that `onException` and `doCatch` match, with their hierarchy.

**Applies to:** `cameleer run`, `cameleer validate` and programs that embed Cameleer · **Apache Camel:** the messages and exception names follow Camel's, so most Camel answers apply

## Where to start

1. **Run `cameleer validate` on the route files.** It finds most start-up errors without
   starting anything, with the file, line and column, and often suggests the name you meant
   ([Validate route files](validate.md)).
2. **Read the first line of the error, then the last `caused by`.** A start-up error prints one
   line, `Error: <kind>: <message> (caused by <kind>: <message>)`, and `cameleer run` exits with
   1. The innermost cause names what is actually wrong.
3. **Turn up the logging** with `--log-level DEBUG` (or `camel.main.log-level=DEBUG`), and use
   `--trace` to print every message as it moves through the routes.
4. **Ask the running integration:** `cameleer get routes` shows each route's state and failure
   count, `cameleer get health` the health checks, and `cameleer log --follow` its log
   ([the cameleer command line](cli.md#managing-running-integrations)).

The same commands with the container image, from the folder that holds the route files:

```bash
cameleer validate routes/
cameleer run routes/orders.camel.yaml --log-level DEBUG --trace

docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer validate /routes/routes
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/routes/orders.camel.yaml --log-level DEBUG --trace
```

A program that embeds Cameleer gets the same errors as `Err(CamelError)` from `start()` or
`add_routes`, with the same kind and message; the [Rust developer guide](rust.md) shows the API.

## Routes that fail to load or start

### Unknown step

```text
Error: YamlDeserializationException: Error loading routes from orders.camel.yaml: Unknown step 'sett' at [0].route.from.steps[0].sett (line 5, column 11). Supported steps: agent, aggregate, bean, choice, ...
```

A step name is misspelled or is not a YAML DSL step. The message lists every step; `cameleer
validate` reports `error[dsl-structure]: unknown step 'sett'` with a "did you mean" hint. Step
names are case-sensitive and follow Camel's (`setBody`, `toD`, `doTry`); the
[YAML DSL reference](yaml-dsl.md) lists their keys.

### Invalid YAML

```text
Error: ParseException: Error loading routes from orders.camel.yaml: Invalid YAML: bad indentation of a mapping entry at line 4, column 6
```

The file is not valid YAML: usually indentation, a tab character, or an unquoted value that
contains `: ` or starts with `{`, `*` or `&`. Quote values with special characters, in particular
Simple expressions (`simple: "${body} contains 'x'"`).

### No component found with scheme

```text
Error: FailedToCreateRouteException: Failed to create route route1 at: >>> to[kafak:orders] <<< because of Failed to resolve endpoint: kafak://orders due to: No component found with scheme: kafak
```

Either the scheme is misspelled (`cameleer validate` suggests `kafka`), or the binary was built
without that component family. In a smaller build the message ends with the fix, for example
`(this cameleer binary was built without the `kafka-mqtt` feature; rebuild cameleer-main with
`--features kafka-mqtt`)`. The Docker image and `cargo install cameleer-main` have every
component; [Build features](build-features.md) lists the families. `cameleer catalog components`
lists the schemes of the binary you have.

### Unknown parameters

```text
Error: FailedToCreateRouteException: Failed to create route route1: Failed to resolve endpoint: timer://t?perod=1000 due to: There are 1 parameters that couldn't be set on the endpoint. Check the uri if the parameters are spelt correctly and that they are properties of the endpoint. Unknown parameters=[perod]
```

An endpoint option is misspelled or is not an option of that component, decided the same way
as in Apache Camel. The component's page lists every option; option names are case-sensitive
(`repeatCount`, not `repeatcount`).
### Property with key not found

```text
Error: FailedToCreateRouteException: Failed to create route route1 at: >>> to[{{orders.target}}] <<< because of Property with key [orders.target] not found in properties from text: {{orders.target}}
```

A `{{placeholder}}` has no value. Set it in `application.properties`, with `--prop
orders.target=...`, or with the environment variable `CAMEL_PROPERTY_ORDERS_TARGET`, or give it
a default in the route: `{{orders.target:log:orders}}`. `cameleer validate` reports it as a warning,
`unresolved-placeholder`, because the value often comes from where the route runs.
[Configuration](configuration.md) lists every source and the order in which they apply.

### No bean could be found in the registry

```text
Error: FailedToCreateRouteException: Failed to create route route1 at: >>> process[ref:enrichOrder] <<< because of No bean could be found in the registry for: enrichOrder
```

The route refers to a processor, strategy, data source or other bean by name, and nothing is
bound under that name. Beans come from Rust code (`ctx.registry().bind_processor(...)`, see the
[Rust developer guide](rust.md#beans-what-yaml-routes-can-refer-to-by-name)), from
`camel.beans.*` with a registered bean factory, or, for data sources, from
`camel.datasource.*`. A route file run by the plain `cameleer` binary cannot use Java classes or
beans; use a script step (`js`, `groovy`, `python`) or a built-in EIP instead. `cameleer
validate` does not report a missing processor or endpoint bean, since those can come from code;
it does report an `aggregationStrategy` name that is neither built in nor bound.

### Invalid expression

```text
Error: FailedToCreateRouteException: Failed to create route route1 at: >>> setBody[simple{${body}] <<< because of expected symbol functionEnd but was eol at location 0
```

The expression does not compile: here a Simple expression is missing its closing `}`. The cause
is an `ExpressionIllegalSyntaxException`; `cameleer validate` reports it as
`error[expression]` with the position.

### Port already in use

```text
Error: FailedToStartRouteException: Failed to start route route1 because of Failed to bind HTTP server on 0.0.0.0:8080: ...
```

Another process, often an earlier run of the same integration, listens on the port. The text
after the colon comes from the operating system ("Address already in use" on Linux, "Only one
usage of each socket address" on Windows). `cameleer ps` lists running integrations and `cameleer
stop --all` stops them; or choose another port with `--port` or `camel.server.port`. The
management port (`camel.management.port`, 9876 by default) can clash the same way.

### Global SSL context parameters are not configured

```text
useGlobalSslContextParameters=true on https but no global SSL context parameters are configured (camel.ssl.enabled=true / CamelGlobalSSLContextParameters)
```

An endpoint or component asks for the global TLS settings, and there are none. Set
`camel.ssl.enabled=true` with the `camel.ssl.*` keys, or bind them from Rust; see
[TLS and security](tls-and-security.md).

### Needs the ai feature

```text
camel.server.mcp-enabled=true needs the ai feature: rebuild with --features ai
```

The MCP server, `ai-tool` and the `agent` step are in the `ai` feature, which the default build,
the Docker image and `cargo install cameleer-main` have. A binary built with
`--no-default-features` needs `--features ai`.

## Errors while messages flow

These do not stop the integration. The exchange fails, the error handler retries it if
redelivery is configured, and the failure is logged:

```text
ERROR [Camel timer://t] DefaultErrorHandler : Failed delivery for (MessageId: ...). Exhausted after delivery attempt: 1 caught: DirectConsumerNotAvailableException: No consumers available on endpoint: direct://nowhere.
```

| kind and message | cause | fix |
|---|---|---|
| `DirectConsumerNotAvailableException: No consumers available on endpoint: direct://nowhere` | nothing consumes `from: direct:nowhere`: a typo, or the route with that `from` is in a file that was not loaded or is stopped | load the file with that route; `cameleer validate` warns `dangling-endpoint` |
| `ConnectException: Connection refused: 127.0.0.1:1` | nothing listens at that host and port | check the address and that the service runs; retry with redelivery ([Error handling](error-handling.md)) |
| `HttpOperationFailedException: HTTP operation failed invoking ... with statusCode: 404` | the server answered with a 4xx or 5xx status | the status code and response body are in the exception; `throwExceptionOnFailure=false` keeps the response as the message instead |
| `IOException: Connection closed unexpectedly` | often plain `http://` sent to a port that speaks TLS | use `https://` |
| `SocketTimeoutException`, `ExchangeTimedOutException` | the other side or a request-reply step did not answer in time | raise the component's timeout option, or handle it with `onException` |

## TLS handshake failures

TLS failures are `SSLPeerUnverifiedException` (the certificate) or `SSLHandshakeException` (the
handshake), both kinds of `SSLException` and `IOException`, so an `onException` for
`IOException` catches them too.

| message | cause | fix |
|---|---|---|
| `TLS certificate error: unable to find trusted issuer 'CN=...' for 'CN=...'` | the server's certificate is signed by a CA that is not trusted: a private CA or a self-signed certificate | add the CA to `camel.ssl.trustStore` or the component's trust store option, or set `SSL_CERT_FILE` to a bundle that contains it |
| `TLS certificate error: certificate is not valid for 'localhost' (valid for: rsa.test, *.wild.test, 127.0.0.1)` | the host name in the URI is not among the certificate's names | connect with a name the certificate lists, or set `serverName` in the TLS settings to one; issue the certificate with the right subject alternative names |
| `TLS certificate error: certificate expired: ...` (or `not yet valid`) | the certificate's dates do not include now, or the machine's clock is wrong | renew the certificate; check the clock |
| `TLS alert received from peer: ...` | the other side refused the handshake: no common protocol version or cipher suite, or it wanted a client certificate | check the server's TLS versions; for `certificate_required`, configure a client certificate (`camel.ssl.certFile` and `camel.ssl.keyFile`, or `keyStore`) |
| `TLS configuration error: ...` | a certificate, key or store file cannot be read or parsed, or a password is wrong | the message names the file; PEM, PKCS#12 and JKS are read |

Cameleer verifies every certificate chain and host name by default. `trustAllCertificates`
exists for tests; never use it in production. [TLS and security](tls-and-security.md) has every
TLS setting.

## Exception kinds

An error in Cameleer has a **kind**, named like the Java exception class Camel would throw. Kinds
have parents, so `onException` and `doCatch` for a kind also catch its subkinds, as Java
subclasses would be caught. A kind that is not in the table, such as a component's own
(`YamlDeserializationException`), is a `RuntimeException`. Java class
names with a package (`java.io.IOException`) match by their simple name.

| kind | parent |
|---|---|
| `Exception` | `Throwable` |
| `RuntimeException` | `Exception` |
| `IOException` | `Exception` |
| `TimeoutException` | `Exception` |
| `InterruptedException` | `Exception` |
| `ParseException` | `Exception` |
| `FileNotFoundException` | `IOException` |
| `ConnectException` | `IOException` |
| `SocketTimeoutException` | `IOException` |
| `SSLException` | `IOException` |
| `SSLHandshakeException` | `SSLException` |
| `SSLPeerUnverifiedException` | `SSLException` |
| `IllegalArgumentException` | `RuntimeException` |
| `NumberFormatException` | `IllegalArgumentException` |
| `IllegalStateException` | `RuntimeException` |
| `NullPointerException` | `RuntimeException` |
| `UnsupportedOperationException` | `RuntimeException` |
| `RejectedExecutionException` | `RuntimeException` |
| `ThrottlerRejectedExecutionException` | `RejectedExecutionException` |
| `CamelExecutionException` | `RuntimeException` |
| `NoSuchLanguageException` | `RuntimeException` |
| `NoSuchBeanException` | `RuntimeException` |
| `NoTypeConversionAvailableException` | `RuntimeException` |
| `TypeConversionException` | `RuntimeException` |
| `ExpressionEvaluationException` | `RuntimeException` |
| `ExpressionIllegalSyntaxException` | `ExpressionEvaluationException` |
| `FailedToCreateRouteException` | `RuntimeException` |
| `FailedToStartRouteException` | `RuntimeException` |
| `CallNotPermittedException` | `RuntimeException` |
| `CamelExchangeException` | `Exception` |
| `NoSuchEndpointException` | `CamelExchangeException` |
| `ResolveEndpointFailedException` | `CamelExchangeException` |
| `DirectConsumerNotAvailableException` | `CamelExchangeException` |
| `HttpOperationFailedException` | `CamelExchangeException` |
| `RollbackExchangeException` | `CamelExchangeException` |
| `CamelAuthorizationException` | `CamelExchangeException` |
| `NoMessageIdException` | `CamelExchangeException` |
| `NoSuchHeaderException` | `CamelExchangeException` |
| `ExchangeTimedOutException` | `CamelExchangeException` |
| `ValidationException` | `CamelExchangeException` |
| `PredicateValidationException` | `ValidationException` |

`CallNotPermittedException` comes from an open circuit breaker, `ThrottlerRejectedExecutionException`
from a throttler with `rejectExecution`, and `PredicateValidationException` from the `validate`
EIP. A processor written in Rust picks any kind (`CamelError::new("OrderRejected", "...")`) and
can give a new kind a parent with `register_kind_parent("OrderRejected",
"IllegalArgumentException")` ([Rust developer guide](rust.md#processors)). This table is checked
against the code by `troubleshooting_kinds_match_the_code` in
`crates/cameleer-main/tests/docs_reference.rs`, and the messages on this page by
`troubleshooting_messages_match_the_binary`.

## Common questions

### `cameleer validate` says the routes are valid, but `cameleer run` fails. Why?

Validate starts nothing, so it cannot see what only exists at run time: processors bound from Rust,
ports already in use, a broker that is down, certificates. Missing placeholders are warnings
there; pass the same properties with `--properties` and use `--strict` to fail on them.

### How do I see the message that failed?

The error handler logs the exchange id. `--trace` (or `cameleer trace` on a running integration)
prints each message with its body and headers at every step; a dead letter channel keeps failed
messages ([Error handling](error-handling.md)).

### Where are the logs?

On standard output, in Camel's format (`time LEVEL [thread] logger : message`). `cameleer log
--follow` shows them for an integration started in the background; in a container, `docker
logs`.

### Why does an error mention a Java class?

Kinds are named after the exceptions Apache Camel throws, so Camel's documentation, Stack Overflow
answers and existing `onException` clauses still apply. No JVM is involved.

## Related

- [Validate route files](validate.md), [the cameleer command line](cli.md)
- [Error handling](error-handling.md), [Observability](observability.md)
- [TLS and security](tls-and-security.md), [Configuration](configuration.md)
- [What Cameleer does not do](limitations.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
