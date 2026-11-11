# Cameleer and SAP Integration Suite: capability map and gap analysis

_Researched 2026-11-11 against Cameleer `main` at `6a8acca` and SAP's documentation as of
software increment 2608 (August 2026). Internal document, not for the site._

## 1. Why this matters, and what "replace Camel" means

SAP Cloud Integration (CPI), the core of SAP Integration Suite, is built on Apache Camel. The
same runtime runs in SAP's cloud tenants and in Edge Integration Cell, the version customers run
on their own Kubernetes. SAP's documentation shows that:

- The current runtime (Cloud Integration 8.x on Cloud Foundry, 7.x on Neo) is pinned to
  **Camel 3.14**. SAP back-ports security fixes to its own "Camel 3.14.7 branch".
- The older **Camel 2.x** runtime (6.x) is still patched in 2026.
- Workers run on Java 21, with a Java 17 fallback on Edge Integration Cell.
- SAP has published no plan for Camel 4.
- New "Upgrade Readiness" checks tell customers to stop using `org.apache.camel.*` internals,
  OSGi, Spring, Quartz and Saxon classes in scripts. That reads like groundwork for changing the
  runtime under customer content.

Integration Suite is much more than the Camel runtime, so this document separates three possible
proposals:

| Proposal | What Cameleer replaces | What SAP keeps | Gap size |
|---|---|---|---|
| **A. Runtime engine** | The Camel 2.x/3.14 worker that executes deployed iFlows, in the cloud and in Edge Integration Cell | Web UI, iFlow designer, mapping editor, monitoring UI, Integration Advisor, TPM, API Management, Event Mesh, Open Connectors | Large but bounded. **This is the realistic pitch.** |
| **B. Edge / hybrid runtime** | Only the Edge Integration Cell worker, where memory, footprint and the customer's Kubernetes bill matter most | Everything else | Same as A, with fewer adapters needed on day one |
| **C. Whole suite** | All of Integration Suite | Nothing | Very large. Most of the suite is design-time tooling and SaaS that has nothing to do with Camel. Not recommended. |

**Acceptance test for A and B:** SAP's own prepackaged content (Business Accelerator Hub, 3,400+
integrations) and a sample of real customer iFlows must deploy and run on Cameleer unchanged,
with the same message processing log (MPL), the same headers and the same results. Every gap below
is judged against that bar.

**Status legend:**
- ✅ Cameleer has it.
- 🟡 Partial: Cameleer has the Camel equivalent, but not SAP's semantics, or something is missing.
- ❌ Missing.
- ➖ Not a runtime concern: SAP design-time, UI or platform service. Out of scope for A and B.

## 2. Headline findings

1. **Nothing in Cameleer is SAP-specific today.** There is no IDoc, RFC, XI, OData, HANA,
   SuccessFactors, Ariba, AS2, AS4 or EDI support, and nothing reads iFlows. A repo-wide search
   found SAP named only as a competitor in marketing notes.
2. **The generic Camel layer is strong.** Almost every CPI flow step corresponds to a Camel EIP that
   Cameleer already implements: router, multicast, splitter, aggregator, enrich and pollEnrich,
   idempotent consumer, loop, encoders, PGP, XML signature, WS-Security, and the JSON/XML/CSV
   converters. Groovy and JavaScript run on Cameleer's own interpreters.
3. **The blockers fall into six groups:**
   - **(a) Reading SAP content:** importing iFlows, the Groovy `Message` API and `ITApiFactory`
     services, the graphical message-mapping runtime, XSLT 3.0.
   - **(b) SAP protocols:** IDoc, RFC, XI, SOAP SAP RM, OData V2/V4.
   - **(c) Runtime services CPI provides:** MPL, persistent JMS queues, data store, variables,
     number ranges, partner directory, value mapping, secure store, Cloud Connector.
   - **(d) Enterprise databases:** HANA, Oracle, SQL Server, DB2. Cameleer speaks only PostgreSQL.
   - **(e) B2B:** CMS/S/MIME, AS2, AS4, OFTP2, EDI.
   - **(f) Security assurance:** Cameleer's TLS, X.509, SSH and crypto are written from scratch and
     have never been audited. SAP will not run customer traffic on that without an independent
     audit.
