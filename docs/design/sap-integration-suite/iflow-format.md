# The SAP Cloud Integration iFlow format, as found in real exports

_Researched 2026-11-11. This document is internal and is not published on the site. It supports
blocker 2, "iFlow importer", in [gap-analysis.md](gap-analysis.md)._

SAP does not publish a specification of the iFlow artifact. Everything below was learned from
real exported bundles. **Bottom line:** the format is regular, declarative and readable by a
program, and nothing in an `.iflw` file needs a JVM. The flow is BPMN 2.0 XML. Every step and
adapter carries a flat list of key/value properties plus a typed variant id. A small number of
step kinds covers most real flows. An importer is a large but well-understood job.

## 1. How this was learned

- **The sample.** 419 distinct `.iflw` files, de-duplicated by content. All of them come from
  public GitHub repositories found by code search for `filename:.iflw` on 2026-11-11 (542 hits).
  12 repositories were also cloned whole to see complete bundles: 47 `META-INF/MANIFEST.MF` files
  and 40 parameter files. Three files did not parse.
- **What the sample contains.**
  - **Copies of SAP standard content.** Script collections named `com.sap.s42cns.*`,
    `com.sap.s4c2cns.*` and `com.sap.scenarios.version2.c4c2s4.*`, and `metainfo.prop` files
    marked `SAP-MarkedSAP2SAP`.
  - **Customer and partner flows,** plus SAP TechEd samples.
  - **A few generated or hand-written templates.** These lack `cmdVariantUri`.
