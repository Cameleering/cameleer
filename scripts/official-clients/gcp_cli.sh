#!/usr/bin/env bash
# gcloud / gsutil / bq (the Google Cloud SDK CLIs) against Cameleer's GcpMock.
#
# usage: gcp_cli.sh <mock-url> <service-account.json> <ca.pem> <workdir> <check> [check...]
# checks: storage, gsutil, pubsub, bigquery
#
# The CLIs are pointed at the emulator (HTTPS: gsutil's JSON API client only speaks TLS) with
# their endpoint-override settings (CLOUDSDK_API_ENDPOINT_OVERRIDES_*, gsutil's
# gs_json_host/port and provider_token_uri, bq --api) trusting the emulator's certificate, and
# authenticate with the emulator's service account key (JWT bearer grant).
# Prints `key: value` lines and `ok: <check>`; exits 1 if a check failed.
set -u
URL=$1 SA=$2 CA=$3 WORK=$4
shift 4
HOSTPORT=${URL#https://}
export CLOUDSDK_CONFIG=$WORK/gcloud-config
rm -rf "$CLOUDSDK_CONFIG"
mkdir -p "$CLOUDSDK_CONFIG"
# talk to the emulator directly (no corporate proxy, no injected access token)
unset CLOUDSDK_PROXY_TYPE CLOUDSDK_PROXY_ADDRESS CLOUDSDK_PROXY_PORT CLOUDSDK_AUTH_ACCESS_TOKEN HTTPS_PROXY https_proxy HTTP_PROXY http_proxy
export NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost
export CLOUDSDK_CORE_PROJECT=test-project CLOUDSDK_CORE_DISABLE_PROMPTS=1
export CLOUDSDK_API_ENDPOINT_OVERRIDES_STORAGE=$URL/storage/v1/
export CLOUDSDK_API_ENDPOINT_OVERRIDES_PUBSUB=$URL/
export CLOUDSDK_API_ENDPOINT_OVERRIDES_BIGQUERY=$URL/bigquery/v2/
export CLOUDSDK_STORAGE_USE_GRPC_IF_AVAILABLE=false
# token refreshes (e.g. for the scopes bq asks gcloud for) go to the emulator too
export CLOUDSDK_AUTH_TOKEN_HOST=$URL/token
export CLOUDSDK_CORE_CUSTOM_CA_CERTS_FILE=$CA REQUESTS_CA_BUNDLE=$CA SSL_CERT_FILE=$CA
FAILED=0
G() { timeout 120 gcloud "$@"; }

echo "version: $(gcloud version 2>/dev/null | head -1)"
G auth activate-service-account --key-file="$SA" >/dev/null 2>&1 && echo "auth: activated" || { echo "auth: failed"; G auth activate-service-account --key-file="$SA"; exit 1; }

check_storage() {
  cd "$WORK" || return 1
  printf 'hello gcs\n' > hello.txt
  head -c 3000000 /dev/urandom > big.bin
  G storage buckets create gs://official-cli || return 1
  echo "storage-buckets: $(G storage buckets list --format='value(name)' | tr '\n' ' ')"
  G storage cp hello.txt gs://official-cli/dir/hello.txt || return 1
  G storage cp big.bin gs://official-cli/big.bin || return 1
  echo "storage-cat: $(G storage cat gs://official-cli/dir/hello.txt)"
  echo "storage-ls: $(G storage ls gs://official-cli/ | tr '\n' ' ')"
  echo "storage-ls-recursive: $(G storage ls -r 'gs://official-cli/**' | tr '\n' ' ')"
  G storage cp gs://official-cli/big.bin big2.bin || return 1
  cmp -s big.bin big2.bin && echo "storage-roundtrip: same" || echo "storage-roundtrip: different"
  echo "storage-describe: $(G storage objects describe gs://official-cli/dir/hello.txt --format='value(size,content_type)')"
  G storage objects update gs://official-cli/dir/hello.txt --custom-metadata=owner=gcloud >/dev/null || return 1
  echo "storage-metadata: $(G storage objects describe gs://official-cli/dir/hello.txt --format='value(custom_fields.owner)')"
  G storage cp gs://official-cli/dir/hello.txt gs://official-cli/copy.txt >/dev/null 2>&1 || return 1
  G storage mv gs://official-cli/copy.txt gs://official-cli/moved.txt >/dev/null 2>&1 || return 1
  G storage rm gs://official-cli/moved.txt >/dev/null 2>&1 || return 1
  echo "storage-after-rm: $(G storage ls gs://official-cli/ | tr '\n' ' ')"
  echo "storage-missing: $(G storage cat gs://official-cli/nope 2>&1 | grep -o 'matched no objects' | head -1)"
}

check_gsutil() {
  cd "$WORK" || return 1
  local B=(-o "Credentials:gs_json_host=${HOSTPORT%:*}" -o "Credentials:gs_json_port=${HOSTPORT#*:}" -o "Boto:ca_certificates_file=$CA" -o "GSUtil:default_project_id=test-project" -o "OAuth2:provider_token_uri=$URL/token" -o "GSUtil:check_hashes=always" -o "GSUtil:use_gcloud_storage=False")
  printf 'via gsutil\n' > g.txt
  timeout 120 gsutil "${B[@]}" mb gs://official-gsutil 2>&1 | tail -1
  timeout 120 gsutil "${B[@]}" cp g.txt gs://official-gsutil/g.txt 2>&1 | tail -1
  echo "gsutil-cat: $(timeout 60 gsutil "${B[@]}" cat gs://official-gsutil/g.txt)"
  echo "gsutil-ls: $(timeout 60 gsutil "${B[@]}" ls gs://official-gsutil | tr '\n' ' ')"
  echo "gsutil-stat: $(timeout 60 gsutil "${B[@]}" stat gs://official-gsutil/g.txt | grep -E 'Content-Length|Hash \(md5\)' | tr -s ' ' | tr '\n' ' ')"
  timeout 60 gsutil "${B[@]}" rm gs://official-gsutil/g.txt 2>&1 | tail -1
  echo "gsutil-after-rm: $(timeout 60 gsutil "${B[@]}" ls gs://official-gsutil | wc -l)"
}

check_pubsub() {
  G pubsub topics create official-topic >/dev/null || return 1
  G pubsub subscriptions create official-sub --topic=official-topic --ack-deadline=30 >/dev/null || return 1
  echo "pubsub-topics: $(G pubsub topics list --format='value(name)' | tr '\n' ' ')"
  echo "pubsub-publish: $(G pubsub topics publish official-topic --message='hello pubsub' --attribute=origin=gcloud,kind=test --format='value(messageIds)' | wc -w)"
  G pubsub topics publish official-topic --message='second' >/dev/null || return 1
  echo "pubsub-pull: $(G pubsub subscriptions pull official-sub --auto-ack --limit=10 --format='value(message.data,message.attributes.origin)' | sort | tr '\t\n' '/ ')"
  echo "pubsub-empty: $(G pubsub subscriptions pull official-sub --auto-ack --limit=10 --format='value(message.data)' | wc -l)"
  echo "pubsub-describe: $(G pubsub subscriptions describe official-sub --format='value(ackDeadlineSeconds,topic)')"
  G pubsub subscriptions delete official-sub >/dev/null || return 1
  G pubsub topics delete official-topic >/dev/null || return 1
  echo "pubsub-after-delete: $(G pubsub topics list --format='value(name)' | wc -l)"
}

check_bigquery() {
  cd "$WORK" || return 1
  # the BigQuery discovery document shipped with bq (the emulator serves the REST API only)
  local DISC; DISC=$(dirname "$(readlink -f "$(command -v bq)")")/../platform/bq/discovery_next/bigquery.json
  [ -f "$DISC" ] || DISC=$(find / -path "*platform/bq/discovery_next/bigquery.json" 2>/dev/null | head -1)
  local BQ=(timeout 120 bq --api="$URL" --ca_certificates_file="$CA" --discovery_file="$DISC" --project_id=test-project --quiet --headless)
  "${BQ[@]}" mk --dataset test-project:official >/dev/null || return 1
  "${BQ[@]}" mk --table official.people name:STRING,age:INTEGER,city:STRING >/dev/null || return 1
  printf '{"name":"ada","age":36,"city":"london"}\n{"name":"alan","age":41,"city":"london"}\n{"name":"grace","age":85,"city":"nyc"}\n' > rows.json
  "${BQ[@]}" insert official.people rows.json >/dev/null || return 1
  echo "bq-ls: $("${BQ[@]}" ls official | tail -n +3 | awk '{print $1 "/" $2}' | tr '\n' ' ')"
  echo "bq-show: $("${BQ[@]}" show --format=json official.people | grep -o '"name": *"[a-z]*"' | tr -d ' "' | tr '\n' ' ')"
  echo "bq-query: $("${BQ[@]}" query --use_legacy_sql=false --format=csv 'SELECT name FROM official.people WHERE city = "london" ORDER BY age DESC' | tail -n +2 | tr '\n' ' ')"
  echo "bq-head: $("${BQ[@]}" head --format=csv -n 10 official.people | tail -n +2 | wc -l)"
  "${BQ[@]}" rm -f -t official.people >/dev/null || return 1
  "${BQ[@]}" rm -f -d official >/dev/null || return 1
  echo "bq-after-rm: $("${BQ[@]}" ls | tail -n +3 | wc -l)"
}

for c in "$@"; do
  if "check_$c"; then echo "ok: $c"; else echo "FAIL $c"; FAILED=1; fi
done
exit $FAILED
