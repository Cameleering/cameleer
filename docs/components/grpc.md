# gRPC component

> The `grpc` component calls gRPC services and hosts gRPC services in a route, with messages described by a `.proto` file read at runtime.

**Scheme:** `grpc:` · **Crate:** `cameleer-grpc` · **Cargo feature:** [`grpc`](../guides/build-features.md) ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [gRPC](https://camel.apache.org/components/next/grpc-component.html)

Cameleer has its own HTTP/2, HPACK and protobuf implementation. Instead of generated Java stubs,
it reads the service and message definitions from `protoFile` when the route starts; messages
are maps (`{"sku": "A-100"}`) on the Camel side. Without `protoFile`, bodies are raw protobuf
bytes. It interoperates with grpcio, grpc-go and curl.

## When to use it

- Expose an operation as a gRPC service for other microservices or for an AI agent's tool
  layer.
- Call a gRPC backend from a route (unary, server streaming, client streaming, bidirectional).
- Low-latency calls between edge gateways and a central service over one HTTP/2 connection.

## URI format

```text
grpc://host:port/package.Service[?options]
```

A consumer hosts the service on `host:port`; a producer calls `method` on the service at
`host:port`.

## Example: host a gRPC service

The service definition, saved next to the route as `inventory.proto`:

```proto
syntax = "proto3";
package inventory;

service Inventory {
  rpc GetStock (StockRequest) returns (StockReply);
}

message StockRequest { string sku = 1; }
message StockReply { string sku = 1; int32 quantity = 2; }
```

The route implements `GetStock`. The request arrives as a map; the route's result (a map, here
built from JSON) is the response.

<!-- example: grpc-inventory-service -->
```yaml
- route:
    id: inventory-service
    from:
      uri: "grpc://0.0.0.0:{{env:GRPC_PORT:50051}}/inventory.Inventory?protoFile={{env:PROTO_FILE:inventory.proto}}"
      steps:
        - log: "${header.CamelGrpcMethodName} for ${body[sku]}"
        - setBody:
            simple: '{"sku": "${body[sku]}", "quantity": 12}'
        - unmarshal:
            json: {}
```

## Example: call a gRPC service

<!-- example: grpc-call-inventory -->
```yaml
- route:
    id: check-stock
    from:
      uri: timer:check-stock?repeatCount=1
      steps:
        - setBody:
            constant: '{"sku": "A-100"}'
        - to: "grpc://{{env:INVENTORY_ADDRESS:localhost:50051}}/inventory.Inventory?method=getStock&protoFile={{env:PROTO_FILE:inventory.proto}}"
        - log: "A-100: ${body[quantity]} in stock"
```

The method can be given as the stub name (`getStock`) or the proto name (`GetStock`). A JSON
text body is accepted as the request message.

## Rust DSL

```rust
fn inventory_service_route(b: &mut RouteBuilder, port: u16, proto: &str) {
    b.from(&format!("grpc://0.0.0.0:{}/inventory.Inventory?protoFile={}", port, proto))
        .route_id("inventory-service")
        .log("${header.CamelGrpcMethodName} for ${body[sku]}")
        .set_body(simple(r#"{"sku": "${body[sku]}", "quantity": 12}"#))
        .unmarshal_json();
}
```

## Options

| option | default | description |
|---|---|---|
| `protoFile` | | `.proto` file with the service and messages (`classpath:` / `file:` prefixes allowed). Without it, bodies are raw bytes. |
| `method` | | Producer: method to call. |
| `producerStrategy` | `SIMPLE` | `SIMPLE`: one call per exchange (server streaming returns an array). `STREAMING`: a long-lived client-streaming or bidi call across exchanges. |
| `streamRepliesTo` | | Endpoint that receives each streamed response. |
| `consumerStrategy` | `AGGREGATION` | `AGGREGATION`: the route runs once per call. `PROPAGATION` (or `DELEGATION`): once per request message. |
| `forwardOnCompleted`, `forwardOnError` | `false` | `PROPAGATION`: also route the completion / error event. |
| `routeControlledStreamObserver` | `false` | Consumer: the route answers through the response observer in the `CamelGrpcResponseObserver` property. |
| `deadline` | | Producer: call deadline (`grpc-timeout`), e.g. `2s`. |
| `usePlainText` | `true` | `false` uses TLS with ALPN `h2`. |
| `negotiationType` | `PLAINTEXT` | `PLAINTEXT`, `PLAINTEXT_UPGRADE` (h2c upgrade) or `TLS`. |
| `keyCertChainResource`, `keyResource` | | PEM certificate chain and key: the consumer's server identity, or the producer's client certificate. |
| `trustCertCollectionResource` | | PEM CAs: the producer trusts them; on a consumer, client certificates from them are required. |
| `authenticationType` | `NONE` | `JWT` (with `jwtSecret`, `jwtAlgorithm` HMAC256/384/512, `jwtIssuer`, `jwtSubject`) or `GOOGLE` (with `serviceAccountResource`). |
| `compression` | | `gzip` or `deflate` for requests. Alias `compressorName`. |
| `maxMessageSize` | 4 MiB | Largest message. |
| `maxConcurrentCallsPerConnection` | 256 | Consumer limit. |
| `flowControlWindow` | 1 MiB | HTTP/2 flow control window. |
| `shutdownTimeout` | `5s` | Consumer shutdown grace time. |
| `clientInterceptors`, `serverInterceptors` | | `#bean` lists of interceptors; `autoDiscoverClientInterceptors` / `autoDiscoverServerInterceptors` (default `true`) pick up registry beans. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelGrpcMethodName` | out | Consumer: method being called (stub name, e.g. `getStock`). |
| `CamelGrpcEventType` | out / in | `onNext`, `onCompleted`, `onError`. Producer with `STREAMING`: `onCompleted` half-closes the call. |
| `CamelGrpcUserAgent` | out | Consumer: client user agent. |

Request metadata becomes headers on the consumer; on the producer, non-`Camel*` scalar headers
are sent as metadata and response metadata becomes headers. Failures raise
`StatusRuntimeException` with the message `CODE: description`. On the consumer, exceptions map
to status codes: `IllegalArgumentException` to `INVALID_ARGUMENT`, `TimeoutException` to
`DEADLINE_EXCEEDED`, `UnsupportedOperationException` to `UNIMPLEMENTED`, others to `INTERNAL`.

## Run it with Docker

```sh
docker run --rm -p 50051:50051 -e PROTO_FILE=/routes/inventory.proto \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/inventory-service.camel.yaml
grpcurl -plaintext -proto inventory.proto -d '{"sku":"A-100"}' localhost:50051 inventory.Inventory/GetStock
```

## Common questions

### Do I need to generate code from the .proto file?
No. Point `protoFile` at the file; Cameleer parses it at startup.

### How do I return an error status?
Throw an exception in the route: `IllegalArgumentException` gives `INVALID_ARGUMENT`; a
`StatusRuntimeException` with message `NOT_FOUND: no such sku` gives that code.

### How do I call a server-streaming method?
With the default `SIMPLE` strategy the body becomes an array of responses; set
`streamRepliesTo=seda:replies` to also handle each response as it arrives.

### Does it support TLS?
Yes: `usePlainText=false` with `keyCertChainResource` and `keyResource` on the server and
`trustCertCollectionResource` on the client. Mutual TLS is supported.

## Related

- [thrift](thrift.md): Thrift RPC.
- [rest](rest.md), [http](http.md).
- Apache Camel: [gRPC component](https://camel.apache.org/components/next/grpc-component.html).

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