4. **Java is the hard limit.** CPI content can include Java: custom adapters built with SAP's
   Adapter Development Kit (ADK) as Java/OSGi Camel components, ESR "imported archives" and
   function libraries, and XSLT calling Java functions. A JVM-free runtime cannot run these
   natively. Section 6.3 gives the options.
5. **The case for Cameleer is strong where SAP hurts:**
   - **Footprint:** up to 54× less memory than Camel on the JVM, 3 ms startup against 1.1 s, and
     1.7–8.7× the throughput (`docs/BENCHMARKS.md`). That matters for Edge Integration Cell sizing
     and for tenant density in SAP's own cloud.
   - **Current Camel semantics:** Cameleer follows Camel 4, while SAP maintains a Camel 3.14 fork.
   - **No third-party supply chain:** the std-only rule means no external crates, so no Log4Shell
     class of exposure and no back-porting of CVE fixes.
   - **MCP and AI:** a mature MCP server and client (official conformance 39/39 and 26/26), while
     SAP is just making MCP servers first-class on Integration Cell.

## 3. Capability map, area by area

### 3.1 Cloud Integration flow steps

| SAP flow step | Cameleer equivalent | Status | Gap |
|---|---|---|---|
| Start/End Message, Terminate, Error Start/End, Escalation events | Route start and end, `stop`, `onException`, `throwException` | 🟡 | No mapping of BPMN events to MPL statuses (ESCALATED, FAILED and so on) |
| Timer Start (basic and advanced schedules) | `timer`, `scheduler`, `cron`, `quartz` | ✅ | SAP's schedule model has to be translated to cron |
| Integration Process, Local Integration Process, Process Call | Routes, `direct` | ✅ | Only the translation is needed |
| Exception Subprocess | `onException` / `doTry` per route | ✅ | Translation |
| Transaction handling (JDBC, JMS, none) | `transacted`, `JdbcTransactionPolicy` | 🟡 | No JMS transaction manager and no XA. The JDBC side is PostgreSQL only |
| Content Modifier (Constant, Header, Property, XPath, Expression, Global/Local Variable, Number Range, External Parameter) | `setHeader`, `setProperty`, `setBody`, `setVariable` | 🟡 | Global and local **persistent** variables, number ranges and externalized parameters are missing (see 3.4) |
| Converters JSON↔XML, CSV↔XML | `xj`, `jacksonXml`, `csv` | 🟡 | SAP's JSON↔XML conventions (namespace mapping, arrays, streaming) must be matched byte for byte |
| EDI↔XML (X12, EDIFACT, VDA, PI XSD), EDI Extractor | none | ❌ | No EDI parser or serializer |
| XML Modifier, Filter (XPath, typed result), Message Digest | `transform`, `filter`, `xpath`, crypto digest | 🟡 | The exact semantics of Filter's typed output and Message Digest need small adapters |
| Encoder/Decoder: Base64, GZIP, ZIP, MIME multipart | `base64`, `gzip`, `zipFile`, `mimeMultipart` | ✅ | Check that headers inline matches SAP |
| Script: Groovy (1.1 legacy, 2.0 = Groovy 4.0.29), JavaScript | `cameleer-script` Groovy and JS interpreters | 🟡 | **SAP Script API missing**: `com.sap.gateway.ip.core.customdev.util.Message`, `ITApiFactory`, `SecureStoreService`, `ValueMappingApi`, `PartnerDirectoryService`, `messageLogFactory`. Customer scripts also use JDK classes and libraries such as `XmlSlurper`, `groovy.xml.*`, `java.time`, `java.util.zip` and BouncyCastle; how much the interpreter covers has to be measured on real scripts |
| Message Mapping (graphical: XSD, WSDL, EDMX, JSON; function library; Groovy UDFs; contexts; multi-mapping) | none | ❌ | A runtime for SAP's `.mmap` format, including queue and context semantics, the standard functions, UDFs and ESR function libraries |
| XSLT mapping (1.2 → XSLT 3.0, Java extension calls) | `xslt` (1.0), `xslt-saxon` (2.0 core) | 🟡 | XSLT 3.0 missing (`xsl:iterate`, `xsl:try`, maps, `xsl:merge`, streaming); Java extension calls impossible |
| Operation Mapping, ID Mapping, Value Mapping | none | ❌ | All three need runtimes. Value mapping is the most used |
| Router (XPath or Simple-subset conditions, default route) | `choice` | ✅ | Translation of SAP operators such as `in` and `regex` to Simple |
| Multicast (parallel or sequential) | `multicast` | ✅ | |
| General and Iterating Splitter (XPath, line, token, grouping, streaming, parallel) | `split` with tokenize and xpath | 🟡 | **No streaming XML split**: XPath splitting builds the full DOM, and tokenize returns all tokens at once. Big files will run out of memory |
| IDoc, EDI, PKCS#7/CMS, Tar and Zip splitters | `tarFile` and `zipFile` splitting | 🟡 | IDoc, EDI and CMS splitters missing |
| Gather and Join (combine, multimapping format, concatenate, zip) | `aggregate` with a strategy | 🟡 | SAP's multimapping envelope (`ns0:Messages/ns0:Message1`) is a strategy to write |
| Aggregator (XPath correlation, combine in sequence, persistent repository) | `aggregate` with JDBC and Redis repositories | 🟡 | Background recovery is missing; the repositories need a HANA or SAP-database backend |
| Request Reply, Send | `to` with InOut and InOnly | ✅ | |
| Content Enricher (combine or key-based enrich), Poll Enrich | `enrich`, `pollEnrich` | 🟡 | SAP's key-based XML merge needs its own strategy |
| Looping Process Call | `loop` with doWhile | ✅ | |
| Idempotent Process Call (DB or in-memory) | `idempotentConsumer` (memory, JDBC, Redis) | ✅ | Also sets `CamelDuplicateMessage` |
| Data Store Write/Get/Select/Delete, Data Store sender | none | ❌ | No persistent data store |
| Write Variables (local or global, 400 days) | Exchange variables only | ❌ | Persistent and global variables missing |
| Persist (message store, 30 days) | none | ❌ | |
| Number Ranges | none | ❌ | |
| Simple Signer, PKCS#7/CMS Signer, Encryptor, Decryptor, Verifier | none | ❌ | No CMS/PKCS#7 |
| XML Digital Signature (including XAdES-BES/EPES), XML Signature Verifier | `xmlsecurity-sign`, `xmlsecurity-verify` | 🟡 | XAdES, `ds:Manifest`, external references and XSLT transforms missing |
| PGP Encryptor and Decryptor | `pgp` | ✅ | CAST5, IDEA and BZip2 are missing, which matters for old partner keys |
| XML Validator (XSD, dynamic from the partner directory) | `validator` | 🟡 | The XSD 1.0 support is a subset |
| EDI Validator, functional acknowledgements (997, 999, CONTRL) | none | ❌ | |
| JMS retry pattern (`SAPJMSRetries`, dead-letter queue after N attempts) | Redelivery policy and dead letter channel | 🟡 | Needs persistent internal queues (3.4) |

