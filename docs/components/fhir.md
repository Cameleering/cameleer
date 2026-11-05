# FHIR component

> The `fhir` component reads and writes healthcare data on a FHIR server (R4, R4B, R5, DSTU3): create, read, update, delete, search, history, transactions, operations, patch and validation.

**Scheme:** `fhir:` · **Crate:** `cameleer-soap` ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [FHIR](https://camel.apache.org/components/next/fhir-component.html)

Cameleer implements the FHIR REST API and the JSON and XML formats itself (no HAPI). Resources
are maps on the Camel side (`${body[id]}`, `${body[resourceType]}`), and can be sent as values
or as FHIR JSON / XML text.

## When to use it

- Turn readings from medical devices at the edge (blood pressure cuffs, glucose meters) into FHIR
  `Observation` resources.
- Sync patients or appointments between an EHR and other systems.
- Search a FHIR server and feed the results to reports or to an AI assistant that summarizes a
  patient record.

## URI format

```text
fhir://api/method?serverUrl=https://host/fhir[&options]
```

| api / method | main parameter (body) | result |
|---|---|---|
| `create/resource` | `resource` | the created resource |
| `read/resourceById` | `id` (with `resourceClass` / `type`) | the resource |
| `read/resourceByUrl` | `url` | the resource |
| `update/resource` | `resource` | the updated resource |
| `delete/resource`, `delete/resourceById`, `delete/resourceConditionalByUrl` | `resource` / `id` / `url` | `OperationOutcome` or null |
| `search/searchByUrl` | `url`, e.g. `Patient?family=Chalmers` | a `searchset` Bundle |
| `history/onInstance`, `onType`, `onServer` | | a `history` Bundle |
| `transaction/withBundle`, `withResources` | `bundle` / `resources` | the response Bundle / resources |
| `capabilities/ofType` | | the `CapabilityStatement` |
| `load-page/next`, `previous`, `byUrl` | `bundle` / `url` | the linked Bundle |
| `operation/onServer`, `onType`, `onInstance`, `onInstanceVersion`, `processMessage` | `parameters` / `bundle` | the operation's response |
| `patch/withBody`, `withUrl` | `patchBody` | the patched resource |
| `meta/getFromServer`, `getFromType`, `getFromResource`, `add`, `delete` | `meta` | the `Meta` |
| `validate/resource` | `resource` | an `OperationOutcome` |

## Example

A gateway endpoint for blood pressure cuffs. A reading such as
`{"patient": "123", "systolic": 128}` becomes a FHIR R4 `Observation` on the server, and the
endpoint answers `201` with the new resource id.

<!-- example: fhir-device-observation -->
```yaml
- route:
    id: bp-to-fhir
    from:
      uri: platform-http:/readings/blood-pressure?httpMethodRestrict=POST
      steps:
        - unmarshal:
            json: {}
        - removeHeaders:
            pattern: "*"
        - setBody:
            simple: '{"resourceType": "Observation", "status": "final", "code": {"text": "Systolic blood pressure"}, "subject": {"reference": "Patient/${body[patient]}"}, "valueQuantity": {"value": ${body[systolic]}, "unit": "mmHg"}}'
        - to: "fhir://create/resource?serverUrl={{env:FHIR_SERVER:http://localhost:8080/fhir}}&fhirVersion=R4"
        - setBody:
            simple: '{"observation": "${body[id]}"}'
        - removeHeaders:
            pattern: "*"
        - setHeader:
            name: CamelHttpResponseCode
            constant: 201
        - setHeader:
            name: Content-Type
            constant: application/json
```

## Rust DSL

```rust
fn bp_to_fhir_route(b: &mut RouteBuilder, server: &str) {
    b.from("platform-http:/readings/blood-pressure?httpMethodRestrict=POST")
        .route_id("bp-to-fhir")
        .unmarshal_json()
        .remove_headers("*")
        .set_body(simple(r#"{"resourceType": "Observation", "status": "final", "code": {"text": "Systolic blood pressure"}, "subject": {"reference": "Patient/${body[patient]}"}, "valueQuantity": {"value": ${body[systolic]}, "unit": "mmHg"}}"#))
        .to(&format!("fhir://create/resource?serverUrl={}&fhirVersion=R4", server))
        .set_body(simple(r#"{"observation": "${body[id]}"}"#))
        .remove_headers("*")
        .set_header("CamelHttpResponseCode", constant(201))
        .set_header("Content-Type", constant("application/json"));
}
```

## Options

| option | default | description |
|---|---|---|
| `serverUrl` | | FHIR base URL. Required. |
| `fhirVersion` | `R4` | `DSTU3`, `R4`, `R4B` or `R5`: selects the resource definitions. |
| `encoding` | `JSON` | `JSON` or `XML` on the wire. |
| `prettyPrint` | `false` | Pretty-print request bodies. |
| `inBody` | per method | Which parameter the body supplies. |
| `username`, `password` | | Basic authentication. |
| `accessToken` | | Bearer token. |
| `socketTimeout` / `receiveTimeout`, `connectionTimeout` | 60000, 30000 | Timeouts in ms. |
| `sslContextParameters` | | `#bean` for `https`. |
| method parameters | | `resource`, `resourceAsString`, `resourceClass`, `type`, `resourceType`, `id`, `stringId`, `version`, `url`, `bundle`, `resources`, `count`, `extraParameters`, `preferReturn`, `name`, `parameters`, `useHttpGet`, `returnType`, `respondToUri`, `asynchronous`, `patchBody`, `meta`, `profile`. |

`log`, `compress`, `summary`, `validationMode`, `proxyHost` and other HAPI client options are
accepted and ignored.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelFhir.<parameter>` | in | Any method parameter, e.g. `CamelFhir.id`; headers win over options. |
| `CamelFhirValidationSuccessful` | out | `validate/resource`: whether the outcome has no errors. |
| `Location`, `ETag` | out | Copied from the server response. |

HTTP errors fail with the HAPI exception names: `ResourceNotFoundException`,
`ResourceGoneException`, `InvalidRequestException`, `AuthenticationException`,
`UnprocessableEntityException` and others, with the `OperationOutcome` diagnostics as the
message.

## Run it with Docker

```sh
docker run --rm -p 8080:8080 -e FHIR_SERVER=https://fhir.example.org/fhir \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/bp-to-fhir.camel.yaml
curl -XPOST -H 'Content-Type: application/json' -d '{"patient": "123", "systolic": 128}' \
  localhost:8080/readings/blood-pressure
```

## Common questions

### How do I read a patient by id?
`fhir://read/resourceById?serverUrl=...&resourceClass=Patient` with the id as the body (or
`CamelFhir.id`); `Patient/123` is accepted too.

### How do I search?
`fhir://search/searchByUrl?serverUrl=...` with a body like `Patient?family=Chalmers`. The
result is a `searchset` Bundle; split `${body[entry]}` to process each match.

### Can I validate a resource against a profile without a server?
Yes: `validate/resource` with `profile` set to a StructureDefinition (file, `#bean` or text)
validates locally.

### Which FHIR versions are supported?
DSTU3, R4, R4B and R5, selected with `fhirVersion`.

## Related

- [http](http.md), [rest](rest.md), [platform-http](platform-http.md).
- Data formats: `fhirJson`, `fhirXml`.
- Apache Camel: [FHIR component](https://camel.apache.org/components/next/fhir-component.html).

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
