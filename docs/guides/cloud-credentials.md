# Cloud credentials, regions and endpoints

> How Cameleer's AWS and Azure components find credentials (keys, profiles, instance and container roles, web identity, managed identity, SAS tokens), choose a region, and talk to emulators such as LocalStack and Azurite.

This guide covers every `aws*` component (for example [aws2-s3](../components/aws2-s3.md),
[aws2-sqs](../components/aws2-sqs.md), [aws-bedrock](../components/aws-bedrock.md)) and every `azure-*`
component (for example [azure-storage-blob](../components/azure-storage-blob.md),
[azure-servicebus](../components/azure-servicebus.md), [azure-openai](../components/azure-openai.md)).
All of them are implemented in Rust without the AWS or Azure SDKs; the behaviour below is that of
Cameleer's own code, which follows the SDKs' conventions.

## AWS

### Where credentials come from

When an endpoint has both `accessKey` and `secretKey`, they are used (with `sessionToken` for
temporary credentials). Otherwise Cameleer walks the default credentials chain, in this order:

| order | source | details |
|---|---|---|
| 1 | Environment variables | `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` (or `AWS_SECRET_KEY`), optional `AWS_SESSION_TOKEN`. |
| 2 | Shared credentials file | `AWS_SHARED_CREDENTIALS_FILE`, else `$HOME/.aws/credentials`; profile `profileCredentialsName`, else `AWS_PROFILE`, else `default`. Keys `aws_access_key_id`, `aws_secret_access_key`, `aws_session_token`. |
| 3 | Web identity (EKS IRSA, any OIDC federation) | `AWS_WEB_IDENTITY_TOKEN_FILE` + `AWS_ROLE_ARN` (optional `AWS_ROLE_SESSION_NAME`): STS `AssumeRoleWithWebIdentity`. The STS endpoint is `AWS_ENDPOINT_URL_STS`, `AWS_ENDPOINT_URL`, the endpoint override, or `https://sts.{region}.amazonaws.com`. |
| 4 | Container credentials (ECS task roles, EKS Pod Identity) | `AWS_CONTAINER_CREDENTIALS_FULL_URI`, or `AWS_CONTAINER_CREDENTIALS_RELATIVE_URI` on `http://169.254.170.2`; authorization from `AWS_CONTAINER_AUTHORIZATION_TOKEN` or `AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE`. |
| 5 | EC2 instance metadata (instance profiles) | IMDSv2 with a session token, falling back to IMDSv1, on `AWS_EC2_METADATA_SERVICE_ENDPOINT` (default `http://169.254.169.254`). Disable with `AWS_EC2_METADATA_DISABLED=true`. |

Remote credentials are cached per process until five minutes before they expire. An unreachable
metadata endpoint is remembered for a minute, so the chain stays fast on machines that are not on EC2.

Two options narrow the chain:

- `useDefaultCredentialsProvider=true` ignores `accessKey` / `secretKey` on the endpoint and always
  uses the chain.
- `useProfileCredentialsProvider=true` (with `profileCredentialsName`) reads only the shared
  credentials file.

`useSessionCredentials`, `useIAMCredentials` and `autoDiscoverClient` are accepted for Camel
compatibility and have no effect: session tokens and roles are handled by the rules above.

Differences from the AWS SDKs: only the credentials file is read, not `~/.aws/config` (so the region
and `credential_process` / SSO settings in that file are not used), and on Windows the file is found
through `HOME` or `AWS_SHARED_CREDENTIALS_FILE`.

### Region

`region` on the endpoint, else `AWS_REGION`, else `AWS_DEFAULT_REGION`, else `us-east-1`. Names are
lower-cased and `_` becomes `-`, so Camel-style `EU_WEST_1` works. Requests go to
`https://{service}.{region}.amazonaws.com` (`.amazonaws.com.cn` for `cn-` regions); global services
(IAM) use their global endpoint and sign for `us-east-1`.

### AWS options

Every AWS component accepts these options:

| option | default | description |
|---|---|---|
| `accessKey`, `secretKey` | | Static credentials. Wrap them in `RAW(...)` so `+` and `/` survive. |
| `sessionToken` | | Session token of temporary credentials. |
| `region` | see above | Region. |
| `useDefaultCredentialsProvider` | `false` | Always use the default chain. |
| `useProfileCredentialsProvider` | `false` | Only the shared credentials file. |
| `profileCredentialsName` | | Profile of the credentials file. |
| `uriEndpointOverride` | | Base URL of an emulator or a VPC endpoint. |
| `overrideEndpoint` | `false` | Must be `true` for `uriEndpointOverride` to be used. |
| `proxyHost`, `proxyPort`, `proxyProtocol` | , `3128`, `HTTP` | Explicit proxy. Without it, the usual proxy environment variables apply. |
| `trustAllCertificates` | `false` | Disables certificate checks. Only for local test servers with self-signed certificates; never in production. |

