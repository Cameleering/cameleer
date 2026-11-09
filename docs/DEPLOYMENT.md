# Deploying Cameleer

Cameleer runs anywhere a container or a single Linux, macOS or Windows binary runs. This guide
covers the container image: Docker, Docker Compose, Kubernetes and serverless platforms.

## The image

| | |
|---|---|
| Name | `pmsudhi/cameleer` on Docker Hub, also `ghcr.io/pmsudhi/cameleer` |
| Platforms | `linux/amd64`, `linux/arm64` |
| Contents | one native binary (glibc) on `gcr.io/distroless/cc-debian12:nonroot`: CA certificates, tzdata, no shell |
| Size | about 20 MB to download; under 12.5 MB built with `FEATURES=core` ([build features](guides/build-features.md)) |
| Entrypoint | `cameleer` (the CLI): `run`, `version`, `get health`, `--help` |
| Working directory | `/routes`: mount route files here, read-only is fine |
| Port | `8080`: the REST DSL, `platform-http` and the health endpoints share it |
| User | `65532` (nonroot); any other `--user` works |
| Filesystem | nothing is written: `CAMEL_LOCAL_STATUS=false`, logs go to stdout; works with a read-only root filesystem |
| Stop | `SIGTERM` stops the routes gracefully (about half a second for a REST route) |
| Tags | `latest`, `0.5.1`, `0.5`, `edge` (manual builds of `main`), `sha-<commit>` |
| Supply chain | SBOM, max-mode build provenance and a GitHub attestation on every push |

Verify an image's provenance:

```sh
gh attestation verify oci://ghcr.io/pmsudhi/cameleer:latest --owner pmsudhi
```

## Configuration

Settings come from, in increasing priority:

1. `application.properties` or `application.yaml`, passed with `--properties /config/application.properties`
2. environment variables: `CAMEL_SERVER_PORT=9090` sets `camel.server.port`, and so on
3. `--prop key=value` on the command line

Routes read environment values with `{{env:NAME:default}}`, as in Apache Camel. Settings can
use them too: `camel.server.port={{env:PORT:8080}}`.

Health endpoints need the management server on:

```sh
--prop camel.server.enabled=true --prop camel.server.healthCheckEnabled=true
```

This serves `/q/health`, `/q/health/live` and `/q/health/ready` on port 8080. Readiness is
`DOWN` (HTTP 503) while a route is stopped. `cameleer get health --url http://localhost:8080`
exits with status 1 when it is DOWN, so it can serve as a Docker health check.

Don't turn on `camel.server.devConsoleEnabled` (or `cameleer run --console`) in production
without a network policy in front: the dev console has unauthenticated endpoints that stop
and start routes.

## Docker

```sh
docker run --rm -p 8080:8080 --read-only \
  -v "$PWD/routes:/routes:ro" \
  -e OPENAI_API_KEY \
  pmsudhi/cameleer run /routes/my-route.camel.yaml
```

