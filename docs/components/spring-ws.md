# Spring Web Services component

> The `spring-ws` component hosts and calls payload-oriented SOAP web services: consumers are selected by the payload root element, SOAP action, XPath or WS-Addressing, and the body is the payload XML.

**Scheme:** `spring-ws:` · **Crate:** `cameleer-soap` ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [Spring Web Services](https://camel.apache.org/components/next/spring-ws-component.html)

No Spring is involved: the scheme reproduces camel-spring-ws on Cameleer's HTTP server. Unlike
[cxf](cxf.md), there is no WSDL and no parameter mapping; routes work with the XML payload.

## When to use it

- Host a contract-first SOAP service where the route builds the XML response itself.
- Route different SOAP requests arriving at one address to different routes by root element or
  SOAP action.
- Send a payload to a SOAP service and get the response payload back.

## URI format

```text
spring-ws:rootqname:{namespace}localName[?options]      (consumer)
spring-ws:soapaction:action[?options]                   (consumer)
spring-ws:uri:http://host:port/path                     (consumer: every request to the address)
spring-ws:xpathresult:value?expression=xpath[&options]  (consumer)
spring-ws:action:wsaAction / spring-ws:to:wsaTo         (consumer, WS-Addressing)
spring-ws:beanname:mappingBean                          (consumer)
spring-ws:http://host:port/path[?options]               (producer)
```

Consumers listen on `address` (default `/ws` on the platform HTTP port). All consumers on one
address share a dispatcher; a request that matches none gets `404`.

## Example

An order status service. Requests whose payload root is `{urn:orders}GetOrderStatus` reach this
route; the order id is read with XPath and the response payload is built with Simple.

<!-- example: spring-ws-order-status -->
```yaml
- route:
    id: order-status
    from:
      uri: "spring-ws:rootqname:{urn:orders}GetOrderStatus?address=http://0.0.0.0:{{env:SOAP_PORT:8088}}/ws"
      steps:
        - setHeader:
            name: orderId
            xpath: "//*[local-name()='id']/text()"
        - setBody:
            simple: '<GetOrderStatusResponse xmlns="urn:orders"><id>${header.orderId}</id><status>shipped</status></GetOrderStatusResponse>'
```

A client sends `<GetOrderStatus xmlns="urn:orders"><id>42</id></GetOrderStatus>` with
`spring-ws:http://host:8088/ws` and gets the response payload back.

## Rust DSL

```rust
fn order_status_route(b: &mut RouteBuilder, port: u16) {
    b.from(&format!("spring-ws:rootqname:{{urn:orders}}GetOrderStatus?address=http://0.0.0.0:{}/ws", port))
        .route_id("order-status")
        .set_header("orderId", xpath("//*[local-name()='id']/text()"))
        .set_body(simple(r#"<GetOrderStatusResponse xmlns="urn:orders"><id>${header.orderId}</id><status>shipped</status></GetOrderStatusResponse>"#));
}
```

## Options

| option | default | description |
|---|---|---|
| `address` | `/ws` on the platform port | Consumer: `http(s)://host:port/path` or a path. |
| `expression` | | `xpathresult`: XPath evaluated on the payload. |
| `namespaces` | | Namespace prefixes for `expression`. |
| `soapAction` | | Producer: SOAP action. |
| `wsAddressingAction` | | Producer: WS-Addressing `Action`. |
| `soapVersion` | `1.1` | Producer: `1.1` or `1.2`. |
| `timeout` | | Producer: timeout in ms. |
| `username`, `password` | | Producer: basic authentication. |
| `sslContextParameters` | | `#bean` for `https`. |
| `wsSecurityUsername`, ... | | The WS-Security options of [cxf](cxf.md#options). |

Spring message factories and interceptors are accepted and ignored. SOAP attachments are not
supported.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelSpringWebserviceSoapAction` | out / in | SOAP action of the request; producer: overrides `soapAction`. |
| `CamelSpringWebserviceSoapHeader` | out / in | SOAP header blocks. |
| `CamelSpringWebserviceAddressingAction` | out / in | WS-Addressing action. |
| `CamelSpringWebserviceEndpointUri` | in | Producer: overrides the target address. |

A null reply body answers `202`; an exception becomes a SOAP fault. On the producer, a fault
fails the exchange with `SoapFaultClientException`.

## Run it with Docker

```sh
docker run --rm -p 8088:8088 -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/order-status.camel.yaml
curl -XPOST -H 'Content-Type: text/xml' localhost:8088/ws -d \
  '<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><GetOrderStatus xmlns="urn:orders"><id>42</id></GetOrderStatus></s:Body></s:Envelope>'
```

## Common questions

### How is this different from cxf?
`cxf` maps typed parameters from a WSDL; `spring-ws` hands the route the XML payload and sends
back whatever XML the route produces.

### How do I serve several operations at one address?
Add one route per root element or SOAP action, all with the same `address`.

### How do I return a SOAP fault?
Throw an exception in the route; it becomes a `Server` (1.1) / `Receiver` (1.2) fault.

## Related

- [cxf](cxf.md), [platform-http](platform-http.md).
- Apache Camel: [Spring Web Services component](https://camel.apache.org/components/next/spring-ws-component.html).

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
