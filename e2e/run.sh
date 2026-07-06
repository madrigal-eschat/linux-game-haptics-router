#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
WORK_DIR="$(mktemp -d)"

QEMU_PID=""
cleanup() {
    if [ -n "$QEMU_PID" ] && kill -0 "$QEMU_PID" 2>/dev/null; then
        kill "$QEMU_PID" 2>/dev/null || true
        wait "$QEMU_PID" 2>/dev/null || true
    fi
    rm -rf "$WORK_DIR"
}
trap cleanup EXIT

ARCH="$(uname -m)"

case "$ARCH" in
    x86_64)  QEMU_BIN=qemu-system-x86_64; CLOUD_IMAGE_URL="https://cloud-images.ubuntu.com/noble/current/noble-server-cloudimg-amd64.img" ;;
    aarch64) QEMU_BIN=qemu-system-aarch64; CLOUD_IMAGE_URL="https://cloud-images.ubuntu.com/noble/current/noble-server-cloudimg-arm64.img" ;;
    *) echo "unsupported host arch: $ARCH" >&2; exit 1 ;;
esac

CACHE_DIR="${E2E_CACHE_DIR:-$HOME/.cache/linux-game-haptics-router-e2e}"
mkdir -p "$CACHE_DIR"
BASE_IMAGE="$CACHE_DIR/base-$ARCH.img"

if [ ! -f "$BASE_IMAGE" ]; then
    echo "downloading base cloud image for $ARCH..."
    curl -fL -o "$BASE_IMAGE.tmp" "$CLOUD_IMAGE_URL"
    mv "$BASE_IMAGE.tmp" "$BASE_IMAGE"
fi

echo "building release binaries..."
(cd "$REPO_ROOT" && cargo build --release --workspace --exclude linux-game-haptics-router-ebpf)
(cd "$REPO_ROOT" && cargo build --release -p linux-game-haptics-router-e2e --bin e2e-tests)

SSH_KEY="$WORK_DIR/id_ed25519"
ssh-keygen -t ed25519 -N "" -f "$SSH_KEY" -q

OVERLAY="$WORK_DIR/overlay.qcow2"
qemu-img create -f qcow2 -F qcow2 -b "$BASE_IMAGE" "$OVERLAY" >/dev/null

sed "s#__SSH_PUBKEY__#$(cat "$SSH_KEY.pub")#" \
    "$SCRIPT_DIR/cloud-init/user-data.tmpl" > "$WORK_DIR/user-data"
cp "$SCRIPT_DIR/cloud-init/meta-data" "$WORK_DIR/meta-data"

SEED_ISO="$WORK_DIR/seed.iso"
if command -v cloud-localds >/dev/null; then
    cloud-localds "$SEED_ISO" "$WORK_DIR/user-data" "$WORK_DIR/meta-data"
else
    genisoimage -output "$SEED_ISO" -volid cidata -joliet -rock \
        "$WORK_DIR/user-data" "$WORK_DIR/meta-data"
fi

SSH_PORT=10222
"$QEMU_BIN" \
    -m 2048 -smp 2 -enable-kvm -nographic \
    -drive file="$OVERLAY",if=virtio,format=qcow2 \
    -drive file="$SEED_ISO",if=virtio,format=raw \
    -netdev user,id=net0,hostfwd=tcp::"$SSH_PORT"-:22 \
    -device virtio-net-pci,netdev=net0 \
    >"$WORK_DIR/qemu.log" 2>&1 &
QEMU_PID=$!

echo "waiting for SSH..."
SSH_OPTS=(-i "$SSH_KEY" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -p "$SSH_PORT")
for _ in $(seq 1 60); do
    if ssh "${SSH_OPTS[@]}" e2e@127.0.0.1 true 2>/dev/null; then
        break
    fi
    sleep 2
done
if ! ssh "${SSH_OPTS[@]}" e2e@127.0.0.1 true 2>/dev/null; then
    echo "VM never became SSH-reachable" >&2
    exit 1
fi

echo "copying binaries into VM..."
scp "${SSH_OPTS[@]}" \
    "$REPO_ROOT/target/release/game-haptics-router" \
    "$REPO_ROOT/target/release/e2e-tests" \
    e2e@127.0.0.1:~/

echo "running e2e-tests in VM..."
set +e
timeout 300 ssh "${SSH_OPTS[@]}" e2e@127.0.0.1 'sudo ./e2e-tests'
RESULT=$?
set -e

exit "$RESULT"