### 3.2 Adapters

| SAP adapter | Cameleer | Status | Gap |
|---|---|---|---|
| HTTPS sender, HTTP receiver | `platform-http`, `http` | 🟡 | Outbound authentication is Basic only. Missing: OAuth2 client credentials, SAML bearer, authorization code and password grants, and CSRF token fetch |
| SOAP 1.x (WS-Security, WS-Addressing, MTOM) | `cxf` | 🟡 | WS-Policy and signed SAML missing. WS-RM persistence missing |
| SOAP SAP RM (EO/EOIO) | none | ❌ | SAP's reliable messaging on SOAP. It needs persistence for exactly-once (EO) and exactly-once-in-order (EOIO) |
| XI sender and receiver | none | ❌ | The PI/PO XI 3.0 protocol: SOAP with SAP headers, EO/EOIO, duplicate handling. **Needed for PI/PO coexistence** |
| IDoc (IDoc-XML over SOAP) | none | ❌ | IDoc XML and the IDoc control record; `SapIDocType` |
| RFC receiver and sender | none | ❌ | **Proprietary protocol.** SAP's NW RFC SDK is a native library under SAP licence. A from-scratch implementation needs the protocol from SAP; with SAP as a partner, that is a request to make |
| OData V2 sender and receiver (CRUD, $batch, function import), V4 receiver, ODC | none | ❌ | OData client and server, V2 (ATOM and JSON) and V4, EDMX metadata, $batch, CSRF. **Used by most S/4HANA and SuccessFactors content** |
| SuccessFactors (SOAP, OData V2/V4, REST), Ariba, LeanIX, MDI | none | ❌ | Built on OData, SOAP and REST plus SAP's authentication |
| SFTP, FTP/FTPS (including Poll Enrich) | `sftp`, `ftp`, `ftps` | ✅ | Check SAP's options: post-processing, `SAP_Ftp*` headers, known hosts |
| SMB, NFS | `file` only (NFS through a mount) | ❌ | SMB2/3 client missing |
| Mail (IMAP, POP3, SMTP, S/MIME) | `imap`, `pop3`, `smtp` | 🟡 | S/MIME missing (needs CMS) |
| JMS (tenant queues) | `jms` over STOMP or AMQP 1.0 | 🟡 | CPI's JMS sits on an internal broker; Cameleer has no persistent internal queue (3.4) |
| AMQP 1.0 (TCP and WebSocket; Event Mesh, Service Bus, Solace, Qpid, Artemis, IBM MQ) | `amqp` | 🟡 | AMQP over WebSocket untested; no interop runs against Solace, Artemis or IBM MQ yet |
| Kafka | `kafka` | 🟡 | SASL OAUTHBEARER and GSSAPI missing (Confluent Cloud and Azure Event Hubs often use OAuth) |
| AdvancedEventMesh (Solace SMF) | none | ❌ | SMF is Solace's proprietary protocol. Solace's AMQP 1.0, MQTT and REST endpoints are a stopgap |
| ProcessDirect, API Direct | `direct`, `vm` | ✅ | Translation |
| JDBC (HANA, Oracle, SQL Server, DB2, PostgreSQL; procedures, batch) | `sql`, `jdbc` (PostgreSQL and embedded) | 🟡 | **HANA, Oracle, SQL Server and DB2 wire protocols missing.** HANA's SQL Command Network Protocol and MS-TDS are published; Oracle TNS is not; DB2 speaks DRDA |
| LDAP | none | ❌ | LDAPv3 client (BER is already in the ASN.1 code) |
| AS2 (with MDN), AS4 (ebMS3 push, pull, receipts), OFTP2, RNIF | none | ❌ | The B2B stack (3.6) |
| ELSTER | none | ❌ | Needs ERiC, a native library from the German tax authority. It cannot be written std-only; it would need an external process |
| AI receiver (SAP AI Core, OpenAI-compatible, Claude) | `openai` presets, `anthropic`, `gemini`, `azure-openai`, `aws-bedrock` | 🟡 | The SAP AI Core gen-AI hub preset is missing |
| IBM MQ (REST and JMS), RabbitMQ | `rabbitmq` | 🟡 | IBM MQ missing: its native protocol is proprietary, but its REST and AMQP 1.0 channels can be reached |
| AWS (S3, SQS, SNS, SWF, DynamoDB, EventBridge, Kinesis) | about 40 AWS components | ✅ | SWF missing (deprecated by AWS) |
| Azure (Storage incl. Table, Service Bus, Cosmos DB) | `azure-*` | 🟡 | Azure Table storage missing |
| Google (BigQuery, Storage, Pub/Sub, Workspace) | `google-*` | ✅ | |
| Microsoft 365 (Dynamics, OneDrive, SharePoint, Outlook, Teams, OneNote) | none | ❌ | Microsoft Graph client |
| Salesforce (REST, Pub/Sub, Service Cloud), Workday, ServiceNow, Snowflake, Splunk, Zendesk, HubSpot, Shopify, BigCommerce, Box, Dropbox, DocuSign, Adobe Sign, Coupa, NetSuite, Anaplan, BambooHR, SugarCRM | none (Slack, Twilio, Jira, MongoDB present) | ❌ | Mostly REST plus OAuth. Each is small once there is a generic OAuth2 client |
| OpenConnectors | none | ➖ | The SAP service stays; Cameleer only needs the OpenConnectors receiver, which is HTTP |
| Adapter Development Kit (Java/OSGi Camel components) | Rust component API | ❌ | **Customer and partner Java adapters cannot run** (6.3) |