### LocalStack, MinIO and other emulators

Set `uriEndpointOverride` and `overrideEndpoint=true`. LocalStack accepts any key, conventionally
`test` / `test`:

<!-- example: cloud-credentials-localstack -->
```yaml
- route:
    id: localstack-orders
    from:
      uri: "timer:localstack?repeatCount=1"
      steps:
        - setBody:
            constant: "hello from Cameleer"
        - to: "aws2-sqs://orders?region=us-east-1&accessKey=test&secretKey=test&uriEndpointOverride={{env:AWS_ENDPOINT_URL:http://localhost:4566}}&overrideEndpoint=true&autoCreateQueue=true"
```

S3 switches to path-style addressing (`http://host:port/bucket/key`) when the endpoint is
overridden, which is what MinIO and LocalStack expect. Note that Cameleer does not read
`AWS_ENDPOINT_URL` by itself for the components (only for STS web identity); pass it through a
placeholder as above.

## Azure

### Storage accounts (Blob, Queue, Data Lake, Files)

| `credentialType` | what it uses |
|---|---|
| `SHARED_ACCOUNT_KEY` (default when `accessKey` is set) | The base64 account key in `accessKey` (`accountKey` is accepted too). Requests are signed with Shared Key. |
| `AZURE_SAS` (default when only `sasToken` is set) | A SAS token (`sasToken`, with or without the leading `?`), appended to every request. |
| `AZURE_IDENTITY` | Microsoft Entra ID bearer tokens for `https://storage.azure.com/.default`, see below. |
| `CLIENT_SECRET` | Entra ID with an explicit service principal (`tenantId`, `clientId`, `clientSecret`). Setting `clientSecret` without a key or SAS selects it too. |

The account name is the first part of the uri path (`azure-storage-blob://myaccount/container`).
Requests go to `https://{account}.{blob|queue|dfs|file}.core.windows.net`.

### Microsoft Entra ID (service principals, workload identity, managed identity)

`credentialType=AZURE_IDENTITY` resolves like the SDK's `DefaultAzureCredential`. Options on the
endpoint win over the environment:

| order | source | details |
|---|---|---|
| 1 | Client secret | `clientSecret` / `AZURE_CLIENT_SECRET` with `tenantId` / `AZURE_TENANT_ID` and `clientId` / `AZURE_CLIENT_ID`. |
| 2 | Workload identity (AKS, any OIDC federation) | `federatedTokenFile` / `AZURE_FEDERATED_TOKEN_FILE` with the tenant and client id; the token file is read again on every refresh. |
| 3 | Managed identity | `managedIdentityEndpoint`, else the App Service / Functions / Container Apps endpoint (`IDENTITY_ENDPOINT` + `IDENTITY_HEADER`), else the instance metadata service `http://169.254.169.254`. `managedIdentityClientId` (or `clientId` / `AZURE_CLIENT_ID`) selects a user-assigned identity. |

The token endpoint is `{authorityHost}/{tenantId}/oauth2/v2.0/token`, with `authorityHost` from the
option, `AZURE_AUTHORITY_HOST` or `https://login.microsoftonline.com` (set it for sovereign clouds).
Tokens are cached per process until a minute before they expire, and a request that gets `401` is
retried once with a fresh token.

### Other Azure services

| component | credentials |
|---|---|
| [azure-key-vault](../components/azure-key-vault.md), [azure-schema-registry](../components/azure-schema-registry.md) | Entra ID: `CLIENT_SECRET` (default; `tenantId`, `clientId`, `clientSecret`) or `AZURE_IDENTITY`. |
| [azure-cosmosdb](../components/azure-cosmosdb.md) | `accountKey` (master key signature, default) or `credentialType=AZURE_IDENTITY`. |
| [azure-openai](../components/azure-openai.md) | `apiKey`, or Entra ID client credentials. |
| [azure-functions](../components/azure-functions.md) | `functionKey` / `masterKey`, or Entra ID tokens with `scope`. |
| [azure-eventgrid](../components/azure-eventgrid.md) | `accessKey`, a SAS token (`credentialType=AZURE_SAS`), or Entra ID. |
| [azure-servicebus](../components/azure-servicebus.md), [azure-eventhubs](../components/azure-eventhubs.md) | `connectionString`, or namespace + `sharedAccessName` + `sharedAccessKey` (SAS tokens on the AMQP `$cbs` node), or `credentialType=AZURE_IDENTITY` / `TOKEN_CREDENTIAL`. |

### Azure options