- **How it was analysed.**
  - **The tally script.** It walks every BPMN element and counts the element kinds, versions,
    property keys, adapters, `{{placeholders}}` and embedded tables. The script is in the session
    scratchpad, not the repo. Counts below come from it.
  - **Manual reading** of whole bundles, including a message mapping (`.mmap`).
  - **A cross-check against [cpilint](https://github.com/mwittrock/cpilint).** cpilint is an
    MIT-licensed iFlow linter (the copy read was the `stevedpedersen/cpilint` fork). Its
    `DefaultXmlModel.java` holds XPath expressions for each step kind, and its `SenderAdapter` and
    `ReceiverAdapter` enums list the adapter `ComponentType` names. It is a good reference; its
    licence allows learning from it.
- **Caveat.** This is a public sample, not SAP's full catalogue. Rare steps and adapters are thin
  or missing (sections 5 and 6). The conformance corpus proposed in the gap analysis, Business
  Accelerator Hub packages, is still needed.

## 2. Bundle layout

An exported iFlow is a zip file. It is also an Eclipse plug-in project and an OSGi bundle:

```
META-INF/MANIFEST.MF                      OSGi manifest with SAP headers (section 3)
META-INF/deployment/queueDefinitions.json JMS queues the flow needs ({"queueDefinitions":[]} if none)
metainfo.prop                             description, source, target (Java properties)
.project, build.properties, .settings/    Eclipse leftovers, ignorable
src/main/resources/
  scenarioflows/integrationflow/<name>.iflw   the flow: BPMN 2.0 XML (section 4)
  parameters.prop                         values of externalized parameters (Java properties)
  parameters.propdef                      their declarations: name, xsd type, required, references
  script/*.groovy, *.js                   scripts
  mapping/*.mmap, *.xsl, *.xslt           message mappings and XSLT
  xsd/, wsdl/, json/, edmx/               schemas used by mappings, validators and adapters
  lib/*.jar                               Java archives (2 of 47 bundles): can't run without a JVM
```

## 3. Bundle metadata

- **`MANIFEST.MF`:**
  - `SAP-BundleType: IntegrationFlow`
  - `SAP-NodeType: IFLMAP` and `SAP-RuntimeProfile: iflmap`. The runtime profile matters: SAP's
    docs say profiles decide which features are available.
  - `Bundle-SymbolicName`, the artifact id. `Bundle-Version`, for example `1.0.2`.
  - A long `Import-Package` list naming SAP internals (`com.sap.it.op.agent.mpl`,
    `com.sap.sod.utils.idoc.soap`, `com.sap.aii.mappingtool.tf7.rt`, ...) and Camel 2.8-era
    packages.

  The import list tells us which runtime pieces SAP wires in (MPL agent, IDoc-SOAP, the PI mapping
  tool runtime). An importer can ignore it.
- **`parameters.prop`.** Java properties: `Name=value`. Values may be format strings. One sample
  has `Directory_Sftp=/outgoing/cpi0204/%2$s_%3$s_%1$s`.
- **`parameters.propdef`.** XML. Each `<parameter>` has `<name>`, `<type>` (`xsd:string` and so
  on), `<isRequired>`, `<constraint>` and `<description>`. A `<param_references>` block links
  parameters to adapter attributes, for example `attribute_id="/attrId::urlPath"`.
- **`metainfo.prop`.** `description`, `source`, `target` and, for SAP content,
  `SAP-MarkedSAP2SAP`.

## 4. The `.iflw` file

The namespaces:

| prefix | namespace |
|---|---|
| `bpmn2` | `http://www.omg.org/spec/BPMN/20100524/MODEL` |
| `ifl` | `http:///com.sap.ifl.model/Ifl.xsd` (three slashes) |
| `bpmndi`, `dc`, `di` | BPMN diagram interchange: layout only, ignorable |

The tree:

```
bpmn2:definitions
  bpmn2:collaboration            IFlowConfiguration: flow-wide settings
    bpmn2:extensionElements/ifl:property*
    bpmn2:participant*           ifl:type = IntegrationProcess | EndpointSender | EndpointRecevier
    bpmn2:messageFlow*           one per adapter channel (sourceRef/targetRef link participants and events)
  bpmn2:process id="Process_1"   the main Integration Process
    startEvent, endEvent, callActivity, serviceTask, exclusiveGateway, parallelGateway,
    subProcess (exception subprocess), sequenceFlow (+ conditionExpression)
  bpmn2:process*                 Local Integration Processes (processType = directCall)
  bpmndi:BPMNDiagram             layout
```

Notes on the tree:

- **`EndpointRecevier` is misspelled in SAP's own output.** It is the receiver participant; 910 of
  them appear in the sample. Match it exactly.
- **Sequence flows define the order.** A route is the walk from the start event along
  `bpmn2:sequenceFlow` (`sourceRef` to `targetRef`).
- **Adapters attach to events and steps.**
  - A sender adapter is a `messageFlow` from an `EndpointSender` participant to the start event.
  - A receiver adapter is a `messageFlow` from a step (an `ExternalCall` service task, or an end
    event) to an `EndpointRecevier` participant.
- **Participant `ifl:type` counts:** IntegrationProcess 964, EndpointRecevier 910,
  EndpointSender 372.
- **Event definitions:**

  | Definition | Count | What it means |
  |---|---|---|
  | `messageEventDefinition` | 1,047 | Normal start and end |
  | `errorEventDefinition` | 487 | Exception subprocess |
  | `timerEventDefinition` | 91 | Timer start |
  | `escalationEventDefinition` | 34 | Escalation end |
  | `terminateEventDefinition` | 5 | Terminate |

### 4.1 How properties are encoded

Every configurable element has `bpmn2:extensionElements` with a flat list of:

```xml
<ifl:property><key>KEY</key><value>VALUE</value></ifl:property>
```

Four keys appear on almost every element:

| key | meaning | example |
|---|---|---|
| `cmdVariantUri` | **The real type and version of the element.** Grammar: `ctype::<Kind>/cname::<Name>/version::<x.y.z>`, where `ctype` is `FlowstepVariant`, `AdapterVariant`, `FlowElementVariant` or `IFlowVariant` | `ctype::FlowstepVariant/cname::Enricher/version::1.5.0` |
| `componentVersion` | major.minor of the same version | `1.5` |
| `activityType` | step family, shared by variants | `Enricher`, `Script`, `Mapping`, `Splitter`, `DBstorage` |
| `subActivityType` | variant inside the family | `GroovyScript`, `JavaScript`, `XSLTMapping`, `parallel` |

`activityType` alone is not enough to identify a step:
- `Splitter` covers `GeneralSplitter`, `Camel` (the iterating splitter) and `ZipSplitter`.
- `DBstorage` covers the data store operations `put`, `get`, `select` and `delete`.
- `Mapping` covers `MessageMapping` and `XSLTMapping`.

The **`cname`** inside `cmdVariantUri` is the key an importer should switch on.

**Each step kind is versioned, and many versions coexist.** For example:

| Kind | Versions in the sample |
|---|---|
| Content Modifier (`Enricher`) | 1.4.2, 1.5.0, 1.5.1 and 1.6.0 |
| HTTP receiver adapter | about 1.7 to 1.17 |
| OData adapter (`HCIOData`) | up to 1.24 |
| Groovy script | 1.0.1, 1.1.1, 1.1.2 and 2.0.0. **2.0.0 is "Script 2.0", Groovy 4**; 1.x is the legacy runtime |

An importer needs, per `cname`, the range of versions it supports and the defaults each version
assumes.

### 4.2 Tables inside property values

Some values are small HTML-escaped tables:
`<row><cell id='Name'>…</cell><cell id='Type'>…</cell>…</row>`.

The most important ones are the Content Modifier's `headerTable` and `propertyTable` (472 and
749 files). Each row has these cells:

