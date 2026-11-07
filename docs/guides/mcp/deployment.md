# MCP use case: where to run the server

> The same route file serves MCP from a desktop, a server, Docker or Kubernetes, Cloud Run, AWS Lambda or an edge gateway. Choose stdio when the client starts the server on the same machine and Streamable HTTP when clients connect to it; choose the 2026-07-28 era when requests may land on different instances. The server commands on this page were run; the client configurations were not checked inside the clients themselves.

**Cargo feature:** [`ai`](../build-features.md) · **Examples:** [`examples/agents/mcp-shop`](https://github.com/pmsudhi/cameleer/tree/main/examples/agents/mcp-shop), [`examples/edge/mcp-gateway-tools.camel.yaml`](https://github.com/pmsudhi/cameleer/blob/main/examples/edge/mcp-gateway-tools.camel.yaml) · **Part of:** [Cameleer MCP](../mcp.md)

## Choose an environment

| environment | transport | good for | watch out for |
|---|---|---|---|
| a desktop client (Claude Desktop, VS Code, Cursor, Claude Code) | stdio | one person, data on their machine, nothing to host | the client must find `cameleer` or Docker; paths must be absolute |
| a server or VM | Streamable HTTP | a team on one endpoint | add authentication (API keys, JWT, OAuth token checks) before you open the port |
| Docker, Compose, Kubernetes | either | the same image as your other routes, health checks, replicas | legacy sessions live in one replica; share `camel.server.mcp-request-state-key` |
| Cloud Run, Knative, Container Apps | Streamable HTTP | scale to zero; routes run about 3 ms after start | the same as Kubernetes |
| AWS Lambda | Streamable HTTP through a function URL | pay per request, the binary is its own runtime | responses are returned whole: use 2026-07-28 clients |
| an edge gateway | either | tools next to the machines; data stays on site | expose only the tools an agent should have; authenticate remote access |

## Desktop: the client starts the server

With the binary (`cargo install cameleer-main`) on the `PATH`, Claude Desktop's
`claude_desktop_config.json` for the shop:

```json
{
  "mcpServers": {
    "acme-outdoor": {
      "command": "cameleer",
      "args": ["run", "/path/to/mcp-shop/shop.camel.yaml",
               "--properties", "/path/to/mcp-shop/application.properties",
               "--prop", "acme.dir=/path/to/mcp-shop", "--mcp-stdio"]
    }
  }
}
```

The same with Docker, nothing to install but Docker (pass `-i` and no `-t`; logs go to stderr,
so stdout carries only protocol messages):

```json
{
  "mcpServers": {
    "acme-outdoor": {
      "command": "docker",
      "args": ["run", "-i", "--rm", "-v", "/path/to/mcp-shop:/routes:ro", "pmsudhi/cameleer",
               "run", "/routes/shop.camel.yaml", "--properties", "/routes/application.properties",
               "--prop", "acme.dir=/routes", "--mcp-stdio"]
    }
  }
}
```

Claude Code: `claude mcp add acme -- cameleer run /path/to/mcp-shop/shop.camel.yaml --properties /path/to/mcp-shop/application.properties --prop acme.dir=/path/to/mcp-shop --mcp-stdio`.
VS Code and Cursor take the same command in their `mcp.json`
([MCP server: connect a client](../mcp-server.md#connect-a-client)).

## A server, or Docker

```bash
cd examples/agents/mcp-shop
cameleer run shop.camel.yaml --properties application.properties
```

In a container, bind to all interfaces (the example's properties use `127.0.0.1`):

```bash
docker run --rm -p 18190:18190 -v "$PWD:/routes:ro" pmsudhi/cameleer \
  run /routes/shop.camel.yaml --properties /routes/application.properties \
  --prop acme.dir=/routes --prop camel.server.host=0.0.0.0
```

Clients connect to `http://<host>:18190/mcp`. Before the port is reachable by others, turn on
authentication: `camel.server.mcp-auth` with API keys, JWT (JWKS or HMAC) or OAuth token
introspection ([MCP server: authentication](../mcp-server.md#authentication)). A Compose file
with health checks, Kubernetes settings and several replicas are in
[MCP server: run it anywhere](../mcp-server.md#run-it-anywhere).

## Cloud Run and AWS Lambda

On Cloud Run, Knative or Container Apps, build a small image with the route files and run it
with `--mcp` ([the Dockerfile](../mcp-server.md#cloud-run-and-other-serverless-containers)). On
Lambda, function URL and API Gateway requests for `/mcp` reach the MCP server in process; put
`camel.server.mcp-enabled=true` and the tags in `application.properties` and build with
`--features ai` ([MCP on AWS Lambda](../mcp-server.md#on-aws-lambda)). In both, requests of the
2026-07-28 era are stateless, so any instance answers any request; set
`camel.server.mcp-request-state-key` (or `CAMEL_SERVER_MCP_REQUEST_STATE_KEY`) to one key so an
approval started on one instance finishes on another.

## An edge gateway

Tools that read the latest MQTT sensor readings and change device setpoints, with setpoints
above 30 °C waiting for an operator, run on the gateway itself:

```bash
docker run -d --name gw --network host --read-only --tmpfs /tmp -e CAMEL_LOCAL_STATUS=true \
  -e MQTT_BROKER -e GATEWAY_ID -v "$PWD:/routes:ro" pmsudhi/cameleer \
  run /routes/mcp-gateway-tools.camel.yaml --mcp
docker exec gw cameleer tool approve <handle>
```

The route and its test are in the
[edge guide](../edge.md#mcp-tools-on-the-gateway-let-an-agent-read-sensors-and-change-setpoints).

## How these were checked

- **Hermetic tests:** the shop over MCP HTTP (`crates/cameleer-main/tests/mcp_shop.rs`), stdio
  through the real binary (`mcp_main.rs`), MCP requests through the Lambda runtime
  (`lambda.rs`), and the edge example against an embedded MQTT broker (`docs_guides.rs`).
- **By hand, in an image built from the source:** the shop over HTTP and over stdio with
  `docker run -i`, a Compose service with health checks, the Cloud Run Dockerfile, and the edge
  example against Mosquitto 2 including `docker exec ... cameleer tool approve`.
- **Not run:** the `gcloud` and `aws` deploy commands themselves, which need an account.

## Related

- [MCP server](../mcp-server.md), [Deployment guide](../../DEPLOYMENT.md)
- [One gateway for several MCP servers](gateway.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, please
[open an issue](https://github.com/pmsudhi/cameleer/issues/new), or fork the repository, fix
it, add a test, run `cargo test --workspace` and open a pull request.