### 3.3 Mapping and scripting compatibility

| Item | Status | Gap |
|---|---|---|
| Camel Simple, as the subset SAP supports | ✅ | Cameleer's Simple is a superset |
| Camel headers SAP documents (`CamelHttpResponseCode`, `CamelFileName`, `CamelSplitIndex`, `CamelLoopIndex`, `CamelAggregatedCompletedBy` and so on) | ✅ | Cameleer uses Camel's names |
| SAP framework headers and properties (`SAP_ApplicationID`, `SAP_MessageProcessingLogID`, `SAP_Sender`, `SAP_Receiver`, `SAP_MplCorrelationId`, `SapAuthenticatedUserName`, `SAP_*` adapter headers) | ❌ | Must be set by the runtime with SAP's meanings |
| Groovy 4 language features | 🟡 | Interpreter, not the JVM. Coverage of real scripts unmeasured; `@Grab` and arbitrary Java classes are impossible |
| Groovy 1.1 legacy runtime (Groovy 2.4 semantics) | 🟡 | Mostly a subset of Groovy 4; to be checked |
| JavaScript script step | 🟡 | SAP's engine is probably Rhino or Nashorn, which add Java interop that ES-only engines lack |
| XSLT 3.0 through Saxon, Java extension functions | ❌ / 🟡 | XSLT 3.0 is missing; Java calls are impossible |
| Graphical message mapping, function libraries, imported archives (Java) | ❌ | `.mmap` runtime missing; Java archives impossible |
| Value mapping, ID mapping, operation mapping | ❌ | |
| JSONata | ❌ | Not used by CPI; only relevant if content arrives from other platforms |