- **`Action`:** `Create` (2,717 rows) or `Delete` (214).
- **`Name`.**
- **`Type`:**

  | Value | Rows |
  |---|---|
  | `constant` | 1,675 |
  | `xpath` | 574 |
  | `expression` (Camel Simple) | 479 |
  | `property` | 103 |
  | `header` | 74 |
  | `global persisted variables` | 19 |
  | `persisted variables` | 6 |
  | `numberRange` | 1 |
- **`Value`.**
- **`Default`.**
- **`Datatype`:** a **Java class name**, mostly empty or `java.lang.String`, also
  `java.lang.Boolean`, `java.lang.Integer` and `java.io.Reader`. Cameleer must map these to its
  own type conversions.

Other tables:

| Table | Where it appears |
|---|---|
| `variable` | Write Variables |
| `routingSequenceTable` | Sequential Multicast |
| `xmlJsonPathTable` | XML to JSON |
| `jsonNamespaceMapping` | JSON to XML |
| `clientCertificates` | HTTPS and SOAP senders |
| PGP user ids | PGP steps |
| `topicSubscriptions` | Solace and AEM adapters |
| `scheduleKey` | Timers and polling adapters |

The `scheduleKey` table uses positional cells: `dateType`, `timeType`, `triggerType`,
`noOfSchedules`, `schedule1=fireNow=true`, `timeZone=( UTC 0:00 ) Greenwich Mean
Time(Etc/GMT)`, and so on. It has to be translated to Cameleer's `timer` or `cron`.

### 4.3 Externalized parameters

`{{Name}}` can appear in any property value, including inside table cells. It is replaced from
`parameters.prop` at deployment. The most common places in the sample:

| Property | Count |
|---|---|
| Content Modifier `propertyTable` | 710 |
| HTTP adapter address | 382 |
| HTTP credential name | 289 |
| OData address | 172 |
| ProcessDirect address | 159 |
| HTTP authentication method | 149 |

Parameters also hold non-string settings such as `authenticationMethod`, `bodyType`,
`transactionalHandling` and `proxyType`. So substitution must happen before a property is
interpreted. This matches Cameleer's `{{...}}` property placeholders. Note that SAP parameter
names may contain spaces (`{{FSM Company ID}}`).

### 4.4 Expressions

- **Router conditions** are `bpmn2:conditionExpression` text on the outgoing `sequenceFlow`
  elements (`GatewayRoute`, 906 instances). The route's `expressionType` is `XML` (512) or
  `NonXML` (418), also spelled `Non XML` and `Non-XML`. Non-XML conditions look like:

  ```
  ${property.FSMHost} = 'https://…'
  ${property.PostExtensionEnabled} regex '(?i)(true|x)'
  ```

  SAP's dialect uses a single `=`, `regex`, `in` and `contains`. It has to be translated to, or
  accepted by, Cameleer's Simple. To check: whether Camel 2/3 Simple accepted a single `=` or SAP
  rewrites it.
