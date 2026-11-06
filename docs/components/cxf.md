# CXF component

> The `cxf` component calls SOAP web services and hosts SOAP services in a route, from a WSDL, with SOAP 1.1 and 1.2, faults, WS-Security, WS-Addressing and MTOM.

**Scheme:** `cxf:` · **Crate:** `cameleer-soap` · **Cargo feature:** [`soap`](../guides/build-features.md) ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [CXF](https://camel.apache.org/components/next/cxf-component.html)

There is no Apache CXF inside: Cameleer parses the WSDL and XSD itself and maps parameters to
and from XML. In the default `POJO` data format the body is the parameter list (an array or a
map) and the result is the return value, typed from the schema.

## When to use it

- Put a JSON or REST front on a legacy SOAP service, so newer apps and AI agents can use it
  without SOAP tooling.
- Host a SOAP service that existing clients (.NET, Java) call, from a WSDL.
- Call SOAP services in finance, telecom, government and healthcare integrations.

## URI format

```text
cxf://http://host:port/path?wsdlURL=...&serviceName={ns}Service&portName={ns}Port[&options]
cxf:/path?...                (consumer on the platform HTTP port)
cxf:bean:name                (endpoint options from a registry bean)
```

## Example

A JSON endpoint in front of a SOAP calculator service. `POST /calc/add` with `{"a": 2, "b": 3}`
calls the SOAP operation `Add` with named parameters and answers `{"sum": 5}`. The WSDL is the
service's contract file (`calculator.wsdl`).

<!-- example: cxf-json-to-soap -->
```yaml
- route:
    id: calc-json-bridge
    from:
      uri: platform-http:/calc/add?httpMethodRestrict=POST
      steps:
        - unmarshal:
            json: {}
        - removeHeaders:
            pattern: "*"
        - to: "cxf://http://{{env:CALC_ADDRESS:localhost:9000}}/calculator?wsdlURL={{env:CALC_WSDL:calculator.wsdl}}&serviceName={http://example.org/calculator}CalculatorService&portName={http://example.org/calculator}CalculatorSoap&defaultOperationName=Add"
        - setHeader:
            name: Content-Type
            constant: application/json
        - setBody:
            simple: '{"sum": ${body}}'
```

A SOAP fault from the service fails the exchange with a `SoapFault` error (the HTTP endpoint
then answers 500); the fault code, string and detail are in the `CamelCxfFault*` headers.

## Rust DSL

```rust
fn calc_bridge_route(b: &mut RouteBuilder, address: &str, wsdl: &str) {
    b.from("platform-http:/calc/add?httpMethodRestrict=POST")
        .route_id("calc-json-bridge")
        .unmarshal_json()
        .remove_headers("*")
        .to(&format!(
            "cxf://http://{}/calculator?wsdlURL={}&serviceName={{http://example.org/calculator}}CalculatorService&portName={{http://example.org/calculator}}CalculatorSoap&defaultOperationName=Add",
            address, wsdl
        ))
        .set_header("Content-Type", constant("application/json"))
        .set_body(simple(r#"{"sum": ${body}}"#));
}
```

## Options

| option | default | description |
|---|---|---|
| `wsdlURL` | | WSDL location (file path, `file:`, `classpath:`, `http(s)://` or `#bean` holding the text). |
| `serviceName`, `portName` | first in the WSDL | `{namespace}local` names of the WSDL service and port. |
| `dataFormat` | `POJO` | `POJO` (typed parameters), `PAYLOAD` (body element XML), `RAW` / `CXF_MESSAGE` (the whole envelope). |
| `defaultOperationName` | | Operation when the `operationName` header is not set. |
| `defaultOperationNamespace` | WSDL namespace | Operation namespace. |
| `address` | URI address | Service address (overrides the URI and the WSDL `soap:address`). |
| `soapVersion` | `1.1` | `1.1` or `1.2` when there is no WSDL. |
| `username`, `password` | | HTTP basic authentication. |
| `receiveTimeout`, `connectionTimeout` | 60000, 30000 | Client timeouts in ms. |
| `sslContextParameters` | | `#bean` with TLS material for `https`. |
| `schemaValidationEnabled` | `false` | Consumer: validate requests against the WSDL schema. |
| `publishedEndpointUrl` | | Consumer: address written into the `?wsdl` document. |
| `attributePrefix` | `@` | POJO: prefix of XML attribute keys. |
| `wsSecurityUsername`, `wsSecurityPassword`, `wsSecurityPasswordType` | | Producer: WS-Security UsernameToken (`PasswordText` or `PasswordDigest`). |
| `wsSecurityUsers` | | Consumer: `alice:secret,bob:pw` required UsernameTokens. |
| `wsSecuritySignatureKey`, `wsSecuritySignatureCertificate` | | Producer: sign the envelope (PEM). |
| `wsSecuritySignatureVerifyCertificate`, `wsSecuritySignatureTrustedCertificates` | | Consumer: require a valid signature. |
| `wsSecurityEncryptionCertificate`, `wsSecurityEncryptionAlgorithm`, `wsSecurityDecryptionKey` | | XML Encryption of the body. |
| `wsSecuritySamlSubject`, `wsSecuritySamlIssuer`, `wsSecuritySamlAttributes` | | Add a SAML 2.0 assertion; consumer checks with `wsSecuritySamlRequired`, `wsSecuritySamlTrustedIssuers`. |
| `wsAddressingEnabled` | `false` | Add and check WS-Addressing headers (also `features=wsa`). |
| `wsrmEnabled` | `false` | WS-ReliableMessaging. |
| `mtomEnabled`, `mtomThreshold` | `false`, 1024 | Send large base64 content as MTOM/XOP parts. |

`loggingFeatureEnabled`, `serviceClass`, `wrappedStyle` and other CXF tuning options are accepted
and ignored.

## Message headers

| header | direction | description |
|---|---|---|
| `operationName` | in / out | Operation to call (producer) or being called (consumer). |
| `operationNamespace` | in / out | Namespace of the operation. |
| `SOAPAction` | in / out | SOAP action. |
| `CamelCxfSoapHeaders` | in / out | Extra SOAP header blocks (XML). |
| `CamelCxfSoapVersion` | out | Consumer: `1.1` or `1.2`. |
| `CamelCxfFaultCode`, `CamelCxfFaultString`, `CamelCxfFaultDetail` | out | Producer: details of a SOAP fault. |
| `CamelCxfWsaAction`, `CamelCxfWsaMessageId`, `CamelCxfWsaRelatesTo` | in / out | WS-Addressing values. |

Attachments travel in the `CamelAttachments` exchange property.

## Run it with Docker

```sh
docker run --rm -p 8080:8080 -e CALC_ADDRESS=soap.example.com:9000 -e CALC_WSDL=/routes/calculator.wsdl \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/calc-json-bridge.camel.yaml
curl -XPOST -H 'Content-Type: application/json' -d '{"a": 2, "b": 3}' localhost:8080/calc/add
```

## Common questions

### How do I pass parameters in POJO mode?
An array is positional (`[2, 3]`), a map is by name (`{"a": 2, "b": 3}`), a single value fills a
one-parameter operation. Values are typed from the XSD.

### How do I host a SOAP service?
Use the same URI in `from:` (`cxf://http://0.0.0.0:9000/calculator?wsdlURL=...`). The route
gets the parameter list as the body and `operationName` as a header; the body at the end is the
return value. `GET /calculator?wsdl` serves the WSDL.

### How do I return a SOAP fault?
Throw an exception; `IllegalArgumentException` becomes a `Client` (1.1) / `Sender` (1.2) fault,
anything else a `Server` / `Receiver` fault.

### Can I work with the raw XML?
Yes: `dataFormat=PAYLOAD` gives the body element XML; `dataFormat=RAW` gives the whole envelope.

## Related

- [spring-ws](spring-ws.md): payload-oriented SOAP endpoints.
- [cxfrs](cxfrs.md): JAX-RS style REST endpoints.
- [platform-http](platform-http.md), [rest](rest.md).
- Apache Camel: [CXF component](https://camel.apache.org/components/next/cxf-component.html).

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
