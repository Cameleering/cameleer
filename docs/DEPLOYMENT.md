# Deploying Cameleer

Cameleer runs anywhere a container or a single Linux, macOS or Windows binary runs. This guide
covers the container image: Docker, Docker Compose, Kubernetes and serverless platforms.

## The image

| | |
|---|---|
| Name | `pmsudhi/cameleer` on Docker Hub, also `ghcr.io/pmsudhi/cameleer` |
| Platforms | `linux/amd64`, `linux/arm64` |
| Contents | one native binary (glibc) on `gcr.io/distroless/cc-debian12:nonroot`: CA certificates, tzdata, no shell |
| Size | under 20 MB to download |
| Entrypoint | `cameleer` (the CLI): `run`, `version`, `get health`, `--help` |
| Working directory | `/routes`: mount route files here, read-only is fine |
| Port | `8080`: the REST DSL, `platform-http` and the health endpoints share it |
| User | `65532` (nonroot); any other `--user` works |
| Filesystem | nothing is written: `CAMEL_LOCAL_STATUS=false`, logs go to stdout; works with a read-only root filesystem |
| Stop | `SIGTERM` stops the routes gracefully (about half a second for a REST route) |
| Tags | `latest`, `0.2.1`, `0.2`, `edge` (manual builds of `main`), `sha-<commit>` |
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
FROM pmsudhi/cameleer:0.2.1
COPY routes/ /routes/
CMD ["run", "/routes/my-route.camel.yaml", "--prop", "camel.server.enabled=true", "--prop", "camel.server.healthCheckEnabled=true"]
```

## Docker Compose

<!-- not-camel -->
```yaml
services:
  orders:
    image: pmsudhi/cameleer:0.2.1
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
          image: pmsudhi/cameleer:0.2.1
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

A route uses about 10 MB at rest. Size the memory limit for your message sizes and
concurrency, not for a JVM.

## Serverless

- **Google Cloud Run, Knative, Azure Container Apps, Fly.io:** these run the image as it is. Cloud Run sends
  requests to `PORT`, which is 8080 unless you change it. If you do change it, add
  `--prop camel.server.port={{env:PORT:8080}}`. Startup takes about 10 ms, so scale-to-zero
  has no practical cold-start cost from Cameleer itself.
- **AWS Lambda (container images):** not supported yet. Lambda needs a Runtime API client,
  which is on the [roadmap](ROADMAP.md). Use ECS/Fargate or App Runner with the image in the
  meantime.

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