- **Content Modifier `expression` cells** are Camel Simple, including the deprecated
  `${in.body}`.
- **`xpath` cells, Filter (`xpathType`) and splitters (`exprType`: `XPath`, `Line Break`)** use
  XPath, with namespaces from the flow-wide `namespaceMapping` property.

### 4.5 References to other files and artifacts

| Property | Points to |
|---|---|
| `script`, `scriptFunction` | `src/main/resources/script/<file>`; the function defaults to `processData` (empty in 26 cases) |
| `scriptBundleId` | **A Script Collection, which is a separate artifact.** Used by 815 of 1,161 script steps, including SAP content (`com.sap.s42cns.*`) |
| `mappinguri` | `dir://mmap/src/main/resources/mapping/<x>.mmap` or `dir://mapping/xslt/src/main/resources/mapping/<x>.xsl` |
| `messageMappingBundleId` | **A Message Mapping, which is a separate artifact** (148 of 261 mapping steps) |
| `processId` | A local process (`Process_19` and so on) in the same file |
| ProcessDirect `address` | Another iFlow's ProcessDirect sender. **Calls between iFlows**, so an importer must load whole packages, not single flows |

So the unit of import is a **package**: iFlows plus Script Collections, Message Mappings and
Value Mappings. A single bundle is not enough.

## 5. Step catalogue, by frequency

Counts are elements in the 419 files. Ten kinds cover 79% of the 8,810 typed elements, 15 cover
89%, and 30 cover 97%. There are 82 kinds in total, including events and routes.