- Mount route folders read-only (`:ro`) unless a route writes there. For a route that writes
  into a mounted folder (the file component's `move`, for example), run as your own user so the
  files are yours: `--user "$(id -u):$(id -g)"`.
- Reach services on the host with `--network host` on Linux. With Docker Desktop on macOS or
  Windows, use `host.docker.internal` instead. Kafka brokers must also advertise an address the
  container can reach.

### Baking routes into your own image

For production, ship an image that contains the routes:

```dockerfile
FROM pmsudhi/cameleer:0.5.1
COPY routes/ /routes/
CMD ["run", "/routes/my-route.camel.yaml", "--prop", "camel.server.enabled=true", "--prop", "camel.server.healthCheckEnabled=true"]
```

## Docker Compose

<!-- not-camel -->
```yaml
services:
  orders:
    image: pmsudhi/cameleer:0.5.1
    command: ["run", "/routes/rest-api.camel.yaml",
              "--prop", "camel.server.enabled=true", "--prop", "camel.server.healthCheckEnabled=true"]
    ports: ["8080:8080"]
    volumes: ["./routes:/routes:ro"]
    read_only: true
    healthcheck:
      test: ["CMD", "/usr/local/bin/cameleer", "get", "health", "--url", "http://localhost:8080"]
      interval: 10s
      timeout: 3s
      retries: 3
```

## Kubernetes

<!-- not-camel -->
```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: orders-routes
data:
  rest-api.camel.yaml: |
    # paste examples/rest-api.camel.yaml here
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: orders
spec:
  replicas: 2
  selector:
    matchLabels: { app: orders }
  template:
    metadata:
      labels: { app: orders }
    spec:
      terminationGracePeriodSeconds: 30
      securityContext:
        runAsNonRoot: true
        seccompProfile: { type: RuntimeDefault }
      containers:
        - name: cameleer
          image: pmsudhi/cameleer:0.5.1
          args: ["run", "/routes/rest-api.camel.yaml",
                 "--prop", "camel.server.enabled=true",
                 "--prop", "camel.server.healthCheckEnabled=true"]
          ports:
            - { name: http, containerPort: 8080 }
          env:
            - name: OPENAI_API_KEY
              valueFrom: { secretKeyRef: { name: openai, key: api-key, optional: true } }
          resources:
            requests: { cpu: 50m, memory: 32Mi }
            limits: { memory: 256Mi }
          readinessProbe:
            httpGet: { path: /q/health/ready, port: http }
            periodSeconds: 5
          livenessProbe:
            httpGet: { path: /q/health/live, port: http }
            periodSeconds: 10
          securityContext:
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: true
            capabilities: { drop: ["ALL"] }
          volumeMounts:
            - { name: routes, mountPath: /routes, readOnly: true }
      volumes:
        - name: routes
          configMap: { name: orders-routes }
```

A route uses about 10 MB at rest, most of it pages of the binary itself; the private memory of
a running route is 1–2 MB. Size the memory limit for your message sizes and concurrency, not
for a JVM.

## Serverless

- **Google Cloud Run, Knative, Azure Container Apps, Fly.io:** these run the image as it is. Cloud Run sends
  requests to `PORT`, which is 8080 unless you change it. If you do change it, add
  `--prop camel.server.port={{env:PORT:8080}}`. Startup takes milliseconds, so scale-to-zero
  has no practical cold-start cost from Cameleer itself.
- **AWS Lambda:** see [below](#aws-lambda). `cameleer` is its own Lambda custom runtime.

## AWS Lambda

`cameleer` serves Lambda invocations itself on the `provided.al2023` runtime: no runtime
library, adapter or layer. When Lambda sets `AWS_LAMBDA_RUNTIME_API`, `cameleer run` starts the
routes and then loops over the
[Runtime API](https://docs.aws.amazon.com/lambda/latest/dg/runtimes-api.html) instead of
waiting for a signal.

- **HTTP events** (API Gateway REST and HTTP APIs, function URLs, ALB) go to your
  `platform-http` routes as if they had come over a socket, and the HTTP response becomes the
  proxy-integration response. The same route file runs on Lambda and in a container.
- **Other events** (S3, SQS, EventBridge, a direct invoke) are sent request-reply to the
  handler endpoint with the event JSON as the body; the reply body is the function result. The
  handler endpoint is the function's **Handler** setting when it is an endpoint URI (for example
  `direct:orders`), else `camel.main.lambda-handler`, else `direct:lambda`.
- A failed exchange is reported to the invocation's `/error` endpoint (`errorType` is the
  exception kind); a startup failure (a bad route file, an unknown component) to `/init/error`.
- Exchange headers: `CamelAwsLambdaRequestId`, `CamelAwsLambdaDeadlineMs`,
  `CamelAwsLambdaFunctionArn`, `CamelAwsLambdaTraceId`, and when Lambda sends them
  `CamelAwsLambdaClientContext` and `CamelAwsLambdaCognitoIdentity`.
- Status files for `cameleer ps` are off on Lambda (the file system is read-only); logs go to
  CloudWatch through stderr.

<!-- example: deployment-lambda-handler -->
```yaml
# orders.camel.yaml: an S3 or EventBridge event in, a JSON summary out
- route:
    from:
      uri: direct:lambda
      steps:
        - setBody:
            simple: '{"bucket":"${jsonpath($.Records[0].s3.bucket.name)}","ok":true}'
```

Package and deploy (the binary must be a static musl build: AL2023 has glibc 2.34, older than
what current build machines link against):

```sh
scripts/package-lambda.sh --arch x86_64 --features cloud --out function.zip orders.camel.yaml
aws lambda create-function --function-name orders --runtime provided.al2023 \
  --architectures x86_64 --handler direct:lambda --zip-file fileb://function.zip \
  --role arn:aws:iam::<account>:role/<role>
```

The zip holds `bootstrap` (the `cameleer` binary under the name Lambda starts; with no
arguments it runs the route files in `$LAMBDA_TASK_ROOT`) and your route files. `--features`
keeps only the component families the routes use (see [build features](guides/build-features.md)), which
makes the zip and the cold start smaller. Measured init times are in
[BENCHMARKS.md](BENCHMARKS.md#faas-cold-start-and-size).

`camel.main.duration-max-messages` stops after that many invocations (useful in tests). Lambda
sends SIGTERM before it shuts a sandbox down; routes are stopped gracefully then.

### An MCP server on Lambda

Function URL and API Gateway requests for `/mcp` reach Cameleer's MCP server in process: zip
the tool routes with an `application.properties` that sets `camel.server.mcp-enabled=true` and
`camel.server.mcp-tags`, and build with `--features ai`. See
[MCP server: on AWS Lambda](guides/mcp-server.md#on-aws-lambda), and
[run it anywhere](guides/mcp-server.md#run-it-anywhere) for Docker, Compose, Kubernetes and
Cloud Run.

## Smaller builds

The default build and image contain every component family. A function, an edge device or a
slim image can leave the rest out with the cargo features of `cameleer-main`. The
[build features guide](guides/build-features.md) lists every feature and what it adds, shows how
to choose them for your routes, and has the commands for each kind of build. In short:

```sh
docker build --build-arg FEATURES=cloud,sql,jq -t my-cameleer .
cargo install cameleer-main --no-default-features --features cloud,sql,jq
scripts/package-lambda.sh --features cloud,jq --out function.zip orders.camel.yaml
```

- A feature list **replaces** the default: name every family you use. The template engines,
  `jq`, the core XPath subset, `http2` and `validate` (`cameleer validate`, `cameleer schema`)
  are features too.
- A route that uses something left out fails at startup with the feature to add, for example
  ``No component found with scheme: kafka (this cameleer binary was built without the
  `kafka-mqtt` feature; rebuild cameleer-main with `--features kafka-mqtt`)``.
- `cameleer version` lists the features of a build.
- `FEATURES=core` is the smallest: 5.8 MB (glibc) or 6.0 MB (static musl) on Linux x86-64,
  against 23.9 MB with every component. More sizes and cold-start times are in
  [BENCHMARKS.md](BENCHMARKS.md#faas-cold-start-and-size).

## Without a container

```sh
cargo install cameleer-main
cameleer run my-route.camel.yaml
```

Camel JBang habits carry over with the `cameleer` command: `cameleer ps`, `cameleer log`,
`cameleer stop`. Those commands use status files under `~/.camel` (`CAMEL_HOME`). Set
`CAMEL_LOCAL_STATUS=false` to run without them.

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
