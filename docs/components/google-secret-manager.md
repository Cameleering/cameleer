# Google Secret Manager component

> The `google-secret-manager` component creates, reads, versions, lists and deletes secrets in Google Cloud Secret Manager, and the `{{gcp:name}}` placeholder reads secrets into route configuration.

**Scheme:** `google-secret-manager:` · **Crate:** `cameleer-cloud` ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [Google Secret Manager](https://camel.apache.org/components/next/google-secret-manager-component.html)

## When to use it

- Keep API keys, database passwords and tokens out of route files: write `{{gcp:db-password}}`.
- Rotate a secret on a schedule by adding a new version.
- Read a secret at runtime, for example to hand a short-lived credential to an AI agent tool.

## URI format

```text
google-secret-manager://projectId[?options]
```

Without a project in the URI, the project of the service account key is used.

## Example

Add a new random version of a secret once a day:

<!-- example: google-secret-manager-rotate -->
```yaml
- route:
    id: rotate-webhook-token
    from:
      uri: "timer:rotate?period=86400000"
      steps:
        - setHeader:
            name: CamelGoogleSecretManagerSecretId
            constant: "{{env:SECRET_ID:webhook-token}}"
        - setBody:
            simple: "${uuid}"
        - to: "google-secret-manager://{{env:GCP_PROJECT:my-project}}?operation=addSecretVersion"
        - log: "added ${body}"
```

Read the latest version of a secret and use it as a header for the next step:

<!-- example: google-secret-manager-read -->
```yaml
- route:
    id: read-api-key
    from:
      uri: "timer:read-key?repeatCount=1"
      steps:
        - setHeader:
            name: CamelGoogleSecretManagerSecretId
            constant: "{{env:SECRET_ID:api-key}}"
        - to: "google-secret-manager://{{env:GCP_PROJECT:my-project}}"
        - setHeader:
            name: X-Api-Key
            simple: "${body}"
        - setBody:
            constant: "key loaded"
        - to: "file:{{env:OUT_DIR:out}}?fileName=status.txt"
```

### Secrets as placeholders

With these properties in `application.properties`, any route can use `{{gcp:name}}`, `{{gcp:name/field}}` (a field of a JSON
secret), `{{gcp:name@version}}` and `{{gcp:name:default}}`:

```properties
camel.vault.gcp.projectId=my-project
camel.vault.gcp.serviceAccountKey=file:/keys/sa.json
# or: camel.vault.gcp.useDefaultInstance=true  (uses GOOGLE_APPLICATION_CREDENTIALS)
```

```text
- to: "https://api.example.com/orders?apiKey={{gcp:api-key}}"
- setBody:
    simple: "{{gcp:db/password}}"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("timer:read-key?repeatCount=1")
        .set_header("CamelGoogleSecretManagerSecretId", constant("api-key"))
        .to("google-secret-manager://my-project")
        .log("secret length ${body.length()}");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `operation` | `getSecretVersion` | `getSecretVersion`, `createSecret`, `addSecretVersion` (Cameleer), `deleteSecret`, `listSecrets`. Header `CamelGoogleSecretManagerOperation`. |

`client` is accepted for Camel compatibility and ignored. Credentials are shared by all Google
components: see [credentials](google-storage.md#credentials).

### Operations

- `getSecretVersion`: body = the payload (text) of `CamelGoogleSecretManagerVersionId`,
  default `latest`.
- `createSecret`: creates the secret (automatic replication) with the body as version 1;
  body = the version name `projects/p/secrets/s/versions/1`.
- `addSecretVersion`: adds the body as a new version.
- `deleteSecret`: deletes the secret and all versions. `listSecrets`: body = list of secret ids.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelGoogleSecretManagerOperation` | in | Operation. |
| `CamelGoogleSecretManagerSecretId` | in | Secret id (required by all operations except `listSecrets`). |
| `CamelGoogleSecretManagerVersionId` | in | Version for `getSecretVersion` (default `latest`). |

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" \
  -v "$HOME/keys/sa.json:/keys/sa.json:ro" -e GOOGLE_APPLICATION_CREDENTIALS=/keys/sa.json \
  -e GCP_PROJECT=my-project -e SECRET_ID=webhook-token \
  pmsudhi/cameleer run /routes/rotate-webhook-token.camel.yaml
```

## Common questions

### How do I use a Google secret in an endpoint URI?
Configure `camel.vault.gcp.projectId` and credentials, then write `{{gcp:secret-name}}` in the
URI. The secret is read when the route starts.

### How do I read one field of a JSON secret?
Use `{{gcp:name/field}}`, for example `{{gcp:db/password}}` for a secret `{"user":..,"password":..}`.

### How do I read an older version?
Set `CamelGoogleSecretManagerVersionId` (producer) or write `{{gcp:name@3}}` (placeholder).

## Related

- [AWS Secrets Manager](aws-secrets-manager.md), [Azure Key Vault](azure-key-vault.md)
- [Apache Camel Google Secret Manager](https://camel.apache.org/components/next/google-secret-manager-component.html)
