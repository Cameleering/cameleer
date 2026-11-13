# Build features: choose what goes into your build

> Cameleer's default build, image and `cargo install` include every component. Cargo features let
> you build a smaller binary that contains only the component families your routes use, for a
> serverless function, an edge device or a smaller container image.

**You don't need this page to get started.** The Docker image `cameleering/cameleer`, `cargo install
cameleer-main` and `cargo build -p cameleer-main` all contain every component, language and data
format. Every page in these docs works with them as is. Come back here when size or cold start
matters.

## What is always included

Every build, even the smallest one (`core`), contains:

- the routing engine, every EIP, error handling, sagas, route templates and Kamelets
- the YAML DSL, the XML DSL, the Rust DSL and the REST DSL
- HTTP/1.1 and TLS: `platform-http`, `http`/`https`, `rest`, `netty`, `netty-http`, `jetty`, `undertow`
- the core components: `direct`, `seda`, `vm`, `timer`, `scheduler`, `cron`, `quartz`, `file`,
  `log`, `mock`, `stub`, `bean`, `class`, `ref`, `browse`, `controlbus`, `dataformat`,
  `dataset`, `dataset-test`, `exec`, `json-validator`, `kamelet`, `language`, `saga`, `stream`,
  `test`
- the core languages: `simple`, `constant`, `header`, `exchangeProperty`, `variable`, `jsonpath`,
  `tokenize`, `bean`, `ref`
- the core data formats: `json`, `jacksonxml`, `csv`, `base64`, `hex`, `string`, `gzip`,
  `zip-deflater`, `urlencoded`
- the `cameleer` commands `run`, `init`, `ps`, `stop`, `log`, `get` and `version`

## The features

Each feature adds one family. Every component, language and data format page in these docs
names its feature in its header line (**Cargo feature:**).

| feature | adds | crate |
|---|---|---|
| `ai` | `ai-tool`, `ai-resource`, `ai-prompt`: routes as tools, resources and prompts for AI agents, with the reliability envelope; the MCP server and client and the `mcp` component | `cameleer-ai` |
| `attest` | attestation in confidential VMs (`camel.attestation.*`): evidence from configfs-tsm and dstack, bound to the routes, configuration and TLS key, at `/q/attestation` and over MCP ([attestation](attestation.md)) | `cameleer-attest` |
| `amqp` | `activemq`, `amqp-stomp`, `jms`, `rabbitmq`, `spring-rabbitmq`, `stomp` (AMQP 0-9-1, STOMP and JMS) | `cameleer-amqp` |
| `amqp10` | `amqp` (AMQP 1.0, JMS over AMQP), `azure-servicebus`, `azure-eventhubs` | `cameleer-amqp10` |
| `cloud` | every `aws*`, `azure-*` and `google-*` component except the two above (62 components: S3, SQS, SNS, DynamoDB, Lambda, Bedrock, Blob, Cosmos DB, Azure OpenAI, Pub/Sub, BigQuery, Vertex AI, ...) and the `{{aws:...}}`, `{{azure:...}}` and `{{gcp:...}}` secret placeholders | `cameleer-cloud` |
| `dataformats` | the data formats `asn1`, `avro`, `barcode`, `bindy`, `cbor`, `crypto`, `fory`, `ical`, `mime-multipart`, `pgp`, `tarfile`, `thrift`, `univocity`, `yaml`, `zipfile`, and the `thrift` component | `cameleer-dataformats` |
| `datastores` | `spring-redis`, `mongodb`, `mongodb-gridfs`, `elasticsearch`, `opensearch`, `cql` (Cassandra), `couchdb` | `cameleer-datastores` |
| `grpc` | the `grpc` component and the `protobuf` data format | `cameleer-grpc` |
| `kafka-mqtt` | `kafka`, `paho`, `paho-mqtt5` | `cameleer-kafka-mqtt` |
| `protocols` | `ftp`, `ftps`, `smtp`/`smtps`, `pop3`/`pop3s`, `imap`/`imaps`, `syslog`, `websocket`/`ws`/`wss`, `vertx-websocket`; the `syslog` data format | `cameleer-protocols` |
| `saas` | `openai`, `llm`, `anthropic`, `gemini`, `slack`, `telegram`, `github`, `twilio`, `jira`, `webhook`, `graphql`, `rss`, `atom` | `cameleer-saas` |
| `script` | the languages `js`/`javascript`, `groovy`, `python`, `java`/`joor`, `csimple`, `mvel`, `ognl`, `spel`, `datasonnet`, and the `mvel` component | `cameleer-script` |
| `soap` | `cxf`, `cxfrs`, `spring-ws`; the `jaxb` data format | `cameleer-soap` |
| `sql` | `sql`, `sql-stored`, `jdbc` (embedded SQL engine and PostgreSQL client) | `cameleer-sql` |
| `ssh` | `sftp`, `ssh` | `cameleer-ssh` |
| `xml` | `xslt`, `xslt-saxon`, `xquery`, full XML Schema validation for `validator`, `xj`, `xmlsecurity-sign`, `xmlsecurity-verify`; the `xquery` language and XPath 1.0 in full | `cameleer-xml` |
| `templates` | `mustache`, `velocity`, `freemarker`, `string-template` | part of `cameleer-core` |
| `jq` | the `jq` language and Simple's `${jq(...)}` | part of `cameleer-core` |
| `xpath` | a smaller XPath 1.0 subset for the `xpath` language and Simple's `${xpath(...)}` when `xml` is left out | part of `cameleer-core` |
| `validate` | the `cameleer validate` and `cameleer schema` commands | part of `cameleer-main` |

Two names cover sets of features:

- `full`, the default, is every feature above.
- `core` is none of them: only what is always included.

## Packs

Packs are industry-specific sets of components that are **not** in `full`
([what a pack is](../packs/index.md)). Each one is a feature of `cameleer-main` and its own
distribution:

| feature | adds | crate | distribution |
|---|---|---|---|
| `health` | `mllp`, `fhir`, `dicom`, `dicomweb`; the `hl7`, `fhirJson`, `fhirXml`, `dicom`, `dicomJson` and `dicomDeidentify` data formats; the `hl7terser` and `dicom` languages and `${dicom.*}` in Simple | `cameleer-health` | image `cameleering/cameleer:health`, archive `cameleer-health-<version>-<target>` |

```sh
cargo install cameleer-main --features health                      # everything plus the health pack
cargo build --profile dist -p cameleer-main --no-default-features --features health   # a small healthcare edge build
docker build --build-arg FEATURES=full,health -t cameleer:health .
```

A route that uses a pack component in a build without the pack fails at start-up with a message
naming the pack, the image and the feature.

## Choose features for your routes

1. List the schemes your routes use: the part before `:` in every `from` and `to` URI, plus any
   `kamelet:` sources.
2. List the languages you use besides Simple and JSONPath (`jq`, `groovy`, `xpath`, ...), and
   the data formats in `marshal` and `unmarshal` steps.
3. Look each one up in the table above, or open its page under Components, Languages or
   Data formats. Anything that isn't in the table is always included.
4. Add `validate` if you run `cameleer validate` with this binary.

For example, a route that reads SQS messages, transforms them with jq and writes rows to
PostgreSQL needs `cloud,jq,sql`. A webhook that calls Slack needs only `saas`, because the
`platform-http` consumer is always included.

**A feature list replaces the default; it doesn't add to it.** `--features cloud` gives `core`
plus `cloud` and nothing else, so jq, the template engines, HTTP/2 and `cameleer validate` are
left out. Name every feature you use, for example `--features cloud,jq,validate`.

## Build it

The same feature names work in every kind of build.

### Container image

```sh
git clone https://github.com/cameleering/cameleer && cd cameleer
docker build --build-arg FEATURES=cloud,jq,sql -t my-cameleer .
docker run --rm my-cameleer version
```

Leave out `FEATURES` for the full image, which is what `cameleering/cameleer` on Docker Hub is.
`FEATURES=core` gives the smallest image.

### The `cameleer` command

```sh
cargo install cameleer-main --no-default-features --features cloud,jq,sql
```

From a clone, `cargo build --profile dist -p cameleer-main --no-default-features --features
cloud,jq,sql` builds `target/dist/cameleer`. The `dist` profile optimizes for size and speed; use
`--release` if you want a quicker build.

### AWS Lambda

```sh
scripts/package-lambda.sh --arch x86_64 --features cloud,jq --out function.zip orders.camel.yaml
```

The [deployment guide](../DEPLOYMENT.md#aws-lambda) explains the rest of the Lambda setup.

### Your own Rust program

Depend only on the crates you need. `cameleer-core` and `cameleer-http` have default features
too; turn them off to drop the parts you don't use:

```toml
[dependencies]
cameleer-core = { version = "0.6", default-features = false, features = ["jq"] }
cameleer-http = { version = "0.6", default-features = false }   # HTTP/1.1 only, no gRPC crate
cameleer-cloud = "0.6"
```

To run YAML routes from Rust with a reduced set of components, depend on `cameleer-main` with
the same feature list as the command above:

```toml
cameleer-main = { version = "0.6", default-features = false, features = ["cloud", "jq"] }
```

## Check what a binary contains

`cameleer version` lists the features compiled in:

```text
$ cameleer version
Cameleer 0.6.0 (components: core, http, yaml, cloud, sql, jq)
```

A full build lists every feature.

## When something is missing

A route that uses a component or language left out of the build fails when it starts, and the
error names the feature to add:

```text
No component found with scheme: kafka (this cameleer binary was built without the `kafka-mqtt`
feature; rebuild cameleer-main with `--features kafka-mqtt`)
```

The same applies to the template components (`templates`), the `jq` language and Simple's
`${jq(...)}` (`jq`), `${xpath(...)}` (`xpath` or `xml`) and the
`cameleer validate` and `cameleer schema` commands (`validate`). Rebuild with the feature added
to your list; nothing else needs to change. The `agent` step and the tool loop of the model
components come with any of `ai`, `saas` or `cloud` (they are `cameleer-core`'s `agent` feature,
which those turn on); in a build without all three, an `agent:` step fails with their names.

Two parts don't fail when their feature is left out. They fall back to a smaller version that is
always included:

- **`validator` without `xml`** only checks that the XML is well-formed and that its root element
  is declared in the schema. It doesn't check content models, types or facets. Add `xml` if you
  validate against an XML Schema.
- **The `xpath` language without `xml`** uses the core XPath subset when the `xpath` feature is
  in, instead of XPath 1.0 in full.

## Sizes

Linux x86-64, `dist` profile, stripped:

| build | glibc | static musl |
|---|---:|---:|
| `core` | 5.8 MB | 6.0 MB |
| every feature (`full`) | 23.9 MB | 24.1 MB |

What each part adds to `core`: `templates` 0.50 MB, `jq` 0.42 MB, `validate` 0.27 MB and `xpath` 0.09 MB (HTTP/2 is part of `core` since 0.7). A smaller binary starts a little faster and uses less memory; startup
is about 1 ms on Linux either way. Container image sizes, cold-start times and memory are in
[BENCHMARKS.md](../BENCHMARKS.md#faas-cold-start-and-size).

## Common questions

### Do I have to choose features?

No. The default build has everything, and it is what the Docker image and `cargo install
cameleer-main` give you. Features only matter when you want a smaller binary.

### Is the full build slower than a small one?

No. Throughput is the same: a component that a route doesn't use costs nothing at run time. The
full build is only bigger on disk and takes a little longer to load.

### I upgraded and `--features cloud` lost jq (or `cameleer validate`). Why?

Since 0.4.0, the template engines, jq, the core XPath subset, HTTP/2 and `validate` are features
too, and a feature list replaces the default. Add them to your list, for example `--features
cloud,jq,validate`.

### How do I find out which feature a component needs?

Open the component's page: the header line names its feature. You can also look it up in the
table above, or build with the features you think you need and read the error, which names
the missing one.

### Does leaving out a family change how the remaining components behave?

No. Each component is the same code in every build; only which components exist changes.

## Related

- [Getting started](getting-started.md): install Cameleer and run your first route
- [Deploying Cameleer](../DEPLOYMENT.md): Docker, Compose, Kubernetes, Cloud Run and AWS Lambda
- [Benchmarks](../BENCHMARKS.md): sizes, cold start and throughput

## Found a problem?

If something on this page doesn't match what Cameleer does, please
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw, or
fork the repository, fix it, add or update a test, and open a pull request.
