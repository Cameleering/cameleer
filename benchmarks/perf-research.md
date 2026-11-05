# Cold start, footprint and throughput: a survey for Cameleer

_Written 2026-11-05 on branch `performance-improvement`. All web sources were accessed on
2026-11-05 unless a different date is given. This is a research note, not a plan of record._

## 0. Why this document exists

Cameleer wants to be the best runtime for FaaS, serverless and edge deployments, and the
integration layer that AI agents call. Its selling point is cold start and efficient compute.
This note surveys what the other runtimes in that space offer, what they publish about
startup, memory and throughput, and which of their techniques a std-only Rust binary can use.

### Where Cameleer stands today

Measured by this project (see `docs/BENCHMARKS.md` and `benchmarks/analysis.md`):

| metric | Cameleer | Apache Camel 4.23 on Temurin 21 | notes |
|---|---|---|---|
| process spawn to CamelContext started | ~10 ms | 751 ms | Windows 11, Ryzen 5 5600X, warm page cache |
| peak RSS | ~9-15 MB | 260-490 MB | same scenarios on both sides |
| throughput | 2.5x-8.4x Camel | 1x | six scenarios; 4x met in three |
| release binary | ~24.6 MB | n/a (JVM + jars) | glibc, thin LTO, `codegen-units=1`, stripped |
| container image | <20 MB compressed | n/a | `gcr.io/distroless/cc-debian12`, linux/amd64 + arm64 |

Caveats about our own numbers:

- The 10 ms figure is on Windows with a warm file cache. It has not yet been measured on Linux,
  in a Lambda execution environment, on Cloud Run, or after dropping the page cache. Every
  platform below reports cold start differently, so we can't yet compare ourselves to their
  published numbers like for like. Section 4.12 covers how to measure.
- Static musl measured 3-17% slower than glibc on the throughput scenarios. The project already
  installs its own thread-caching allocator (`cameleer_core::alloc`), so the remaining gap isn't
  only musl's malloc for small blocks (see 4.1).
- `panic = "abort"` isn't an option: the executor and the HTTP server catch panics in routes
  (comment in the workspace `Cargo.toml`).

### How to read the numbers

Every number below is tagged with one of these labels:

- **[vendor]**: a claim by the project or company that sells the product.
- **[independent]**: a measurement by a third party, with a method you can inspect.
- **[anecdotal]**: a blog post or forum report with little or no method.
- **[not found]**: I looked and couldn't find a primary source.

"Cold start" means different things in different places. The table at the end of section 2
says which definition each number uses.

---

## 1. Integration runtimes and stream processors

### 1.1 Apache Camel on the JVM

- **What it offers:** the reference implementation Cameleer mirrors, with 300+ components, EIPs,
  Java/YAML/XML DSLs, Camel Main, Spring Boot and Quarkus runtimes, and Camel JBang.
- **Startup and memory:** Camel doesn't publish startup or RSS figures for plain Camel Main
  **[not found]**. Our own measurement is 751 ms to a started context and 260-490 MB peak RSS
  on Java 21 with default heap settings **[independent, ours]**. JVM defaults size the heap
  from host memory, so RSS depends heavily on `-Xmx` and the GC chosen. A tuned JVM would come
  in lower, and we haven't measured one.
- **Techniques worth noting:**
  - Camel 3/4 moved a lot of reflection to build time (generated configurers, `@UriParams`
    metadata), which cut startup.
  - Lazy route startup (`autoStartup=false`, `camel.main.lazy-start-producer`). Startup is
    dominated by class loading and JIT warm-up, which Cameleer doesn't have.

### 1.2 Camel Quarkus (JVM and native)

- **What it offers:** Camel components as Quarkus extensions. Quarkus does build-time
  bootstrapping (CDI wiring, config, reflection registration) and can compile to a GraalVM or
  Mandrel native image.