### 3.4 Runtime services CPI provides around the Camel engine

| Service | Status | Gap |
|---|---|---|
| Message processing log: statuses COMPLETED, PROCESSING, RETRY, ESCALATED, FAILED, CANCELLED, DISCARDED, ABANDONED; log levels None to Trace; custom header properties; MPL attachments | 🟡 | Cameleer has message history, a tracer and a backlog tracer, but no MPL with SAP's model or query API. **SAP's monitoring UI depends on it** |
| Persistent JMS queues inside the tenant (limits, retry, move, OData APIs) | ❌ | `seda` and `vm` live in memory. Edge Integration Cell embeds a Solace broker, so there Cameleer needs AMQP or SMF to it, not its own store |
| Data store (local and global, expiry) | ❌ | |
| Global and local variables | ❌ | |
| Number ranges | ❌ | |
| Partner directory (per-partner key-value: XSLT, XSD, certificates) | ❌ | |
| Externalized parameters | 🟡 | Property placeholders exist; SAP's per-artifact parameter model doesn't |
| Security material: user credentials, secure parameters, OAuth2 (client credentials, SAML bearer, authorization code, password), PGP keyrings, SSH known hosts | 🟡 | Keystores (PKCS#12, JKS, JCEKS, PEM) and PGP keys load; there is no secure-store API and no outbound OAuth2 or SAML bearer flows for `http` |
| Keystore service (SAP key history, signing requests, OData API) | 🟡 | Cameleer loads keystores but has no managed keystore service. The service belongs to SAP's platform; Cameleer needs only a read API |
| Cloud Connector (Location ID, HTTP and RFC proxy, SOCKS5 for SFTP, mail, AMQP, OFTP) | ❌ | A SOCKS5 client with SAP's authentication, plus the BTP connectivity-service HTTP proxy with its JWT headers |
| Principal propagation | ❌ | |
| Cluster locks for pollers, multi-worker high availability | ❌ | No leader election or cluster service. Pollers on several workers would double-process files |
| Distributed tracing (W3C `traceparent`, OTLP export) | 🟡 | OpenTelemetry export is in progress (`docs/ROADMAP.md`); `traceparent` propagation is to be checked |
| Health and metrics (Prometheus on Edge Integration Cell) | ✅ | `/q/health`, `/q/metrics` |
| Deployment, undeployment and hot redeployment of artifacts at run time | 🟡 | `--dev` reload and route control exist; there is no deployment API that takes an iFlow bundle |
| Inbound authentication (roles, X.509 to user, OAuth through XSUAA) | ➖ / ❌ | In the cloud, BTP and Istio do this in front of the worker. Cameleer's own REST inbound authentication is TLS and mTLS only |
| Transport (TMS, CTS+, MTAR), versioning, content packages | ➖ | SAP tooling; unaffected if the bundle format stays the same |

### 3.5 Persistence and databases the runtime itself needs

CPI keeps aggregation state, idempotency records, data stores, variables, MPLs and JMS on a
database: SAP HANA or PostgreSQL in Edge Integration Cell, which also uses Redis, Valkey or HANA as
its data store. Cameleer has PostgreSQL and Redis clients, so **Edge Integration Cell's PostgreSQL
and Redis/Valkey options are within reach**. HANA would need a HANA wire-protocol client.

### 3.6 B2B and EDI

| Item | Status |
|---|---|
| CMS/PKCS#7 (sign, encrypt, verify), S/MIME | ❌ (the foundation for everything below) |
| AS2 (RFC 4130) with synchronous and asynchronous MDNs | ❌ |
| AS4 (OASIS ebMS 3.0 / AS4 profile, WS-Security, push and pull, receipts) | ❌ (builds on `cxf` WS-Security, which is partial) |
| OFTP2 (RFC 5024), RNIF (RosettaNet) | ❌ |
| EDIFACT, X12, VDA, TRADACOMS, EANCOM parsing and serialization, validation against MIG XSDs, acknowledgements | ❌ |
| Trading Partner Management, Integration Advisor, B2B monitor | ➖ (design-time; they generate iFlows and partner directory entries that the runtime must then run) |

### 3.7 Suite capabilities that are not the Camel runtime

None of these are needed for proposals A and B; SAP keeps them. They are listed for completeness
and to show where Cameleer could add value later.

| Capability | Cameleer today | Note |
|---|---|---|
| API Management (proxies, policies, developer hub, analytics, monetization) | 🟡 | Cameleer has a reliability envelope (rate limits, API keys, JWT, quotas, audit) for AI tools and MCP only. The API Management runtime is a separate engine; not a Camel replacement |
| Integration Cell MCP servers | ✅ | Cameleer's MCP server and `--mcp-expose` are a strong fit; a possible second pitch |
| Event Mesh, Advanced Event Mesh (Solace) | ➖ | Brokers. Cameleer is a client |
| Open Connectors | ➖ | SaaS |
| Integration Advisor, TPM, Integration and Migration Assessment | ➖ | Design-time |
| Data Space Integration (Dataspace Protocol 2025-1, Catena-X) | ❌ | Separate product |
| API Composition (graph, GraphQL) | ➖ | Cameleer has a `graphql` component |
| Generative AI in the designer (Joule, iFlow generation, script review) | ➖ | Design-time |
| Web designer, monitoring UI, transport | ➖ | Cameleer must feed SAP's monitoring through the same MPL and APIs |

## 4. Where Cameleer already wins

| Point | Evidence |
|---|---|
| Memory and startup | Up to 54× less memory; 3 ms startup against 1.1 s for Camel on the JVM and 30 ms for Camel Quarkus native (`docs/BENCHMARKS.md`, DigitalOcean c-4 runs) |
| Throughput | 1.7–8.7× Camel 4.23 on the JVM across the published scenarios |
| Size | 23.9 MB binary with every component; 5.8 MB for `core`; 20.4 MB compressed container image |
| Camel version | Camel 4 names and semantics against SAP's Camel 3.14 fork. Moving SAP to Cameleer would also move it off Camel 3 |
| Supply chain | No external crates. SAP currently ships and patches Saxon, BouncyCastle, Spring, Quartz, Jasypt, IAIK, SSHD and JSch inside the worker |
| AI and MCP | MCP server (official conformance 39/39 for 2025-11-25, 26/26 for 2025-06-18), MCP client and gateway, a tool envelope with approvals and audit, an `agent` step, and six model providers. SAP's own MCP servers only allow authentication, quota and IP-filter policies |
| Verified interop | 74 real-server, 58 official-client and 35 simulator tests (`docs/INTEROP.md`) |
| Serverless and edge | AWS Lambda runtime, read-only root filesystem, 2.6 MiB per running HTTP route in a container |

## 5. Blockers, ranked

Effort uses T-shirt sizes for one experienced engineer working the way this project has so far
(std-only, with tests): S ≈ under 2 weeks, M ≈ 2–6 weeks, L ≈ 1–3 months, XL ≈ more than 3 months.
These are rough estimates, not commitments.

| # | Blocker | Why it blocks | Effort |
|---|---|---|---|
| 1 | **Independent security audit** of `cameleer-tls`, `cameleer-ssh`, xmlsecurity and the crypto data formats, plus fuzzing of every parser (STATUS todo 5) | SAP's product security review will reject unaudited home-grown TLS and crypto for customer traffic | External audit plus M of fixes |
| 2 | **iFlow importer:** read the `.iflw` BPMN bundle, its parameters and its resources into the route model | Without it no SAP content runs. The format is not publicly specified, but a study of 419 real exports ([iflow-format.md](iflow-format.md)) found it regular and declarative: 15 step kinds cover 89% of elements, and message mappings are interpretable XML, not Java. SAP's cooperation would still help with defaults per version and rare steps | L |
| 3 | **SAP Script API** for Groovy and JavaScript (`Message`, `ITApiFactory` services, `messageLogFactory`), plus a script-coverage run over a corpus of real customer scripts | Most real iFlows contain scripts | M to L |
| 4 | **OData V2 and V4** client and server, with EDMX, $batch and CSRF | S/4HANA, SuccessFactors and most standard content | L |
| 5 | **IDoc-over-SOAP and XI 3.0** adapters, **SOAP SAP RM** with EO/EOIO | ECC and S/4HANA on-premise, and PI/PO coexistence | L |
| 6 | **Runtime services:** MPL with SAP's model, persistent queues or a connection to the Edge Integration Cell broker, data store, variables, number ranges, partner directory, value mapping, secure store | SAP's monitoring and many flow steps depend on them | L to XL |
| 7 | **Message mapping runtime** (`.mmap`) and **XSLT 3.0** | Mapping is in nearly every iFlow | L (mapping) + L (XSLT 3.0) |
| 8 | **HANA wire protocol** (and at least MS-TDS for SQL Server) | Runtime persistence in the cloud is on HANA; customer JDBC adapters | M (HANA) + M (TDS); Oracle TNS is XL and legally uncertain |
| 9 | **Cloud Connector and outbound authentication:** SOCKS5 and the connectivity proxy, OAuth2 client credentials, SAML bearer, authorization code | All on-premise traffic and most SaaS receivers | M |
| 10 | **Streaming splitters and large-message handling** (StAX-style XML splitting, lazy tokenize) | Payloads of hundreds of megabytes are common in CPI | M |
| 11 | **Clustering:** cluster locks for pollers, multi-worker high availability | Edge Integration Cell needs 3 nodes; double processing is unacceptable | M |
| 12 | **RFC** | ABAP back ends. Needs the protocol from SAP | L, depends on SAP |
| 13 | **B2B:** CMS and S/MIME, then AS2, AS4, EDI converters, validator and acknowledgements, then OFTP2 and RNIF | Premium-edition customers | XL in total |
| 14 | **Connectors:** Salesforce, Microsoft Graph, ServiceNow, Workday, Snowflake, Splunk, LDAP, SMB, IBM MQ, Solace SMF | Long tail of adapters | M each group |

## 6. Strategic questions for the owner

### 6.1 Which proposal?

Proposal **B (Edge Integration Cell runtime) is the strongest first step**:

- Footprint and cost hit the customer directly on their own Kubernetes, where Cameleer's numbers
  matter most.
- Edge Integration Cell uses PostgreSQL and Redis/Valkey, which Cameleer already speaks.
- SAP can trial it on a subset of content without touching its multi-tenant cloud.

Proposal A follows once the blockers are cleared.

### 6.2 What to ask SAP for

A partnership would remove the riskiest unknowns:

- The iFlow bundle and `.mmap` format specifications.
- The RFC protocol, or permission to use the NW RFC SDK from an external process.
- The XI and SAP RM protocol specifications.
- The MPL data model and monitoring API contracts.
- A corpus of anonymized iFlows and scripts for a compatibility run.
- Access to the Business Accelerator Hub content for conformance tests.

### 6.3 Java content

SAP content can include Java: ADK adapters, imported archives, Java mapping functions and XSLT
Java extensions. The options:

1. **Out-of-process JVM bridge.** Run unconverted Java pieces in a sidecar JVM and call them over a
   local protocol such as gRPC, which Cameleer already has. This keeps Cameleer std-only, but the
   footprint advantage shrinks for flows that need it.
2. **Convert.** SAP already ships an AI "Integration Content Migration Agent" that turns Java
   mappings into Groovy 4. Cameleer could rely on that path and on porting partner adapters to
   Rust.
3. **Refuse Java content.** This would leave some customers stuck on the Java runtime.

A mix of 1 and 2 is likely. The decision is the owner's, since option 1 changes the project's
architecture.

### 6.4 The std-only rule

Two gaps cannot be closed std-only, because the only implementations are native libraries:
ELSTER's ERiC, and RFC if SAP will not share the protocol. Both would need an external helper
process. That does not break the no-crates rule, but it is a new pattern for the project.

## 7. Suggested phases

| Phase | Content | Exit criterion |
|---|---|---|
| 0. Credibility | Security audit and fuzzing; OpenTelemetry/OTLP and `traceparent`; HANA client; clustering; fix the doc inconsistencies below | Audit report with no open high findings |
| 1. Run simple iFlows | iFlow importer for the common steps; SAP Script API; SAP headers; MPL; data store, variables, number ranges, value mapping; outbound OAuth2; Cloud Connector; streaming split | A set of HTTP, SFTP, SOAP and JDBC iFlows from the Business Accelerator Hub runs unchanged on Edge Integration Cell with matching MPLs |
| 2. Run S/4HANA and SuccessFactors content | OData V2/V4; IDoc; XI; SOAP SAP RM; message mapping runtime; XSLT 3.0; partner directory | A majority of the S/4HANA and SuccessFactors standard packages pass the conformance run |
| 3. B2B | CMS and S/MIME, AS2, AS4, EDI and acknowledgements, OFTP2, RNIF | TPM-generated B2B flows run |
| 4. Long tail | RFC (with SAP), SaaS connectors, IBM MQ, Solace SMF, LDAP, SMB, Oracle and DB2 | Adapter list parity |

## 8. Doc inconsistencies found during this research

- `docs/FEATURES.md` line 90 says aggregation repositories are in memory only, and line 97 says
  the idempotent repository is memory-only. JDBC and Redis repositories exist
  (`crates/cameleer-sql/src/repository.rs`, `crates/cameleer-datastores/src/redis/repository.rs`)
  and are documented at lines 238 and 306.
- `docs/FEATURES.md` line 80 says OCSP is not supported, while `ARCHITECTURE.md` lists an `ocsp`
  module for stapling. Server-side stapling seems to exist; client-side OCSP checking does not.

## 9. Sources

**SAP**

- SAP's Markdown source for help.sap.com, `github.com/SAP-docs/btp-integration-suite` (state of
  2026-11-05, increment 2608). Key pages:
  - `docs/ISuite_Integrations_APIs/connectivity-options-93d82e8.md` (adapter list)
  - `headers-and-exchange-properties-provided-by-the-integration-framework-d0fcb09.md`
  - `using-camel-simple-expression-language-4688083.md`
  - `jms-resource-limits-and-optimizing-their-usage-4857054.md`
  - `docs/ci/WhatsNewInCloudIntegration/understanding-software-version-numbers-caad468.md`
  - `patch-releases-for-cloud-integration-023a472.md` (Camel 3.14.7 branch)
  - `docs/ci/Development/define-*.md` (flow steps)
  - `docs/ISuite_Edge_Integration_Cell/` (Kubernetes, PostgreSQL/HANA, Redis/Valkey, embedded
    Solace)
  - `docs/ISuite/artificial-intelligence-6a6727c.md`
- SAP Integration Suite product page:
  https://www.sap.com/products/technology-platform/integration-suite.html. Direct fetches were
  refused, so content counts (3,400+ integrations, 250+ connectors) and pricing come from search
  snippets of SAP pages and 2024 SAP decks. **Treat pricing as unverified.**
- Advanced event mesh: help.pubsub.em.services.cloud.sap and docs.solace.com service-class limits.

**Cameleer**

- `docs/FEATURES.md`, `docs/components/`, `docs/dataformats/`, `docs/languages/`
- `docs/guides/migrating-from-apache-camel.md`, `docs/guides/tls-and-security.md`,
  `docs/guides/observability.md`
- `docs/DEPLOYMENT.md`, `docs/BENCHMARKS.md`, `docs/INTEROP.md`, `docs/STATUS.md`, and the
  crates named above

**Not verified:**

- The iFlow file format and the SAP JavaScript engine (background knowledge, not stated in SAP's
  docs).
- Current Business Accelerator Hub package counts.
- Regional pricing.
- Whether SAP plans a runtime change. Nothing public was found.
