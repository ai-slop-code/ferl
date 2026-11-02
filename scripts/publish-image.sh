#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
: "${CANDIDATE_DIR:?set CANDIDATE_DIR to the downloaded candidate directory}"
: "${EXPECTED_VERSION:?set EXPECTED_VERSION}"
: "${EXPECTED_REVISION:?set EXPECTED_REVISION}"
: "${REGISTRY_AUTH_FILE:?set REGISTRY_AUTH_FILE under RUNNER_TEMP}"
: "${GHCR_USER:?set GHCR_USER}"
: "${GHCR_TOKEN:?set GHCR_TOKEN from the workflow token}"
: "${GITHUB_OUTPUT:?must run inside GitHub Actions}"

REPOSITORY=ai-slop-code/ferl
REGISTRY_REPOSITORY="ghcr.io/$REPOSITORY"
METADATA="$CANDIDATE_DIR/image-metadata.json"
PUBLISH_DIR="$CANDIDATE_DIR/publication"
mkdir -p "$PUBLISH_DIR"

python3 - "$METADATA" "$EXPECTED_VERSION" "$EXPECTED_REVISION" "$CANDIDATE_DIR" <<'PY'
import hashlib
import json
from pathlib import Path
import sys

metadata_path, version, revision, directory = sys.argv[1:]
metadata = json.loads(Path(metadata_path).read_text())
if metadata["version"] != version:
    raise SystemExit("candidate version does not match the release tag")
if metadata["source_revision"] != revision:
    raise SystemExit("candidate source revision does not match the workflow commit")
if metadata["platform"] != "linux/amd64":
    raise SystemExit("candidate platform is not linux/amd64")
archive = Path(directory) / metadata["image_archive"]
actual = hashlib.sha256(archive.read_bytes()).hexdigest()
if actual != metadata["image_archive_sha256"]:
    raise SystemExit("candidate image archive checksum does not match its metadata")
PY

ARCHIVE_NAME=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["image_archive"])' "$METADATA")
IMAGE_ID=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["image_id"])' "$METADATA")
MANIFEST_DIGEST=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["manifest_digest"])' "$METADATA")

LOADED=$(podman load --quiet -i "$CANDIDATE_DIR/$ARCHIVE_NAME" | tail -n 1)
LOCAL_TAG="localhost/ferl-publish:$EXPECTED_REVISION"
podman tag "$LOADED" "$LOCAL_TAG"
[ "$(podman image inspect --format '{{.Id}}' "$LOCAL_TAG")" = "$IMAGE_ID" ] || {
    echo "loaded image ID differs from the accepted candidate" >&2
    exit 1
}

printf '%s' "$GHCR_TOKEN" | podman login ghcr.io --username "$GHCR_USER" --password-stdin >/dev/null

python3 - "$REPOSITORY" "$EXPECTED_VERSION" "$MANIFEST_DIGEST" <<'PY'
import base64
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request

repository, version, manifest_digest = sys.argv[1:]
user = os.environ["GHCR_USER"]
token = os.environ["GHCR_TOKEN"]
scope = f"repository:{repository}:pull,push"
auth = base64.b64encode(f"{user}:{token}".encode()).decode()
query = urllib.parse.urlencode({"service": "ghcr.io", "scope": scope})
request = urllib.request.Request(f"https://ghcr.io/token?{query}", headers={"Authorization": f"Basic {auth}"})
with urllib.request.urlopen(request, timeout=30) as response:
    bearer = json.load(response)["token"]

accept = ", ".join([
    "application/vnd.oci.image.manifest.v1+json",
    "application/vnd.oci.image.index.v1+json",
    "application/vnd.docker.distribution.manifest.v2+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
])
request = urllib.request.Request(
    f"https://ghcr.io/v2/{repository}/manifests/v{version}",
    method="HEAD",
    headers={"Authorization": f"Bearer {bearer}", "Accept": accept},
)
try:
    with urllib.request.urlopen(request, timeout=30) as response:
        existing = response.headers.get("Docker-Content-Digest", "")
except urllib.error.HTTPError as error:
    if error.code != 404:
        raise SystemExit(f"registry tag check failed with HTTP {error.code}")
    existing = ""

if existing and existing != manifest_digest:
    raise SystemExit(f"tag v{version} already names {existing}; refusing to overwrite it with {manifest_digest}")
print("version-tag-state=" + ("resume" if existing else "new"))
PY

push_tag() {
    tag=$1
    digest_file="$PUBLISH_DIR/digest-$(printf '%s' "$tag" | tr ':/' '__').txt"
    podman push --format oci --digestfile "$digest_file" "$LOCAL_TAG" "docker://$REGISTRY_REPOSITORY:$tag" >/dev/null
    pushed=$(tr -d '[:space:]' < "$digest_file")
    [ "$pushed" = "$MANIFEST_DIGEST" ] || {
        echo "registry digest for $tag ($pushed) differs from the accepted candidate ($MANIFEST_DIGEST)" >&2
        exit 1
    }
}

push_tag "sha-$EXPECTED_REVISION"
push_tag "v$EXPECTED_VERSION"

python3 - "$REPOSITORY" "$MANIFEST_DIGEST" "$EXPECTED_VERSION" "$EXPECTED_REVISION" "$PUBLISH_DIR/publication.json" <<'PY'
import base64
import json
import os
import sys
import urllib.parse
import urllib.request

repository, digest, version, revision, output = sys.argv[1:]
user = os.environ["GHCR_USER"]
token = os.environ["GHCR_TOKEN"]
auth = base64.b64encode(f"{user}:{token}".encode()).decode()
query = urllib.parse.urlencode({"service": "ghcr.io", "scope": f"repository:{repository}:pull"})
request = urllib.request.Request(f"https://ghcr.io/token?{query}", headers={"Authorization": f"Basic {auth}"})
with urllib.request.urlopen(request, timeout=30) as response:
    bearer = json.load(response)["token"]
request = urllib.request.Request(
    f"https://ghcr.io/v2/{repository}/manifests/{digest}",
    method="HEAD",
    headers={"Authorization": f"Bearer {bearer}", "Accept": "application/vnd.oci.image.manifest.v1+json, application/vnd.oci.image.index.v1+json"},
)
with urllib.request.urlopen(request, timeout=30) as response:
    if response.headers.get("Docker-Content-Digest") != digest:
        raise SystemExit("registry did not return the published digest")
record = {
    "repository": f"ghcr.io/{repository}",
    "digest": digest,
    "reference": f"ghcr.io/{repository}@{digest}",
    "tags": [f"v{version}", f"sha-{revision}"],
    "version": version,
    "source_revision": revision,
}
with open(output, "w", encoding="utf-8") as stream:
    json.dump(record, stream, indent=2, sort_keys=True)
    stream.write("\n")
PY

podman rmi -f "$LOCAL_TAG" >/dev/null 2>&1 || true
{
    echo "digest=$MANIFEST_DIGEST"
    echo "reference=$REGISTRY_REPOSITORY@$MANIFEST_DIGEST"
} >> "$GITHUB_OUTPUT"
echo "published $REGISTRY_REPOSITORY@$MANIFEST_DIGEST"
