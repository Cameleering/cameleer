<p align="center"><img src="https://raw.githubusercontent.com/cameleering/cameleer/main/site/brand/cameleer-logo.png" alt="Cameleer" width="480"></p>

# Cameleer

**Apache Camel routes, rebuilt in Rust.** If you know [Apache Camel](https://camel.apache.org/), you already know Cameleer: the same YAML DSL, endpoint URIs, Enterprise Integration Patterns and Simple expressions, in a single native binary.

Cameleer is the first [Cameleering](https://cameleer.ing/cameleering/) project: software that carries the same load on less, with every gain measured against the original ([the ledger](https://cameleer.ing/cameleering/ledger/)).

- **450x faster startup** than Apache Camel (3 ms vs 1.1 s), **up to 54x less memory**, **1.7–8.7x** the throughput in every scenario ([comparison](https://cameleer.ing/compare/apache-camel-vs-cameleer/))
- About **230 components**: Kafka, HTTP/REST, AMQP, MQTT, JMS, SQL/PostgreSQL, Redis, MongoDB, AWS, Azure, GCP, OpenAI, Slack, files, FTP/SFTP and more
- **Routes as MCP tools** for AI agents (`--mcp`, or `--mcp-stdio` for Claude Desktop): schema checks, approvals, run-once, timeouts and audit on every call; official MCP conformance 39/39 and 26/26 ([MCP](https://cameleer.ing/mcp/))
- Built for **AI agents** (the integration and action layer), **serverless** and the **edge**
- Image: one native binary on distroless, **linux/amd64 and linux/arm64**, non-root, no shell, about 20 MB to download

Website: https://cameleer.ing/ · Source: https://github.com/cameleering/cameleer

## Quick start

Save a route as `hello-api.camel.yaml`:

```yaml
- rest:
    path: /orders
    get:
      - path: /{id}
        to: direct:getOrder
- route:
    from:
      uri: direct:getOrder
      steps:
        - setBody:
            simple: '{"id": "${header.id}", "status": "shipped"}'
```

Run it and call it:

```sh
docker run --rm -p 8080:8080 -v "$PWD:/routes:ro" cameleering/cameleer run /routes/hello-api.camel.yaml
curl localhost:8080/orders/42        # {"id": "42", "status": "shipped"}
```

Any route written for Apache Camel's YAML DSL runs the same way.

## Real use cases

Each of these runs end to end in the project's test suite. Each file's header lists the environment variables it needs.

| route | what it does |
|---|---|
| [ai-ticket-triage.camel.yaml](https://github.com/cameleering/cameleer/blob/main/examples/ai-ticket-triage.camel.yaml) | Kafka in. An LLM (OpenAI, Ollama, vLLM, Azure OpenAI) classifies each ticket, and urgent ones go to their own topic. |
| [alerts-to-slack.camel.yaml](https://github.com/cameleering/cameleer/blob/main/examples/alerts-to-slack.camel.yaml) | An HTTP webhook takes monitoring alerts and posts the critical ones to Slack. |
| [files-to-s3.camel.yaml](https://github.com/cameleering/cameleer/blob/main/examples/files-to-s3.camel.yaml) | Watches a folder and uploads each new CSV file to S3. |
| [rest-api.camel.yaml](https://github.com/cameleering/cameleer/blob/main/examples/rest-api.camel.yaml) | JSON REST API with validation, a content-based router and error handling. |

```sh
docker run --rm -e KAFKA_BROKERS=kafka:9092 -e OPENAI_API_KEY \
  -v "$PWD:/routes:ro" cameleering/cameleer run /routes/ai-ticket-triage.camel.yaml
```

## Tags

| tag | meaning |
|---|---|
| `latest` | the latest release |
| `0.6.0`, `0.6` | a release, and the latest patch of a minor version |
| `edge` | a manual build from `main` |
| `sha-<commit>` | the image of one commit |
| `health`, `0.6.0-health`, `0.6-health`, `edge-health` | the same, plus the [health pack](https://github.com/cameleering/cameleer/blob/main/docs/packs/health/index.md): HL7 v2 and MLLP, FHIR R4, DICOM (DIMSE and DICOMweb), de-identification |

The same image is published to `ghcr.io/cameleering/cameleer`. Every push carries an SBOM, build provenance and a GitHub attestation: `gh attestation verify oci://ghcr.io/cameleering/cameleer:latest --owner cameleering`.

## Configuration

| | |
|---|---|
| Entrypoint | `cameleer` (the CLI; `run`, `run --mcp`, `version`, `get`, `tool`, `--help`) |
| Working directory | `/routes`: mount your route files here, read-only is fine |
| Port | `8080` (REST DSL and `platform-http`; `camel.server.port`) |
| User | `65532` (nonroot); any `--user` works |
| Environment | `{{env:NAME:default}}` in routes; `CAMEL_*` and `application.properties` for configuration; `--prop key=value` on the command line |
| Filesystem | Works with `--read-only`; no status or log files are written (`CAMEL_LOCAL_STATUS=false`), logs go to stdout |
| Stop | `SIGTERM` stops the routes gracefully |
| Health | with `--prop camel.server.healthCheckEnabled=true`: `/q/health/live` and `/q/health/ready` on port 8080 |

Kubernetes, Docker Compose and Cloud Run setups are in the [deployment guide](https://github.com/cameleering/cameleer/blob/main/docs/DEPLOYMENT.md).

## A smaller image

This image includes every component. To build one with only the component families your routes
use, clone the repository and pass them as `FEATURES`:

```sh
docker build --build-arg FEATURES=cloud,jq -t my-cameleer .
```

A feature list replaces the default, so name every family you use (`templates`, `jq`, `xpath`,
`http2` and `validate` are features too). The [build features guide](https://cameleer.ing/docs/guides/build-features.html)
lists them and what each adds.

## Rust crates and CLI

The same engine is on crates.io: `cargo install cameleer-main` gives you the CLI, and `cameleer-core` and the component crates let you embed it in your own Rust program.

## License

Apache License 2.0. The license and NOTICE are in the image under `/usr/share/doc/cameleer/`.

Cameleer is an independent project. It is not affiliated with or endorsed by the Apache Software Foundation. Apache, Apache Camel and Camel are trademarks of the ASF.