- **Numbers:**
  - Quarkus' own guide says to measure startup as **time to first request**, not the
    framework's self-reported "started in" line, and to measure footprint as **RSS**, not heap
    ([Quarkus: Measuring performance](https://quarkus.io/guides/performance-measure)). This
    is the right method for us too.
  - The quarkus.io home page shows a benchmark graphic, but the figures are only in an image,
    with no date or method in the text **[vendor, not inspectable]**.
  - I found no published Camel Quarkus native startup or RSS numbers **[not found]**.
  - A 2021 Camel blog proof of concept ran Camel routes on Lambda through
    `quarkus-amazon-lambda` and reported init duration "by magnitude faster" in native mode,
    with no exact figure ([camel.apache.org, 2021-08](https://camel.apache.org/blog/2021/08/DeployingCamelRouteInAWSLambda/))
    **[vendor, qualitative]**.
  - An independent 2022 Lambda comparison of GraalVM native images measured about 1.9 s cold
    start for Quarkus and 1.2 s for plain Java
    ([filia-aleks, Medium](https://filia-aleks.medium.com/aws-lambda-graalvm-native-quarkus-vs-plain-java-90176eff515d))
    **[anecdotal]**. Treat it as old.
  - The best current proxy is the daily `maxday/lambda-perf` data: a GraalVM Java 21 native
    hello-world on `provided.al2023` averages **131 ms (arm64) / 154 ms (x86_64)** init and
    about 33-34 MB memory used (see 2.1) **[independent]**.
- **Techniques worth adapting:**
  - **Build-time initialization:** do at build time whatever doesn't depend on the runtime
    environment. Cameleer has no reflection, but parsing, component catalogs and lookup tables
    built at startup are the analogue (see 4.2).
  - **Closed-world dead-code elimination:** native image drops unreachable classes. For us, the
    equivalent is Cargo features plus LTO (see 4.5).

### 1.3 Camel K and Kaoto

- **Camel K** is a Kubernetes operator that builds and runs integrations from source. Red Hat's
  October 2024 digest said **Red Hat Camel K** was approaching end of life, and pointed users to
  Camel Quarkus and to Camel JBang's Kubernetes plugin
  ([Red Hat Developer, 2024-10-16](https://developers.redhat.com/blog/2024/10/16/camel-integration-quarterly-digest-q3-2024)).
  The upstream Apache Camel K project's status is separate, and I didn't verify a formal end
  of life for it.
- **Kaoto** is the visual route designer (a VS Code extension that installs the Camel JBang
  CLI) ([KaotoIO/kaoto](https://github.com/KaotoIO/kaoto-next/)).
- **Startup and memory:** none relevant. Camel K's startup is the startup of the Camel
  runtime underneath it.
- **What to take from them:**
  - Kaoto edits the YAML DSL, which Cameleer already accepts, so Kaoto-authored routes are a
    distribution channel.
  - The move away from operator-built images toward "CLI plus plain Kubernetes manifests" fits
    a single static binary well.

### 1.4 Spring Cloud Function, Spring native and CRaC

- **What they offer:**
  - Spring Cloud Function adapts `java.util.function` beans to Lambda, Azure and HTTP.
  - Spring Boot 3 supports GraalVM native images and, since Spring Framework 6.1 and Boot 3.2,
    CRaC (Coordinated Restore at Checkpoint).
- **Numbers:**
  - Azul's docs say restoring a compressed CRaC checkpoint adds up to 150 ms to startup, and
    claim a memory footprint similar to native image for Spring PetClinic
    ([Azul: CRaC compared to native](https://docs.azul.com/core/crac/crac-native-compared))
    **[vendor]**.
  - InfoQ reported Azul's 2022 figure: time to first operation fell from about 3.9 s to 38 ms
    for a Spring Boot app with CRaC
    ([InfoQ, 2023-06](https://www.infoq.com/news/2023/06/crac-cracks-mainstream-adoption/))
    **[vendor, reported]**.
  - Blog tables putting native at around 80 ms and 75-85 MB give no method **[anecdotal]**.
- **Techniques worth adapting:** CRaC's checkpoint and restore hooks are the same idea as
  Lambda SnapStart's runtime hooks (2.1): close sockets and reseed randomness before the
  snapshot, then reopen and reseed after restore. Cameleer would only need this if a platform
  snapshots it (4.3).

### 1.5 Redpanda Connect (formerly Benthos)

- **What it offers:** a Go single-binary stream processor with YAML pipelines, Bloblang
  mapping, and many inputs and outputs. It's the closest product to Cameleer's
  "YAML integration in one binary" pitch. Redpanda acquired Benthos in May 2024; the core
  engine stays MIT-licensed
  ([SiliconANGLE, 2024-05-30](https://siliconangle.com/2024/05/30/redpanda-acquires-benthos-enhance-data-streaming-platform);
  [Redpanda blog](https://www.redpanda.com/blog/redpanda-connect)).
- **Numbers:**
  - Redpanda cites its smallest user running on "100 millicores and 10 MiB of memory",
    processing hundreds of messages a second, and claims a third of the compute of Kafka
    Connect ([Redpanda blog](https://www.redpanda.com/blog/redpanda-connect)) **[vendor,
    anecdote]**.
  - A 2024 Wikimedia test measured about 143 MB average memory for Benthos in their log
    pipeline, against 319 MB for VarnishKafka and about 40 MB for mtail
    ([Wikimedia Phabricator T360454](https://phabricator.wikimedia.org/T360454))
    **[independent, small]**.
  - Startup time: **[not found]**.
- **Techniques worth adapting:**
  - The `lint` and `blobl` subcommands give fast config validation, which Cameleer already
    has as `cameleer validate`.
  - Build tags produce slimmer distributions (the cloud plugins are separate). This is the
    same idea as feature-gated builds (4.5).

### 1.6 Vector

- **What it offers:** Rust observability pipeline (logs and metrics), from Datadog. It uses
  Tokio, with VRL as its transform language.
- **Numbers:**
  - The results I found conflict, and none is a primary benchmark.
  - An IBM Cloud write-up found Vector's memory lowest under heavy profiles
    ([IBM Cloud, Medium](https://medium.com/ibm-cloud/log-collectors-performance-benchmarking-8c5218a08fea))
    **[anecdotal]**.
  - A 2025 dev.to benchmark found Fluent Bit 3.0 using 40% less memory than Vector 0.40 at
    100k logs/s (210 MB vs 350 MB)
    ([dev.to](https://dev.to/johalputt/benchmark-vector-040-vs-fluent-bit-30-log-processing-throughput-for-100k-logssecond-k73))
    **[anecdotal]**.
  - Vector's own `vector-test-harness` comparison was published years ago. I couldn't find
    current results **[not found]**.
- **Techniques worth adapting:**
  - Vector is the closest Rust peer. It ships per-component Cargo features (`sources-*`,
    `sinks-*`, `transforms-*`), so distributors can build slim binaries. That's the direct
    template for 4.5.
  - It also publishes a component and config schema, which is close to `cameleer schema`.

### 1.7 Fluent Bit

- **What it offers:** a C log and metrics agent (CNCF) for Kubernetes nodes and embedded
  devices.
- **Numbers:**
  - The home page claims a "~450kb minimal footprint" without saying whether that means
    binary size or memory ([fluentbit.io](https://fluentbit.io/)) **[vendor, ambiguous]**.
  - Throughput: see 1.6 **[anecdotal]**.
- **Techniques worth adapting:**
  - CMake options compile plugins in or out.
  - Fixed-size memory buffers with filesystem spill keep RSS bounded under backpressure.
    Bounded buffers are the relevant idea for Cameleer's seda queues and aggregators.

### 1.8 NATS server and JetStream

- **What it offers:** a Go messaging server with a single binary, sub-millisecond pub/sub,
  and JetStream persistence.
- **Numbers:** NATS publishes no official throughput or RSS reference figures. `nats bench` is
  the tool they point to ([Synadia glossary](https://www.synadia.com/glossary/nats-bench))
  **[not found]**. The binary size of about 22 MB comes from an unofficial page **[anecdotal]**.
- **Techniques worth adapting:**
  - A benchmark tool ships inside the product (`nats bench`). `cameleer-bench` could be exposed
    the same way, as `cameleer bench`, so users can measure their own hardware.
  - The default memory-stream caps are a good example of bounded defaults.

### 1.9 Kafka Connect

- **What it offers:** JVM connector framework for Kafka.
- **Numbers:** the only figure I found is Redpanda's relative claim in 1.5 **[vendor]**.
  Kafka Connect doesn't target FaaS, and I found no startup data **[not found]**. I left it
  out of the comparison.

---

## 2. Serverless and edge platforms

### 2.1 AWS Lambda

**Runtime API.** This protocol is what a Cameleer `bootstrap` would implement
([AWS docs: runtime API](https://docs.aws.amazon.com/lambda/latest/dg/runtimes-api.html)).

| call | method and path (prefix `http://$AWS_LAMBDA_RUNTIME_API/2018-06-01`) | notes |
|---|---|---|
| next event | `GET /runtime/invocation/next` | blocks; **do not set a read timeout**; the process may be frozen between calls |
| success | `POST /runtime/invocation/{AwsRequestId}/response` | echo `Lambda-Runtime-Invocation-Id` if received |
| invocation error | `POST /runtime/invocation/{AwsRequestId}/error` | JSON `{errorMessage, errorType, stackTrace}`, header `Lambda-Runtime-Function-Error-Type` |
| init error | `POST /runtime/init/error` | same body; 500 means "exit promptly" |
| SnapStart restore | `GET /runtime/restore/next`, `POST /runtime/restore/error` | only for SnapStart functions |

Headers returned by `next`:

- `Lambda-Runtime-Aws-Request-Id`
- `Lambda-Runtime-Deadline-Ms` (Unix ms)
- `Lambda-Runtime-Invoked-Function-Arn`
- `Lambda-Runtime-Trace-Id` (the runtime should set `_X_AMZN_TRACE_ID`)
- `Lambda-Runtime-Client-Context` and `Lambda-Runtime-Cognito-Identity`
- `Lambda-Runtime-Invocation-Id` (new; a single request ID can have several attempts)

Environment variables:

- `AWS_LAMBDA_RUNTIME_API`: the endpoint.
- `_HANDLER`: the configured handler string.
- `LAMBDA_TASK_ROOT`: the code directory.
- The usual `AWS_REGION` and credential variables.

On `provided.al2023` the entry point is an executable named `bootstrap` at the root of the zip
or image. Lambda Managed Instances accept concurrent `next`/`response` calls up to
`AWS_LAMBDA_MAX_CONCURRENCY`, which matters for a multi-threaded runtime like ours.

**Lifecycle and limits** ([AWS docs: execution environment](https://docs.aws.amazon.com/lambda/latest/dg/lambda-runtime-environment.html)):

- **Init:** limited to **10 s** (extensions, then runtime bootstrap, then static function
  code). On timeout, init is retried at the first invocation under the function timeout.
  Provisioned concurrency, SnapStart and Managed Instances allow longer init (130 s or the
  function timeout, whichever is higher, up to 900 s).
- **Shutdown:** 0 ms with no extensions, 500 ms with an internal extension, 2,000 ms with
  external extensions, then `SIGKILL`.
- **Invoke failures:** an invoke crash resets the environment. The next invocation re-runs init
  as a "suppressed init", and that time shows up inside the invoke duration.
- **INIT billing:** INIT has been billed for all configurations since 2025-08-01. Custom
  runtimes and container images were already billed for INIT
  ([AWS Compute Blog](https://aws.amazon.com/blogs/compute/aws-lambda-standardizes-billing-for-init-phase/);
  [InfoQ, 2025-05](https://www.infoq.com/news/2025/05/aws-lambda-init-phase)). So a 10 ms init
  versus a 250 ms init is now a direct cost difference on every cold start, not only latency.
- **Cold start frequency:** AWS says cold starts occur in under 1% of invocations and range from
  under 100 ms to over 1 s **[vendor]**.

**Memory and CPU.** Memory can be set from 128 MB to 10,240 MB, and CPU scales with it. **At
1,769 MB a function has the equivalent of one vCPU**
([AWS docs: memory](https://docs.aws.amazon.com/lambda/latest/dg/configuration-memory.html)).
At 128 MB a function gets about 7% of a vCPU. That is the regime where a 10 MB-RSS runtime
shines, and also where any CPU-heavy startup work (parsing, table building) costs about 14
times more wall-clock time than on a full core. **Always measure init at 128 MB as well as at
1,769 MB.**

**arm64 (Graviton2).** AWS claims "up to 34% better price performance". The per-GB-second
price is 20% lower than x86 for on-demand and provisioned concurrency
([AWS Compute Blog](https://aws.amazon.com/blogs/compute/migrating-aws-lambda-functions-to-arm-based-aws-graviton2-processors))
**[vendor]**. The 34% combines the price cut with an assumed speed-up, and AWS itself says
results depend on the workload. Cameleer already builds `linux/arm64` images.

**Rust on Lambda.** Rust support moved from "experimental" to **generally available on
2025-11-14**, using the `lambda_runtime` crate on `provided.al2023` or `provided.al2`
([AWS What's New](https://aws.amazon.com/about-aws/whats-new/2025/11/aws-lambda-rust);
[InfoQ, 2025-11](https://infoq.com/news/2025/11/aws-lambda-rust-support-ga/)). We can't use
that crate (std-only rule), but the protocol is simple HTTP/1.1, which `cameleer-http` already
speaks.

**Cold start by runtime.** `maxday/lambda-perf` runs this daily: hello-world functions, zip
packaging, 512 MB, us-east-1, 10 cold starts each, read from the `REPORT` line
([data, metadata date 2026-11-05](https://raw.githubusercontent.com/maxday/lambda-perf/main/data/last.json);
[site](https://maxday.github.io/lambda-perf/)) **[independent]**.

| runtime | arch | avg init (ms) | avg memory used (MB) |
|---|---|---|---|
| Rust, provided.al2023 | arm64 | **17.5** | 13.9 |
| Rust, provided.al2023 | x86_64 | 19.5 | 14.0 |
| Go, provided.al2 | arm64 | 48.3 | 18.0 |
| C++, provided.al2023 | arm64 | 50.4 | 17.0 |
| Python 3.13 | arm64 | 78.1 | 31.2 |
| Node.js 22 | arm64 | 111.7 | 70.2 |
| GraalVM Java 21 native, provided.al2023 | arm64 | 131.0 | 34.0 |
| Java 25 (managed) | x86_64 | 184.4 | 81.6 |
| Java 21 (managed) | arm64 | 234.2 | 94.0 |
| .NET 8 | x86_64 | 236.8 | 65.2 |

Reading this for Cameleer:

- A trivial Rust function already sets the floor at about 17-20 ms. That floor is mostly
  Lambda's own work (fetching and unpacking code, starting the Firecracker sandbox, the
  process `exec`) plus a few ms of runtime.
- Cameleer can't beat it. The realistic goal is to **stay within a few ms of it** with a real
  route loaded, and about 15 MB memory used, which is already our RSS range.
- A 24.6 MB binary is several times larger than a hello-world Rust binary. Package size feeds
  into code download and page-in time (see 2.5 and 4.5).
- Java SnapStart wasn't in the part of the data I could read, and the fork `davo/lambda-perf`
  notes it's no longer benchmarked
  ([github.com/davo/lambda-perf](https://github.com/davo/lambda-perf)).

**SnapStart.**

- Launched for Java (Corretto 11) on 2022-11-28, claiming "up to 10x faster function startup"
  ([AWS What's New](https://aws.amazon.com/about-aws/whats-new/2022/11/aws-lambda-snapstart-java-functions))
  **[vendor]**.
- Python 3.12+ and .NET 8+ followed on 2024-11-18, claiming startup "from several seconds to as
  low as sub-second" for heavy imports
  ([AWS News Blog](https://aws.amazon.com/blogs/aws/aws-lambda-snapstart-for-python-and-net-functions-is-now-generally-available/))
  **[vendor]**.
- For Python and .NET there are charges for snapshot caching and for each restore.
- Restore duration covers loading the snapshot and running the after-restore hooks, and AWS
  reports it separately ([AWS docs: SnapStart monitoring](https://docs.aws.amazon.com/lambda/latest/dg/snapstart-monitoring.md)).
- SnapStart is a feature of the **managed** Java, Python and .NET runtimes. I found nothing
  saying it applies to `provided.*` custom runtimes, so Cameleer can't use it today, and with
  a ~20 ms native init it has little to gain.
- The runtime API does define `/runtime/restore/next`, so a custom runtime could take part if
  AWS ever enables it.

**Container images.** Lambda loads container images lazily, in chunks, with deduplication
across customers (Brooker et al., "On-demand Container Loading in AWS Lambda", USENIX ATC
2023, from memory, not re-fetched for this note). The pages a cold start touches are what get
fetched, so a smaller binary with fewer touched pages starts faster. Zip packages are limited
to 50 MB zipped and 250 MB unzipped (AWS quotas, from memory).

### 2.2 Cloudflare Workers

- **What it offers:** JavaScript and Wasm running in V8 isolates inside a shared process, at
  every Cloudflare location.
- **Numbers:**
  - Cloudflare says isolates avoid per-function VM cold starts, and that "any given isolate can
    start around a hundred times faster than a Node process on a container or virtual machine"
    ([Cloudflare docs: how Workers works](https://developers.cloudflare.com/workers/reference/how-workers-works/))
    **[vendor, no method]**.
  - Cloudflare has also blogged that it warms the isolate during the TLS handshake, which is
    the origin of the "0 ms cold start" claim. I couldn't re-find a primary source with a
    number for this note **[not found]**.
- **Limits** ([Cloudflare docs: limits](https://developers.cloudflare.com/workers/platform/limits/)):
  - Memory: **128 MB per isolate**, including Wasm memory.
  - Startup: global scope must run within **1 s**.
  - CPU per request: 10 ms on the free plan; 30 s by default and up to 5 min on the paid plan.
  - Bundle size: 64 MiB uncompressed.
- **For Cameleer:**
  - Workers can run Wasm modules, so a `wasm32` Cameleer core is plausible in principle.
  - No threads and no raw TCP: outbound traffic goes through `fetch` and the `connect()`
    sockets API, not POSIX. That rules out most of Cameleer's protocol stack (see 4.4).
  - The 128 MB cap is generous for us. The 64 MiB bundle cap is fine.

### 2.3 Fastly Compute

- **What it offers:** Wasm (WASI) at the edge, one fresh instance per request.
- **Numbers:**
  - Marketing: "35.4 microseconds" startup
    ([fastly.com serverless](https://www.fastly.com/edge-cloud-network/serverless)) **[vendor]**.
  - The engineering blog (Lucet era) is more detailed: about 52 µs to load and instantiate a
    WASI hello-world; 30 µs instantiation, 5 µs context switch, 23 µs teardown. Instantiation
    grows linearly with initial heap size, because heap copying dominates
    ([Fastly: Lucet performance and lifecycle](https://www.fastly.com/blog/lucet-performance-and-lifecycle))
    **[vendor, with method]**.
  - Fastly has since moved from Lucet to Wasmtime. I didn't find current numbers
    **[not found]**.
- **Technique:** per-request instantiation is cheap only because the module is compiled ahead
  of time and memory images are mapped copy-on-write (see 2.4). The process model is
  "instantiate per request, no shared mutable state", the opposite of a long-lived CamelContext.

### 2.4 Fermyon Spin, Wasmtime and WASI

- **What it offers:** Spin is a framework for WASI component apps (HTTP triggers, Redis/MQTT
  triggers, key-value). Fermyon Wasm Functions runs on Akamai, which acquired Fermyon in
  2025.
- **Numbers:**
  - Fermyon claims Spin apps "cold start in half of a millisecond" (March 2025) and
    "0.52 milliseconds" on its product page, then "under one millisecond" after the Akamai
    acquisition (December 2025)
    ([Fermyon, 2025-03-27](https://fermyon.com/blog/introducing-wasm-functions);
    [Fermyon showcase](https://fermyon.com/showcase);
    [Fermyon joins Akamai, 2025-12-01](https://www.fermyon.com/blog/fermyon-joins-akamai))
    **[vendor]**.
  - The same author notes that network latency of 300 ms dwarfs a 0.5 ms start, which is a
    fair point for our own messaging too.
- **How Wasmtime makes this possible**
  ([Wasmtime: fast instantiation](https://docs.wasmtime.dev/examples-fast-instantiation.html);
  [Wasmtime 10 performance](https://bytecodealliance.org/articles/wasmtime-10-performance)):
  - Modules are precompiled.
  - A **pooling allocator** pre-reserves slots for instances; it's off by default.
  - **Copy-on-write memory images** turn instantiation into "create a mapping": data is only
    copied when first written.
  - The Wasmtime 10 article reports SpiderMonkey.wasm instantiation falling from about 2 ms to
    microseconds **[vendor, with method]**.
- **Targets:**
  - `wasm32-wasip1` is the stable preview-1 target. `wasm32-wasip1-threads` is tier 2 but
    "in flux".
  - `wasm32-wasip2` is the component-model target. Its networking is better than p1's
    ([rust-lang/rust#129638](https://github.com/rust-lang/rust/pull/129638)), but sockets need
    a host grant: Wasmtime denies outbound connections by default.
  - `std::thread::spawn` isn't available on wasip2. See 4.4 for what breaks.

### 2.5 Firecracker microVMs

- **What it offers:** the KVM-based VMM that runs Lambda and Fargate
  ([Agache et al., NSDI 2020](https://www.usenix.org/conference/nsdi20/presentation/agache)).
- **Numbers**, from the project's own specification
  ([SPECIFICATION.md](https://github.com/firecracker-microvm/firecracker/blob/main/SPECIFICATION.md))
  **[vendor, tested in CI]**:
  - VMM startup to API socket: 8 CPU ms, 6-60 ms wall clock, typically about 12 ms.
  - Guest boot: **≤ 125 ms** from InstanceStart to the guest's `/sbin/init`.
  - VMM memory overhead: ≤ 5 MiB for a 1 vCPU / 128 MiB guest.
  - Guest compute: > 95% of bare metal.
- **Snapshots**
  ([snapshot-support.md](https://github.com/firecracker-microvm/firecracker/blob/main/docs/snapshotting/snapshot-support.md)):
  - Guest memory is restored with a `MAP_PRIVATE` mapping of the memory file, paged in on
    demand, or through a userfaultfd handler.
  - The doc gives no restore latency number. It depends on memory size, vCPU count and device
    count; secondary sources say "low milliseconds" **[not found as primary]**.
  - Caveats for any process inside a snapshot:
    - Cloned VMs share PRNG and application state; VMGenID reseeds only the kernel.
    - The wall clock jumps.
    - Network connections may be lost.
    - Vsock connections are closed.
- **For Cameleer:** inside Lambda, every page our binary touches at startup is potentially a
  lazy fetch from the image or snapshot store. Fewer touched pages means a faster cold start,
  which is the main argument for a smaller binary and lazy initialization.

### 2.6 Google Cloud Run

- **What it offers:** containers that scale to zero, are billed per request or per instance,
  and listen on `$PORT`.
- **Startup CPU boost** gives extra vCPU during instance startup and for 10 s after: a limit of
  1 vCPU is boosted to 2, 2 to 4, and 4 to 8. The boosted CPU is billed. Google says some
  workloads, especially Java, start in half the time
  ([Cloud Run docs: CPU](https://docs.cloud.google.com/run/docs/configuring/cpu);
  [Google Cloud blog](https://cloud.google.com/blog/products/serverless/announcing-startup-cpu-boost-for-cloud-run--cloud-functions))
  **[vendor]**. For a runtime that's ready in 10 ms, the boost has almost nothing to act on.
  That's worth stating in our docs, because it saves users money.
- **Execution environments:** gen1 is based on gVisor, with "fast cold start times" and
  emulation of most syscalls. gen2 is a microVM with full Linux compatibility and better
  sustained performance, but slower cold starts for some services
  ([Cloud Run docs: execution environments](https://docs.cloud.google.com/run/docs/configuring/execution-environments))
  **[vendor]**. Two things for Cameleer:
  - We need to test on gen1, because gVisor syscall coverage may differ for `CancelIoEx`-style
    paths (Linux uses `shutdown` there) and for socket options.
  - gen1 is the recommended choice for scale-from-zero.
- **Min instances** remove cold starts at the cost of always-on billing. With 10 ms startup,
  min instances are rarely worth it for us.

### 2.7 Azure Functions custom handlers

- **What it offers:** the Functions host runs any executable as a "custom handler". The host
  passes the port in `FUNCTIONS_CUSTOMHANDLER_PORT`, and the handler serves HTTP. With HTTP
  request forwarding enabled in `host.json`, HTTP triggers arrive as plain requests. Other
  triggers arrive as a JSON envelope
  ([Microsoft Learn module](https://learn.microsoft.com/en-us/training/modules/serverless-go/3-web-api)).
  One secondary source says the setting is now documented as `enableProxyingHttpRequest`
  rather than `enableForwardingHttpRequest` **[unverified]**.
- **Numbers:** Microsoft publishes no custom-handler cold start figures **[not found]**. The
  host process itself (.NET) starts before our handler, so the platform floor is the host's
  startup, not ours.
- **For Cameleer:** small effort. Listen on the port from `FUNCTIONS_CUSTOMHANDLER_PORT` and map
  the JSON envelope to an exchange.

### 2.8 Knative Serving

- **What it offers:** scale to zero on Kubernetes. When no pods are running, the activator
  buffers requests while the autoscaler creates a pod.
  - `scale-to-zero-grace-period` defaults to 30 s.
  - `scale-to-zero-pod-retention-period` defaults to 0 s.
  - Scale to zero requires the Knative Pod Autoscaler, not the HPA.

  ([Knative docs: scale to zero](https://knative.dev/docs/serving/autoscaling/scale-to-zero/);
  [SYSTEM.md](https://github.com/knative/serving/blob/main/docs/scaling/SYSTEM.md))
- **Numbers:** the dominant cost is pod scheduling plus image pull plus readiness probing,
  usually seconds. Knative publishes no single number **[not found]**.
- **For Cameleer:**
  - The application's 10 ms is lost in Kubernetes overheads.
  - What helps here is a small image (fast pull), a readiness endpoint that turns ready the
    moment the context starts, and a short probe period.

### 2.9 Summary: what "cold start" means in each source

| source | what is measured | headline number | type |
|---|---|---|---|
| Cameleer (ours) | process spawn → context started, warm cache, Windows | ~10 ms | independent (ours) |
| Apache Camel 4.23 / Java 21 (ours) | same | 751 ms | independent (ours) |
| maxday lambda-perf | Lambda `Init Duration` (runtime + static init), 512 MB | Rust 17.5-19.5 ms, Java 21 234 ms | independent |
| Fastly | Wasm instantiate (Lucet era) | 30-52 µs | vendor with method |
| Fermyon Spin | "cold start", undefined | 0.52 ms | vendor |
| Cloudflare Workers | isolate start vs Node process | "~100x faster" | vendor, no method |
| Firecracker | InstanceStart → guest `/sbin/init` | ≤ 125 ms | vendor spec |
| CRaC (Azul) | restore overhead | ≤ 150 ms added | vendor |
| SnapStart | "startup" | up to 10x faster | vendor |

The microsecond numbers are for **instantiating an already-loaded Wasm module inside a running
host process**. They aren't comparable to a process `exec`. A native process can't start in
microseconds: `fork` + `exec` + `ld.so` + page faults cost hundreds of µs to a few ms. So
Cameleer's honest claim is "**milliseconds as a native process; within a few ms of the
platform floor on Lambda**", not microseconds. Microseconds would only come from an
embedded, Wasm or snapshot model.

---

## 3. Comparison at a glance

| product | language / model | startup (best published) | memory (best published) | source quality |
|---|---|---|---|---|
| Cameleer | Rust std-only native | ~10 ms to context ready | 9-15 MB RSS | ours |
| Apache Camel (JVM) | Java | 751 ms (ours) | 260-490 MB (ours) | ours |
| Camel Quarkus native | Java → native image | not found; GraalVM hello-world on Lambda 131-154 ms | ~33 MB (Lambda hello-world) | independent proxy |
| Spring + CRaC | Java, checkpoint/restore | ~38 ms time to first op (2022) | "similar to native" | vendor |
| Redpanda Connect | Go native | not found | 10 MiB (anecdote) to ~143 MB (Wikimedia) | vendor / small independent |
| Vector | Rust (Tokio) | not found | 188-350 MB in log benchmarks | anecdotal |
| Fluent Bit | C | not found | "~450 KB footprint" | vendor, ambiguous |
| NATS | Go native | not found | not found | not found |
| Rust hello-world on Lambda | Rust native | 17.5 ms init | 13.9 MB | independent |
| Spin / Fastly | Wasm in host | 0.03-0.5 ms instantiate | host-dependent | vendor |

---

## 4. Techniques for a std-only Rust binary

Each subsection gives the technique, what the evidence says, and how it applies to Cameleer.
"Std-only" means no crates. Linker flags, `RUSTFLAGS` and Cargo profile settings are allowed.

### 4.1 Static linking: musl vs glibc, static-pie

| option | pros | cons |
|---|---|---|
| glibc dynamic (today) | fastest `memcpy`/`memmove` (ifunc-selected AVX2/AVX-512, SVE), fast malloc for large blocks, full NSS | needs a glibc base image (distroless/cc); `ld.so` symbol resolution at start |
| musl static (`x86_64-unknown-linux-musl`) | `FROM scratch`, no loader, tiny image, no glibc version coupling | malloc contends under threads; simpler `memcpy`; measured 3-17% slower here |
| glibc static / static-pie (`-C target-feature=+crt-static` on `-gnu`) | no loader dependency, keeps glibc's string routines and malloc | NSS (`getaddrinfo` with non-`files` sources, `getpwnam`) needs matching `libnss_*.so` at runtime; glibc officially discourages static linking |

Notes and evidence:

- musl's maintainer attributes most malloc complaints to multithreaded synchronization in
  mallocng, which was designed for low overhead and hardening, not speed
  ([musl list, 2022-09](https://openwall.com/lists/musl/2022/09/19/4)).
- Tweag found musl malloc to be the contention point in a static Wasmtime build and fixed it by
  switching allocators ([Tweag, 2023-08-10](https://tweag.io/blog/2023-08-10-rust-static-link-with-mimalloc))
  **[independent]**.
- Cameleer's allocator already serves blocks ≤ 4 KiB from thread caches, but takes its 16 KiB
  chunks, and every block > 4 KiB, from `System`. Under musl those still hit mallocng's lock.
  The remaining musl gap probably comes from two places:
  - large-block traffic: bodies, `Vec` growth, HTTP buffers;
  - musl's plain `memcpy`, compared with glibc's vectorized one.

  A test that separates the two: raise `MAX_SMALL` to 64 KiB and take chunks from `mmap`, then
  re-run the musl benchmark. If the gap closes, a scratch-image musl build becomes viable.
- Static glibc with static-pie works on stable Rust (rust-lang/rust#77386, #86374). It needs
  `--target x86_64-unknown-linux-gnu` passed explicitly because of proc-macros. `std::net`
  host-name resolution goes through `getaddrinfo`, so for container use either:
  - ship the NSS libraries; or
  - make sure only `files dns` is in use and test it. glibc 2.34+ moved the files/dns NSS
    modules into libc, but static binaries still warn about it.

  This is riskier than it looks. Verify with a DNS-resolving test inside `scratch`.
- Image size: distroless/cc adds glibc, libgcc, CA certificates and tzdata, a few MB
  compressed. A `scratch` image would need to bring CA certificates (we already have a bundled
  tzdata). The saving is real but small next to our 24.6 MB binary.
- **Verdict:** keep glibc as the default. A musl/scratch variant is worth offering only if the
  allocator change closes the gap.

### 4.2 Lazy initialization and on-demand registration

- Cameleer starts in 10 ms, but `cameleer-main`'s `new_main` registers every component factory.
  On a 128 MB Lambda (about 7% of a vCPU), each ms of single-core startup work costs about
  14 ms of wall clock. **Profile startup at low CPU**, for example with
  `taskset` plus cgroup `cpu.max=7000 100000` on Linux.
- Patterns that cost nothing std-only:
  - Register **factories** as `fn` pointers in a `static` sorted slice, and look them up by
    scheme with binary search. Build no `HashMap` at startup and allocate nothing until a
    scheme is used.
  - Use `std::sync::OnceLock` / `LazyLock` (stable since 1.70 / 1.80) for tables: Simple
    language operator tables, the MIME map, the tz database index, the TLS cipher suite tables,
    the XML entity tables.
  - Don't parse the bundled `zones.bin` or the CA bundle until first use. Today's ordering
    probably already does this; verify with a startup trace.
  - Start no threads at startup that the routes don't need: seda consumers, timers, the
    management/JMX-like services, the status file writer (`CAMEL_LOCAL_STATUS=false` already
    exists for containers).
- Quarkus's lesson applies directly: move work to build time. For us, that means `const`
  tables and `include_bytes!` data that's already in the final layout, not parsed at startup.

### 4.3 Snapshot and restore: CRIU, Firecracker snapshots, SnapStart

- These exist to amortize **seconds** of JVM, Python or .NET initialization. Restore isn't
  free: Firecracker pages memory in lazily, CRaC adds up to 150 ms, and SnapStart charges
  restore time and caching for Python/.NET.
- For a process that's ready in 10-20 ms, restoring a snapshot is unlikely to be faster than
  starting fresh, and it brings the uniqueness hazards from 2.5: PRNG state, TLS session keys,
  cached time, open sockets.
- The useful parts to borrow are cheap:
  1. A clean **"before checkpoint / after restore" hook point** in `CamelContext`. On
     suspend, close pooled connections. On resume, reseed the `cameleer-tls` RNG from the OS,
     re-read the clock and reconnect. This is also what Lambda's freeze/thaw needs: a process
     frozen for minutes holds dead TCP connections.
  2. Detecting a restore (VMClock `vm_generation_counter`, or Lambda's `/runtime/restore/next`)
     if a platform ever snapshots us.
- **Verdict:** don't build snapshot support. Do add freeze-aware connection pools: validate a
  connection before use if it has been idle longer than N seconds.

### 4.4 WebAssembly targets: what breaks in std

| std area | `wasm32-wasip1` | `wasm32-wasip2` | impact on Cameleer |
|---|---|---|---|
| `std::thread::spawn` | unsupported (`wasip1-threads` target exists, "in flux") | unsupported | seda, thread pools, timers, aggregator timeouts, the HTTP server's thread-per-connection all need a single-threaded executor |
| `std::net` | no `connect`; only preopened listeners | TCP/UDP through WASI sockets; host must grant network access | every protocol client works only on hosts that allow sockets; edge platforms (Workers, Fastly) mostly don't offer raw sockets |
| `std::time::Instant` / `SystemTime` | works | works | the TSC tick clock (`rdtsc`) won't compile; needs a cfg fallback |
| `std::fs` | preopened directories only | same | file component OK under a preopen |
| `std::process` | unsupported | unsupported | `exec` component out |
| `std::sync` (Mutex, Condvar) | compiles; blocking without threads is a deadlock risk | same | park/unpark-based seda can't work |
| allocator | `System` is dlmalloc in wasi-libc | same | `CamelAllocator` thread-locals are fine single-threaded |
| stack checks | no native stack bounds | same | `cameleer_core::stack` needs a wasm path |

- Realistic scope: a **`cameleer-core` subset** (DSL, EIPs, Simple/JSONPath/XPath, data
  formats, direct/log/transform) compiled to `wasm32-wasip2`. It would run as a single
  exchange per invocation, exported through `wasi:http/incoming-handler`. Exporting a
  component handler without crates (no `wit-bindgen`) means writing the canonical ABI glue by
  hand. That's a large job, and it's the main obstacle under the std-only rule.
- Payoff: a Spin, Fastly or Wasmtime-hosted integration step with microsecond instantiation.
  That would be the only route to a real "microsecond cold start" claim.
- **Verdict:** long-term research item. High effort, high risk, but a unique position: there's
  no "Camel in Wasm" today as far as I found.

### 4.5 Binary size

The workspace builds release binaries with the `dist` profile: thin LTO,
`codegen-units = 1`, `strip=symbols` set in the Dockerfile `RUSTFLAGS`, and
`panic = "unwind"` (required). Options, roughly in order of payoff for us:

| technique | stable? | expected effect | notes for Cameleer |
|---|---|---|---|
| **Cargo features per component family** (`aws`, `azure`, `gcp`, `kafka`, `amqp`, `sql`, `xml`, `script`, ...) in `cameleer-main` | yes | largest: a Lambda build with http+json+aws-sqs might be a fraction of 24.6 MB | Vector does this (1.6). Cost is `#[cfg(feature)]` around registrations in `new_main` plus CI for the default "all" build |
| fat LTO (`lto = "fat"`) | yes | a few % smaller and sometimes faster than thin | longer link; measure both |
| per-crate `opt-level = "s"` for cold crates (`[profile.dist.package.cameleer-saas] opt-level = "s"`) | yes | smaller code for rarely-hot crates (cloud SDK clients, SOAP, PGP, script interpreters) without slowing the routing core | keep `cameleer-core`, `-http`, `-tls` at `3` |
| `opt-level = "z"` globally | yes | smallest, but slower | not for the hot path; we sell throughput too |
| `-C link-arg=-Wl,--gc-sections` | default on Linux for Rust | already applied | nothing to do |
| `strip = true` in profile | yes | same as current `RUSTFLAGS` | move into `[profile.dist]` for consistency |
| `-Wl,-z,pack-relative-relocs` (DT_RELR) | yes (needs glibc ≥ 2.36 and binutils ≥ 2.38 or lld) | shrinks `.rela.dyn`; for a large PIE often several hundred KB to MB | debian12's glibc is 2.36, so supported; see 4.10 |
| `panic = "abort"` | yes | removes landing pads, about 5-10% | **not possible**: route panics are caught with `catch_unwind` |
| `-Z build-std` + `panic_immediate_abort`, `-Zfmt-debug=none`, `-Zlocation-detail=none` | **nightly only** | large on tiny programs ([min-sized-rust](https://github.com/johnthagen/min-sized-rust)) | ruled out (stable toolchain) |
| removing `core::fmt` use in hot paths | yes | small for size, can help speed | `format!` in error paths is fine; avoid it in per-exchange paths |
| UPX | n/a | 50-70% smaller file | **don't**: decompression adds startup time and RSS, defeats page sharing, and antivirus tools flag it |
| generic monomorphization audit (`cargo llvm-lines` is a crate, so use `-Z print-type-sizes` (nightly) or `nm --size-sort`) | partly | finds bloat hot spots | `nm -S --size-sort target/dist/cameleer` works without crates |

Size matters for FaaS in three ways:

- Lambda zip download and unpack.
- Image pull on Knative or Cloud Run (compressed size).
- Page faults during startup, for the parts touched.

Untouched code costs nothing at runtime, beyond file size, because it's never paged in. So
feature gating matters for packaging. Lazy init (4.2) matters for touched pages.

### 4.6 Allocators and RSS

- glibc malloc creates up to **8 arenas per core** on 64-bit. Arenas retain freed memory, so a
  many-threaded process can grow RSS. Cloudflare saw 60-80 arenas for about 25 threads, and
  the common fix is `MALLOC_ARENA_MAX=2`
  ([glibc manual: tunables](https://www.gnu.org/software/libc/manual/2.30/html_node/Memory-Allocation-Tunables.html);
  [Cloudflare blog](https://blog.cloudflare.com/the-effect-of-switching-to-tcmalloc-on-rocksdb-memory-use);
  [Heroku](https://devcenter2.assets.heroku.com/articles/tuning-glibc-memory-behavior)).
- In Cameleer, small blocks bypass glibc (`CamelAllocator`), so glibc only sees 16 KiB chunks
  and blocks > 4 KiB. Arena growth comes from threads allocating large bodies. Setting
  `MALLOC_ARENA_MAX=2` (or `GLIBC_TUNABLES=glibc.malloc.arena_max=2`) in the container image
  `ENV` is a one-line experiment: measure RSS and throughput under the splitagg and HTTP
  scenarios.
- `CamelAllocator` never returns small blocks to the OS. For a Lambda that lives for hours,
  that's fine. For an always-on Cloud Run instance, a burst leaves RSS high. Options:
  - a per-thread cap: already exists via `bin_cap`;
  - an idle trim that calls `malloc_trim(0)`. That's a glibc extension, callable via
    `extern "C"`, which is still std-only. Do it when the context has been idle for N seconds.
- Thread caches multiply by thread count. Fewer threads (4.8) means less cached memory.

### 4.7 I/O models: thread-per-connection, epoll, io_uring, IOCP

- Today: blocking sockets, with thread-per-connection on the HTTP server. Our own
  measurements: a do-nothing server reaches about 400k req/s with the shared load generator
  on this machine, Cameleer about 70% of that, and an IOCP server measured no faster
  (`benchmarks/analysis.md`).
- In FaaS, concurrency per instance is **1** on default Lambda, and modest on Cloud Run
  (default 80). Thread-per-connection is a good fit there: no event loop cost, simple
  blocking code, and the threads are already created.
- **epoll** (via `extern "C"` declarations, std-only) helps with many idle connections:
  WebSocket, long-poll, MQTT servers. That isn't the FaaS case.
- **io_uring:**
  - Docker 25+ blocks `io_uring_*` in its default seccomp profile, following containerd
    ([moby/moby#46762](https://github.com/moby/moby/pull/46762);
    [Docker seccomp docs](https://docs.docker.com/engine/security/seccomp.md)). Software
    without a fallback fails there; TigerBeetle is the example cited.
  - gVisor (Cloud Run gen1) and Lambda's sandbox support for io_uring is doubtful
    **[not verified]**.
  - **Verdict:** don't depend on io_uring. If it's ever added, it must be optional, with an
    epoll/blocking fallback chosen at runtime.
- **Verdict:** keep thread-per-connection as the default. Make thread counts and stack sizes
  configurable (4.8). An event-driven HTTP server remains the owner's decision; it doesn't help
  FaaS.

### 4.8 Page faults and RSS: threads, stacks, statics

- Each `std::thread` reserves 2 MiB of stack by default (`RUST_MIN_STACK` or
  `thread::Builder::stack_size`). That's virtual memory. RSS only grows by the pages touched,
  but deep recursion (XPath, script interpreters, the regex engine) touches more.
- Each thread also costs a TLS block, `CamelAllocator` caches, and a guard page. On Lambda at
  128 MB, fewer threads also means less contention for a fraction of a CPU.
- Actions:
  - Audit how many threads exist after `start()` for a minimal route, on Linux, with
    `ls /proc/<pid>/task | wc -l`. Each should be justified.
  - Use explicit, smaller stacks (for example 256 KiB) for threads with bounded depth: seda
    consumers, timers, HTTP workers. Keep large stacks for script and XSLT execution, which
    `cameleer_core::stack` already guards.
  - Use lazy thread pools: create workers on first use, not at context start.
  - `static` data in `.rodata` is shared and paged in on demand. Prefer it to building `Vec`s
    at startup.

### 4.9 Zero-copy I/O

- `std::io::copy` on Linux already specializes to `copy_file_range`, `sendfile` and `splice`
  between files, sockets and pipes, where it can (std's `kernel_copy`). So file → socket
  transfers in the HTTP server and file component get zero-copy for free **if** they use
  `io::copy` on the raw `File`/`TcpStream`, not through `BufReader` or the TLS stream. TLS
  rules out sendfile, unless kernel TLS is used (`setsockopt(SOL_TLS)`, which `extern "C"`
  can reach, but that's a large change).
- Shared buffers: a `Bytes`-like type (`Arc<[u8]>` plus offset and length) lets multicast,
  wire-tap and split share one body without copying. That matches the splitagg finding that
  per-part copies dominate (`benchmarks/analysis.md`). Std-only implementation is easy. The
  cost is an API change to `Message` bodies.

### 4.10 Relocations and dynamic loading: PIE, RELRO, `-z now`, DT_RELR

- Rust linux targets default to **PIE** and **full RELRO** (`-z relro -z now`), from rustc's
  target spec (from memory, not re-verified for this note). Full RELRO means all symbol binding
  happens at load time, not lazily, and the GOT is then made read-only.
- For a 24.6 MB PIE, the cost at load is mostly **relative relocations**: every pointer in
  static data (vtables, `&'static str` in tables, function-pointer registries) needs a
  `R_*_RELATIVE` fix-up. That fix-up writes, and so dirties, the page holding it. That's both
  startup time and private RSS.
- How to measure:
  - `readelf -r target/dist/cameleer | grep -c RELATIVE` gives the relocation count.
  - `LD_DEBUG=statistics ./cameleer version` shows the relocation time reported by `ld.so`.
- Options:
  - **DT_RELR** (`-C link-arg=-Wl,-z,pack-relative-relocs`) encodes relative relocations in a
    compact bitmap. It shrinks the file, and loads faster because it reads less. Supported by
    glibc 2.36+, so OK on distroless debian12. Low risk, one flag.
  - **Non-PIE** (`-C relocation-model=static`) removes relative relocations entirely, but loses
    ASLR for the main binary. That's a security trade-off we shouldn't make by default.
  - **static-pie** (4.1) still self-relocates at start, so it doesn't save the relocation work.
  - Fewer pointer-rich statics: index-based tables instead of `&'static str` arrays. That's
    usually a micro-optimization, so only pursue it if measurement shows relocation is
    significant.
- Lazy binding (`-z lazy`) would defer symbol resolution. We link only libc, libm, libgcc_s and
  ld.so, so there are few dynamic symbols, and the gain is negligible next to the RELRO
  hardening lost. Keep `-z now`.

### 4.11 Throughput techniques already in place, or still open

These are already done and documented in `benchmarks/analysis.md`:

- the thread-caching allocator;
- lazy ids, vector headers, copy-on-write maps;
- the TSC clock and `RoutesOnly` statistics;
- the two-lock seda queue.

Still open, and relevant to FaaS costs (GB-seconds):

- shared JSON keys;
- a smaller `Exchange`;
- shared body buffers (4.9).

On Lambda, billed duration is wall clock times memory. At 128 MB and 7% of a vCPU, CPU
efficiency per exchange translates almost linearly into cost.

### 4.12 Measuring cold start properly

The 10 ms number needs a Linux companion. Proposed protocol for a `scripts/coldstart.sh`:

1. **Phases**, timed separately:
   - `exec` → `main()`: loader and relocations. Use an `eprintln!` with
     `CLOCK_MONOTONIC` at the top of `main`, compared with the parent's timestamp taken before
     `spawn`.
   - `main()` → context started (READY).
   - READY → first exchange completed: lazy init that was deferred.
2. **Cache state**, measured both ways:
   - **Warm:** binary in the page cache. Run it 20 times; report median and p99.
   - **Cold:** `sync; echo 3 > /proc/sys/vm/drop_caches` before each run (needs root), or a
     fresh copy of the binary to a new path on a tmpfs, then `vmtouch -e`. Report separately.
     Cold numbers are what a fresh Lambda or Knative node sees.
3. **CPU constraint:**
   - unconstrained;
   - 1 vCPU (`taskset -c 0`);
   - 128 MB-Lambda-equivalent: a cgroup v2 `cpu.max = "7200 100000"` (about 1/14 of a core).
4. **Counters:**
   - `perf stat -e page-faults,minor-faults,major-faults,task-clock`;
   - peak RSS from `/usr/bin/time -v`;
   - thread count.
5. **On platform:**
   - Lambda `REPORT` `Init Duration` and `Max Memory Used` at 128, 512 and 1,769 MB, arm64 and
     x86_64, 10+ forced cold starts each. Change an environment variable to force a new
     environment, as maxday does. Compare with the Rust hello-world row in 2.1.
   - Cloud Run gen1 vs gen2: time from deploy-ready to first 200, with min instances = 0.
6. **Report like Quarkus:** time to first successful request, and RSS (not heap).

---

## 5. What Cameleer should adopt (prioritized)

Ranked by expected impact on FaaS adoption: cold start, then image size, then RSS, then
throughput. Effort: S = days, M = 1-3 weeks, L = more. Every item is feasible std-only unless
noted.

| # | item | main impact | effort | risk | std-only? |
|---|---|---|---|---|---|
| 1 | **Lambda custom runtime built into the binary**: `cameleer` acts as `bootstrap` when `AWS_LAMBDA_RUNTIME_API` is set. Add a `aws-lambda:` consumer (or `lambda:` route `from`) that loops `GET /next` (no read timeout), maps the event and headers to an exchange, and posts `/response` or `/error`, plus `/init/error` on startup failure. Handle the deadline header (`Lambda-Runtime-Deadline-Ms` → exchange timeout) and the invocation ID header. Publish arm64 and x86_64 `provided.al2023` zips from CI. | makes the cold start USP usable on the largest FaaS platform; without it, the 10 ms is theoretical | M | low; the protocol is small and documented; test against an embedded fake runtime API (hermetic) | yes (`cameleer-http` client) |
| 2 | **Linux cold-start measurement harness** (4.12) and published numbers next to `maxday/lambda-perf`'s Rust row | credibility; finds the real startup costs (relocations, page faults, thread spawns) before optimizing | S | none | yes (script plus `perf` if present) |
| 3 | **Feature-gated slim builds**: Cargo features per component family in `cameleer-main`, a `default = ["all"]`, and a documented `--no-default-features --features lambda,http,json,aws` recipe. Optionally ship a `cameleer-slim` artifact. | image and zip size (24.6 MB → likely single-digit MB for typical functions); fewer pages to fetch on Lambda's lazy loader | M | medium: feature-combination CI matrix; registration code must compile under every subset | yes |
| 4 | **Startup work audit at low CPU**: static factory tables, `LazyLock` for parse-on-start data, no threads before first use, `CAMEL_LOCAL_STATUS=false` semantics as the FaaS default | init time at 128 MB (14x CPU penalty), RSS | S-M | low | yes |
| 5 | **DT_RELR and relocation check**: add `-C link-arg=-Wl,-z,pack-relative-relocs` to the Linux dist build after confirming the linker version in the build image; measure relocation count and `LD_DEBUG=statistics` before and after | load time, file size, dirty pages | S | low (glibc ≥ 2.36 at runtime; distroless debian12 has 2.36; the Lambda AL2023 glibc is newer) | yes (linker flag) |
| 6 | **Thread and stack discipline**: count threads for a minimal route; smaller explicit stacks for bounded-depth workers; lazy pools; configurable HTTP worker count | RSS, startup | S | low (keep large stacks where recursion is deep) | yes |
| 7 | **Per-crate size profile**: `opt-level = "s"` for cold crates (cloud, saas, soap, script, dataformats), keeping core, http and tls at 3; trial fat LTO; move `strip` into `[profile.dist]` | binary size with no hot-path cost | S | low; verify with `scripts/bench.sh` | yes |
| 8 | **Freeze-aware connection pools**: validate or reconnect pooled connections after idle time; a context-level "resumed" hook that reseeds the TLS RNG and refreshes clocks | correctness on Lambda thaw and any future snapshot platform | S-M | low | yes |
| 9 | **Cloud Run and Azure entry points**: honour `$PORT` and `FUNCTIONS_CUSTOMHANDLER_PORT` for the platform-http consumer; document "startup CPU boost not needed"; test on Cloud Run gen1 (gVisor) | adoption on two more FaaS platforms | S | low | yes |
| 10 | **Allocator RSS hygiene**: experiment with `MALLOC_ARENA_MAX=2` in the image; idle `malloc_trim(0)`; take allocator chunks via `mmap` and raise `MAX_SMALL` | RSS on long-lived instances; may close the musl gap | S-M | low to medium (benchmark both scenarios) | yes (`extern "C"` libc calls) |
| 11 | **Static musl / scratch variant**, only if #10 closes the 3-17% gap | image size (no distroless base), no glibc coupling | M | medium: NSS/DNS behaviour, CA bundle in image | yes |
| 12 | **Shared body buffers** (`Arc<[u8]>` slices) and shared JSON keys | throughput, so GB-seconds | M-L | medium (public API change) | yes |
| 13 | **`wasm32-wasip2` core subset** exported as a `wasi:http` handler (single-threaded executor, no seda, no TSC clock) | the only path to microsecond-scale instantiation and edge platforms (Spin, Fastly, Wasmtime hosts) | L | high: hand-written component ABI glue without `wit-bindgen`; large cfg surface | yes in principle; painful |
| 14 | Snapshot/restore support (CRIU, SnapStart-style) | little: native init is already ~20 ms on Lambda | M | medium (uniqueness bugs) | n/a; **don't do** |
| 15 | io_uring backend | little for FaaS; blocked by Docker's default seccomp | L | high | yes via raw syscalls; **don't do** now |

### Expected outcome if items 1-7 land

These are estimates, not measurements:

- **Init:** Lambda `Init Duration` within a few ms of the 17-20 ms Rust floor for a route using
  http + json + one AWS component.
- **Memory:** about 14-16 MB memory used, the same as the hello-world Rust row.
- **Package:** a zip package well under 10 MB.

That would let the project say, with a citation, that a full integration runtime starts about
as fast as an empty Rust function, more than 10x faster than managed Java and about 7x faster
than a GraalVM native hello-world on the same platform. That claim is stronger and more honest
than "microsecond cold start", which only a Wasm build (item 13) could support.

---

## 6. Open questions and sources not found

- No primary benchmark found for: Camel Quarkus native startup or RSS; Redpanda Connect, NATS
  or Vector startup; Azure custom handler cold start; Firecracker snapshot restore latency;
  Cloudflare's TLS-handshake pre-warming figure.
- Not re-verified for this note, and stated from memory: Rust's default PIE and full-RELRO
  link settings; Lambda zip size quotas; the ATC 2023 container-loading paper's details; the
  glibc/binutils versions needed for DT_RELR. Check these before quoting them externally.
- Not checked: whether SnapStart can ever apply to `provided.*` runtimes; io_uring availability
  inside Lambda and gVisor; the current name of the Azure HTTP forwarding setting.
- Our own Windows numbers need Linux and on-platform counterparts (item 2) before any public
  comparison with the table in 2.9.