Storage components accept `accessKey` (or `accountKey`), `sasToken`, `credentialType`, `tenantId`,
`clientId`, `clientSecret`, `authorityHost`, `federatedTokenFile`, `managedIdentityClientId`,
`managedIdentityEndpoint`, `uriEndpointOverride` + `overrideEndpoint`, `proxyHost` + `proxyPort` and
`trustAllCertificates` (local tests only). The other Azure components accept the subset that applies
to them, listed on their pages.

### Azurite and other emulators

Storage endpoints with `uriEndpointOverride` and `overrideEndpoint=true` use Azurite's path-style
addressing, `http://host:port/{account}/...`. Azurite's development account is `devstoreaccount1`
with a well-known, public key:

<!-- example: cloud-credentials-azurite -->
```yaml
- route:
    id: azurite-upload
    from:
      uri: "timer:azurite?repeatCount=1"
      steps:
        - setBody:
            constant: "hello from Cameleer"
        - setHeader:
            name: CamelAzureStorageBlobBlobName
            constant: hello.txt
        - to: "azure-storage-blob://devstoreaccount1/uploads?operation=uploadBlockBlob&accessKey=RAW(Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==)&uriEndpointOverride={{env:AZURITE_BLOB_URL:http://127.0.0.1:10000}}&overrideEndpoint=true"
```

For Key Vault, Functions, OpenAI and Schema Registry, `uriEndpointOverride` replaces the service base
URL. Service Bus and Event Hubs emulators are reached with a connection string that contains
`UseDevelopmentEmulator=true`.

## Configure once for all endpoints

Every endpoint option can be set as a component default with
`camel.component.<scheme>.<option>`, in `application.properties` or as an environment variable
`CAMEL_COMPONENT_<SCHEME>_<OPTION>` (dashes in the scheme become underscores, the option is written in
upper snake case). Options in the uri win. This keeps route files free of regions and endpoints:

<!-- example: cloud-credentials-component-defaults -->
```yaml
- route:
    id: orders-with-defaults
    from:
      uri: "timer:defaults?repeatCount=1"
      steps:
        - setBody:
            constant: "order o-1001"
        - to: "aws2-sqs://orders?autoCreateQueue=true"
```

```properties
camel.component.aws2-sqs.region=eu-west-1
camel.component.aws2-sqs.uriEndpointOverride=http://localhost:4566
camel.component.aws2-sqs.overrideEndpoint=true
```

The same settings as environment variables:

```sh
export CAMEL_COMPONENT_AWS2_SQS_REGION=eu-west-1
export CAMEL_COMPONENT_AWS2_SQS_URI_ENDPOINT_OVERRIDE=http://localhost:4566
export CAMEL_COMPONENT_AWS2_SQS_OVERRIDE_ENDPOINT=true
```

## Secrets in routes

Instead of writing keys into routes, read them from a vault with placeholders:
`{{aws:name}}` resolves a secret from [AWS Secrets Manager](../components/aws-secrets-manager.md)
(configured with `camel.vault.aws.*`), `{{azure:name}}` one from
[Azure Key Vault](../components/azure-key-vault.md) (`camel.vault.azure.*`), and `{{env:NAME:default}}`
an environment variable. Both vaults can restart routes when a secret changes.

## Running in containers

With the Docker image, pass credentials as environment variables, or rely on the platform's identity:

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  -v "$PWD:/routes:ro" cameleering/cameleer run /routes/orders.camel.yaml
```

On ECS, EKS (IRSA or Pod Identity), EC2, AKS (workload identity), App Service, Container Apps or Azure
VMs, no keys are needed: the container, web identity, instance metadata or managed identity sources
above provide short-lived credentials.

## Common questions

### Why do I get "Unable to load AWS credentials"?

None of the sources had credentials: set `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`, mount a
credentials file and set `AWS_SHARED_CREDENTIALS_FILE`, or run with a role. In containers, check that
`HOME` points to the directory that contains `.aws`.

### Why does my AWS secret key fail with a signature error?

A secret key with `+` or `/` in a uri must be wrapped in `RAW(...)`, otherwise `+` turns into a space.

### Which Azure credential should I use in production?

A managed identity or workload identity with `credentialType=AZURE_IDENTITY`: no secret to rotate.
Use SAS tokens with narrow permissions and short expiry for devices at the edge.

## Related

- [AWS S3](../components/aws2-s3.md), [AWS SQS](../components/aws2-sqs.md), [AWS Secrets Manager](../components/aws-secrets-manager.md)
- [Azure Storage Blob](../components/azure-storage-blob.md), [Azure Key Vault](../components/azure-key-vault.md), [Azure Service Bus](../components/azure-servicebus.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