| `cname` (BPMN tag) | Count | Main properties | Cameleer target |
|---|---|---|---|
| `Enricher` (callActivity), the Content Modifier | 1,432 | `headerTable`, `propertyTable`, `bodyType` (`expression`/`constant`), `wrapContent` (the body) | `setHeader` / `setProperty` / `removeHeader` / `setBody` |
| `GroovyScript` (callActivity) | 1,158 | `script`, `scriptFunction`, `scriptBundleId` | `groovy` with the SAP `Message` API shim |
| `GatewayRoute` (sequenceFlow) | 906 | `expressionType` + `conditionExpression` | `choice/when` |
| `ExternalCall` (serviceTask), Request Reply | 731 | the receiver adapter on its messageFlow | `to` (InOut) |
| `MessageEndEvent` | 668 | – | end of route |
| `NonLoopingProcess`, Process Call | 594 | `processId` | `to("direct:…")` |
| `LocalIntegrationProcess` (process) | 519 | `transactionalHandling`, `transactionTimeout` | route from `direct:` |
| `IntegrationProcess` (process) | 443 | `transactionalHandling` (`Required`, `Not Required`, `Required JMS`) | the main route |
| `ExclusiveGateway`, Router | 417 | `throwException` (no route matched) | `choice/otherwise` |
| `ErrorEventSubProcessTemplate` (subProcess) | 317 | – | `onException` / `doTry` |
| `MessageStartEvent` | 266 | – | `from` |
| `MessageMapping` | 261 | `mappinguri`, `mappingname`, `messageMappingBundleId` | **`.mmap` runtime (section 7)** |
| `JsonToXmlConverter` | 99 | `useNamespaces`, `addXMLRootElement`, `additionalRootElementName`, `jsonNamespaceMapping` | `xj` with SAP's conventions |
| `intermediatetimer`, Timer Start | 88 | `scheduleKey` table | `timer` / `cron` |
| `XmlToJsonConverter` | 74 | `xmlJsonUseStreaming`, `xmlJsonSuppressRootElement`, `xmlJsonConvertAllElements`, `xmlJsonPathTable` | `xj` |
| `Filter` | 60 | `xpathType`, `wrapContent` (the XPath) | XPath filter |
| `XSLTMapping` | 51 | `mappinguri`, `mappingoutputformat`, `mappingSource`, `mappingHeaderNameKey` | `xslt` (3.0 needed) |
| `GeneralSplitter` | 40 | `splitType`, `exprType`, `splitExprValue`, `grouping`, `Streaming`, `ParallelProcessing`, `SplitterThreads`, `StopOnExecution`, `timeOut` | `split` |
| `put` / `get` / `select` / `delete` (Data Store) | 39 / 31 / 10 / 9 | `storageName`, `visibility` (local/global), `messageId`, `expire`, `encrypt`, `override`, `maxresults`, `stopOnMissingEntry` | **data store (missing)** |
| `Variables`, Write Variables | 38 | `variable` table, `visibility`, `expire`, `encrypt` | **persistent variables (missing)** |
| `Gather` | 36 | `aggregationAlgorithm`, `messageType`, `sourceXPath`, `targetXPath` | aggregation strategy |
| `EscalationEndEvent` | 27 | `ErrorCode` | MPL status ESCALATED |
| `Send` (serviceTask) | 27 | the receiver adapter | `to` (InOnly) |
| `Camel`, the Iterating Splitter | 22 | as the General Splitter, plus `tokenValue` | `split` |
| `XmlModifier` | 16 | `removeXmlDeclaration`, `removeExternalDTDs`, `xmlCharacterHandling` | small processor |
| `SequentialMulticast` / `Multicast` / `Join` (parallelGateway) | 14 / 13 / 11 | `routingSequenceTable` | `multicast` |
| `Base64 Encode/Decode`, `ZIP Compress/Decompress`, `MIME Multipart Message Decode` | 6/10, 5/4, 3 | – | `marshal` / `unmarshal` |
| `LoopingProcess` | 10 | `processId`, `expressionType`, `loopId`, `postProcessing` | `loop` (doWhile) |
| `XmlToCsvConverter` / `CsvToXmlConverter` | 9 / 5 | `XPath_Field_Location`, `Field_Separator_in_CSV`, `XML_Schema_File_Path`, `headerMapping`, ... | `csv` + XML |
| `contentEnricherWithLookup` | 9 | `enrichmentType`, `originalMessageNodePath`, `resourceMessageNodePath`, key elements | `enrich` + key merge |
| `JavaScript` | 7 | as Groovy | `js` |
| `XmlValidator`, `IdempotentProcessCall`, `PollEnrich`, `Persist` | 6, 4, 4, 4 | – | `validator`, `idempotentConsumer`, `pollEnrich`, **message store (missing)** |
| `PgpEncrypt`/`PgpDecrypt`, `Encrypt`/`Decrypt` (PKCS#7), `SimpleSignMessage`, `XMLDigitalVerifySign`, `MessageDigest`, `Aggregator`, `ZipSplitter` | 2–3 each | – | `pgp`; **CMS missing**; `xmlsecurity`; digest; `aggregate`; `zipFile` split |

Not seen in the sample, though SAP's docs list them: IDoc, EDI and PKCS#7 splitters, EDI
converters, validator and extractor, ID Mapping, Number Range as a step, XML Signer, the
Escalation event, and the operation mapping step. Their encoding has to come from the corpus or
from SAP.

## 6. Adapter catalogue, by frequency

Each adapter is a `messageFlow` with `ComponentType`, `direction`, `TransportProtocol`,
`MessageProtocol`, the `cmdVariantUri` `ctype::AdapterVariant/cname::<adapter>/...`, and the
common keys `Name`, `Description`, `system`, `ComponentNS`, `ComponentSWCVName` and
`ComponentSWCVId`. A dash in the direction column means the property was missing.

| `ComponentType` / direction / protocol | Count | Main properties | Cameleer |
|---|---|---|---|
| `HTTP` receiver | 437 | `httpAddressWithoutQuery`, `httpAddressQuery`, `httpMethod` / `methodSourceExpression`, `authenticationMethod`, `credentialName`, `privateKeyAlias`, `proxyType` (`Internet` / `On-Premise`), `locationID`, `httpRequestTimeout`, `throwExceptionOnFailure`, `retryOnConnectionFailure`, `allowedRequestHeaders`, `allowedResponseHeaders`, `enableMPLAttachments` | `http` + outbound OAuth2 + Cloud Connector |
| `ProcessDirect` receiver and sender | 181 / 43 | `address` | `direct` / `vm` across flows |
| `HCIOData` receiver, OData V2 and V4 | 169 / 8 | `address`, `resourcePath`, `operation` (`Query(GET)`, `Read(GET)`, `Create(POST)`, `Patch(PATCH)`), `queryOptions`, `customQueryOptions`, `edmxFilePath`, `fields`, `pagination`, `odatapagesize`, `enableBatchProcessing`, `isCSRFEnabled`, `alias`, `scc_location_id` | **OData client (missing)** |
| `HTTPS` sender | 168 | `urlPath`, `senderAuthType` (`RoleBased`), `userRole`, `clientCertificates`, `xsrfProtection`, `maximumBodySize` | `platform-http` + inbound auth |
| `SOAP` sender and receiver (SOAP 1.x, Plain SOAP) | 69 / 26 | `address`, `soapWsdlURL`, `soapServiceName`, `soapWsdlPortName`, `useWSAddressing`, WS-Security keys (`WSSecurity`, `WSSecurityType`, `SigningOrder`, `AlgorithmSuiteAssertion`, ...) | `cxf` |
| `Salesforce` | 22 | REST, Bulk, Bulk 2, Streaming and Pub/Sub operations, JWT/OAuth credentials | **missing** |
| `SuccessFactors` (OData V2, SOAP) | 35 | `address`, `entity`, `query`, `sfsfODataAction`, `urlSuffixSfOData`, `pagesize` | **missing** |
| `SFTP` receiver and sender, `PollingSFTP` | 15 / 4, 4 | `host`, `path`, `fileName`, `authentication`, `credential_name`, `privateKeyAlias`, `location_id`, `fileExist`, `useTempFile`, `file.move`, `readLock`, `doneFileName`, Camel-style reconnect keys | `sftp` (the keys mirror Camel's file options) |
| `RFC` receiver | 12 | `destination`, `transactioncommit`, `newConnection` | **missing** |
| `Mail` receiver and sender | 13 | `server`, `auth`, `ssl`, `to`/`cc`/`bcc`/`from`/`subject`/`body`, S/MIME keys `encrypt.smime.*`, `signature.smime.*` | `smtp`/`imap` (S/MIME missing) |
| `SolacePubSubPlus` / `AdvancedEventMesh` (SMF) | 12 / 6 | `host`, `messageVpn`, `jcsmpProperties`, OAuth keys, `topicSubscriptions`, `consumerMode`, `deliveryMode` | **missing** |
| `JMS` | 8 | `QueueName_inbound`/`QueueName_outbound`, `RetryInterval`, `MaxRetryInterval`, `ExponentialBackoff`, `useDeadLetterQueue`, `ExpirationPeriod`, `EncryptMessage`, `TransferExchangeProperties` | **internal queues (missing)** |
| `AMQP` (WebSocket, AMQP 1.0) | 8 | `host`, `port`, `path`, `destinationName`, `destinationType`, `connectWithTLS`, `maxRetries`, `delivery` | `amqp` |
| `IDOC` sender (IDoc SOAP) | 7 | `address`, `userRole`, `xmlCharacterHandling` | **missing** |
| `DataStoreConsumer` sender | 6 | `storageName`, `visibility`, `PollDelay`, retry keys | **missing** |
| `JDBC` receiver | 4 | `alias` (data source), `queryTimeout`, `batchMode`, `pageSize` | `jdbc` (PostgreSQL only) |
| `FTP`, `AS2` (incl. MDN), `XI`, `AI` (`Custom-OpenAI-REST`), `AmazonWebServices` (S3), `OpenConnectors`, `ODataSender` | 1–3 each | – | `ftp`; AS2 missing; XI missing; `openai`; `aws2-s3`; HTTP; OData server missing |

## 7. Message mappings (`.mmap`)

The `.mmap` file is XML, `xiObj` in the namespace `urn:sap-com:xi`. It is PI's mapping object,
`typeID="XI_TRAFO"`, with an `XiTrafo` payload marked `mappingtool version="XI7.1"`. Its parts:

- **`lnks`** name the source and target structures by kind and path:
  - `SOURCE_IFR_MESS` and `TARGET_IFR_MESS`, with `typeID` `xsd`, `json`, `wsdl` or `edmx`. For
    example `Products.xsd` in `src/main/resources/xsd`, or an OpenAPI JSON request schema.
  - `UsedFuncLib`, which names Groovy UDF scripts (`typeID="gsh"`).
- **The mapping is a tree of `brick` elements, one per target field:**
  - `type="Dst"`: the target XPath, for example `/…/PurchaseOrderItem/items/Item`.
  - Its `arg` children are the inputs:
    - `type="Src"`: a source path such as `/Products/Product`.
    - `type="Func"`: a function, with `fname` (for example `SplitByValue`, `index`), `fns`
      (`dflt` for the standard library) and `bindings` for its parameters. One example has
      `index` with `start=10`, `inc=10`; another has date formats `oform=yyyy/MM/dd` and calendar
      settings.
  - `viewData` holds editor coordinates and can be ignored.

**What this means.** Mappings are declarative and contain no compiled Java. A Cameleer runtime
can interpret them. The hard part is semantic: matching PI's queue and context model (contexts,
`SplitByValue`, `removeContexts`, `collapseContexts`, `useOneAsMany`, and the order in which
elements are produced) and the standard function library exactly. That needs SAP's function
documentation and a set of reference input/output pairs. Function libraries and imported
archives from the ESR are the exception: they are Java.

## 8. What an importer looks like

1. **Load a package**: iFlow bundles, Script Collections, Message Mappings and Value Mappings.
   Substitute `{{parameters}}` from `parameters.prop`.
2. **Parse each `.iflw`.**
   - Each `bpmn2:process` becomes a route. `Process_1` takes its `from` from the sender
     `messageFlow` on its start event, or from a timer. Local processes become `direct:` routes.
   - Walk `sequenceFlow` from the start event:
     - `callActivity` and `serviceTask` become processors, chosen by `cname` and version.
     - `exclusiveGateway` becomes `choice`.
     - A `parallelGateway` pair (`Multicast`/`SequentialMulticast` … `Join`, usually followed by
       `Gather`) becomes `multicast` with an aggregation strategy.
     - Receiver `messageFlow`s become endpoint URIs.
   - An error `subProcess` becomes the route's `onException` / `doTry`.
3. **Build the existing Cameleer route model**, the same one the YAML and XML DSLs produce
   (`cameleer-yaml` model), so validation, `cameleer validate` and the runtime are reused.
4. **Report unsupported `cname`/version pairs.** Like `cameleer validate`, refuse to deploy a
   flow that has them instead of running it differently.

Suggested order of work (from section 5's frequencies):

- **First:** Content Modifier, Groovy, Router, Request Reply with HTTP, process calls, exception
  subprocess, timers, the JSON/XML converters, Filter, splitters, ProcessDirect. That set covers
  most simple flows.
- **Next:** OData, message mapping, XSLT, data store and variables, Gather/Multicast, SOAP.

## 9. Still unknown

- **Defaults and changes per version.** These are not visible in exports when a property is left
  at its default, and are only guessable from documentation.
- **SAP semantics that exports can't show.** How the Content Modifier treats `Datatype`, the exact
  JSON↔XML conventions, MPL statuses after escalations, and the transaction behaviour of
  `Required JMS`.
- **The encoding of rare steps and adapters** that the sample lacks (end of section 5).
- **Whether SAP considers the format stable.** No public statement was found. A specification from
  SAP remains the best outcome (gap analysis, section 6.2). Until then, the importer must be
  tested against many real exports.
