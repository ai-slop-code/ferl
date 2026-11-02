#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
ARCHIVE=${1:-${FERL_RELEASE_ARCHIVE:-}}
[ -n "$ARCHIVE" ] || { echo "usage: build-image.sh <ferl-server-VERSION-TARGET.tar.gz>" >&2; exit 2; }
TARGET=${FERL_RELEASE_TARGET:-x86_64-unknown-linux-gnu}
case "$TARGET" in
    x86_64-unknown-linux-gnu) PLATFORM=linux/amd64 ;;
    *) echo "unsupported image target '$TARGET'; the delivery target is x86_64-unknown-linux-gnu" >&2; exit 2 ;;
esac
VERSION=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' "$ROOT/Cargo.toml" | head -n 1)
REVISION=$(git -C "$ROOT" rev-parse HEAD)
OUT=${FERL_IMAGE_OUTPUT_DIR:-"$ROOT/target/image-candidate"}
RUN_ID=$(python3 -c 'import uuid; print(uuid.uuid4().hex)')
BASE_IMAGE=$(sed -n 's/^FROM \([^ ]*\).*/\1/p' "$ROOT/Dockerfile" | head -n 1)
IMAGE_TAG="ferl-candidate:$RUN_ID"
ARCHIVE_NAME="ferl-image-$VERSION-$TARGET.oci.tar"
STAGE="$ROOT/target/image-stage-$RUN_ID"

[ -n "$VERSION" ] && [ -n "$BASE_IMAGE" ] || { echo "cannot read package version or base image" >&2; exit 1; }
[ -z "$(git -C "$ROOT" status --porcelain --untracked-files=normal)" ] || {
    echo "image candidates require a clean committed source tree" >&2
    exit 1
}

cleanup() {
    rm -rf "$STAGE"
    podman image exists "$IMAGE_TAG" 2>/dev/null && podman rmi -f "$IMAGE_TAG" >/dev/null 2>&1 || true
}
trap cleanup EXIT

umask 022
mkdir -p "$OUT" "$STAGE/context/bin"
OUT=$(CDPATH= cd -- "$OUT" && pwd)

python3 - "$ARCHIVE" <<'PY'
import hashlib
from pathlib import Path
import sys

archive = Path(sys.argv[1])
sidecar = archive.with_name(archive.name + ".sha256")
expected = sidecar.read_text().split()[0]
if hashlib.sha256(archive.read_bytes()).hexdigest() != expected:
    raise SystemExit("release archive does not match its checksum sidecar")
PY

python3 "$SCRIPT_DIR/verify-release.py" \
    --extract-to "$STAGE/release" \
    --archive "$ARCHIVE" \
    --source-revision "$REVISION" \
    --target "$TARGET" \
    --openapi "$ROOT/spec/openapi.yaml" \
    --require-clean-source
python3 "$SCRIPT_DIR/verify-release.py" \
    --root "$STAGE/release" \
    --archive "$ARCHIVE" \
    --source-revision "$REVISION" \
    --target "$TARGET" \
    --openapi "$ROOT/spec/openapi.yaml" \
    --require-clean-source

cp "$ROOT/Dockerfile" "$ROOT/.dockerignore" "$STAGE/context/"
cp "$STAGE/release/bin/ferl-server" "$STAGE/context/bin/ferl-server"
chmod 0755 "$STAGE/context/bin/ferl-server"

podman info >/dev/null
podman pull --platform "$PLATFORM" "$BASE_IMAGE" >/dev/null
podman build \
    --platform "$PLATFORM" \
    --pull=never \
    --network none \
    --build-arg "FERL_VERSION=$VERSION" \
    --build-arg "FERL_REVISION=$REVISION" \
    --tag "$IMAGE_TAG" \
    --file "$STAGE/context/Dockerfile" \
    "$STAGE/context"

ACTUAL_PLATFORM=$(podman image inspect --format '{{.Os}}/{{.Architecture}}' "$IMAGE_TAG")
[ "$ACTUAL_PLATFORM" = "$PLATFORM" ] || {
    echo "built image platform is $ACTUAL_PLATFORM, expected $PLATFORM" >&2
    exit 1
}

SCAN_NAME="ferl-scan-$RUN_ID"
podman create --name "$SCAN_NAME" --label "io.ferl.image-stage=$RUN_ID" "$IMAGE_TAG" >/dev/null
podman export "$SCAN_NAME" | python3 -c '
import re
import sys
import tarfile

patterns = [
    re.compile(rb"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
    re.compile(rb"gh[pousr]_[A-Za-z0-9]{36}"),
    re.compile(rb"github_pat_[A-Za-z0-9_]{22,}"),
    re.compile(rb"AKIA[0-9A-Z]{16}"),
    re.compile(rb"xox[baprs]-[A-Za-z0-9-]{10,}"),
]
hits = 0
with tarfile.open(fileobj=sys.stdin.buffer, mode="r|") as archive:
    for member in archive:
        if not member.isfile():
            continue
        stream = archive.extractfile(member)
        data = stream.read() if stream else b""
        for pattern in patterns:
            if pattern.search(data):
                hits += 1
if hits:
    raise SystemExit(f"image filesystem contains {hits} credential-shaped match(es); candidate rejected")
' || { podman rm -f "$SCAN_NAME" >/dev/null 2>&1 || true; echo "image content scan failed" >&2; exit 1; }
podman rm -f "$SCAN_NAME" >/dev/null

podman save --format oci-archive --output "$OUT/$ARCHIVE_NAME" "$IMAGE_TAG"
IMAGE_ID=$(podman image inspect --format '{{.Id}}' "$IMAGE_TAG")

python3 - "$OUT" "$ARCHIVE_NAME" "$ARCHIVE" "$VERSION" "$REVISION" "$TARGET" "$PLATFORM" "$BASE_IMAGE" "$IMAGE_ID" "$RUN_ID" "$STAGE/release/bin/ferl-server" <<'PY'
import hashlib
import json
from pathlib import Path
import sys
import tarfile

(out, archive_name, release_archive, version, revision, target, platform,
 base_image, image_id, run_id, binary) = sys.argv[1:]
out = Path(out)
image_archive = out / archive_name


def sha256_file(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


with tarfile.open(image_archive) as tar:
    index = json.load(tar.extractfile("index.json"))
manifest_digest = index["manifests"][0]["digest"]
if not manifest_digest.startswith("sha256:"):
    raise SystemExit("OCI archive index does not name a sha256 manifest")

image_archive_sha256 = sha256_file(image_archive)
sidecar = out / f"{archive_name}.sha256"
sidecar.write_text(f"{image_archive_sha256}  {archive_name}\n")

metadata = {
    "run_id": run_id,
    "version": version,
    "source_revision": revision,
    "target": target,
    "platform": platform,
    "base_image": base_image,
    "release_archive_sha256": sha256_file(Path(release_archive)),
    "binary_sha256": sha256_file(Path(binary)),
    "image_archive": archive_name,
    "image_archive_sha256": image_archive_sha256,
    "image_id": image_id,
    "manifest_digest": manifest_digest,
    "content_scan": "passed",
}
temporary = out / f".image-metadata.{run_id}.tmp"
temporary.write_text(json.dumps(metadata, indent=2, sort_keys=True) + "\n")
temporary.replace(out / "image-metadata.json")
PY

echo "built $OUT/image-metadata.json for $PLATFORM from $ARCHIVE_NAME"
