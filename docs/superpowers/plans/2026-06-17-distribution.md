# flannel-rs Distribution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish flannel-rs as a multi-arch GHCR image and per-arch static-musl binary tarballs, via a tag-triggered GitHub Actions release workflow.

**Architecture:** Cross-compile static musl binaries (amd64 + arm64) with `cargo-zigbuild` on one runner; package them as checksummed tarballs (GitHub Release) and `COPY` them into a minimal per-arch image built with `docker buildx` and pushed to GHCR. A `workflow_dispatch` dry-run path builds everything without publishing. The existing in-image `Dockerfile` and `ci.yml` test path are untouched.

**Tech Stack:** Rust + `cargo-zigbuild` (musl cross), `docker buildx` (multi-arch), GitHub Actions, GHCR.

---

## Pre-flight: standing rule

Before ANY `git push`, run the full local gate (fmt/clippy/build/test) and push only if green. CI/workflow YAML doesn't affect the Rust gate, but the musl build added in Task 1 must compile.

---

## File Structure

```
Dockerfile.release          # NEW: COPY prebuilt static binaries (arch-arg); minimal runtime
.github/workflows/release.yml  # NEW: build musl binaries -> tarballs + multi-arch image
deploy/flannel-rs-release.yaml # NEW: DaemonSet pointing at the GHCR image (default pull policy)
```
Unchanged: `Dockerfile` (local dev `flannel-rs:dev`), `.github/workflows/ci.yml`, `tests/`, `deploy/flannel-rs.yaml`.

---

## Task 1: Verify static-musl build + Dockerfile.release

**Files:**
- Create: `Dockerfile.release`

- [ ] **Step 1: Confirm the workspace cross-compiles to static musl (amd64)**

This de-risks the whole effort before touching CI. Install the toolchain and build:
```bash
rustup target add x86_64-unknown-linux-musl
cargo install cargo-zigbuild --locked
pip install ziglang==0.13.0   # provides the `zig` cross-linker cargo-zigbuild uses
cargo zigbuild --release --target x86_64-unknown-linux-musl \
  -p flanneld -p cni-host-local -p cni-flannel -p cni-bridge -p cni-portmap
```
Expected: builds all five binaries. Verify they are static:
```bash
file target/x86_64-unknown-linux-musl/release/flanneld
file target/x86_64-unknown-linux-musl/release/portmap
```
Expected: each reports `ELF 64-bit ... statically linked`.
If a dependency fails to build for musl, STOP and report it (the spec's known risk) — do not proceed to CI tasks until the local musl build is green.

- [ ] **Step 2: Write Dockerfile.release**

`Dockerfile.release`:
```dockerfile
# Release image: copies prebuilt static binaries staged under dist/<arch>/.
# Built per-arch by `docker buildx` (TARGETARCH is set automatically). No cargo
# build here — the binaries are produced by the release workflow's musl build.
FROM debian:bookworm-slim
ARG TARGETARCH
# flanneld shells out to iptables; iproute2 + ca-certificates for runtime.
RUN apt-get update \
    && apt-get install -y --no-install-recommends iproute2 iptables ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY dist/${TARGETARCH}/flanneld   /usr/local/bin/flanneld
COPY dist/${TARGETARCH}/flannel    /opt/cni/bin/flannel
COPY dist/${TARGETARCH}/bridge     /opt/cni/bin/bridge
COPY dist/${TARGETARCH}/host-local /opt/cni/bin/host-local
COPY dist/${TARGETARCH}/portmap    /opt/cni/bin/portmap
ENTRYPOINT ["/usr/local/bin/flanneld"]
```

- [ ] **Step 3: Validate Dockerfile.release locally (amd64, single arch)**

Stage the amd64 binaries where the Dockerfile expects them and build:
```bash
mkdir -p dist/amd64
cp target/x86_64-unknown-linux-musl/release/{flanneld,flannel,bridge,host-local,portmap} dist/amd64/
docker build -f Dockerfile.release -t flannel-rs:release-test --build-arg TARGETARCH=amd64 .
docker run --rm --entrypoint ls flannel-rs:release-test \
  /usr/local/bin/flanneld /opt/cni/bin/flannel /opt/cni/bin/bridge /opt/cni/bin/host-local /opt/cni/bin/portmap
rm -rf dist   # dist/ is a build-time staging dir, never committed
```
Expected: image builds; all five paths exist.

- [ ] **Step 4: Ignore the staging dir + commit**

Append to `.gitignore`:
```
/dist
```
Then:
```bash
git add Dockerfile.release .gitignore
git commit -m "build: Dockerfile.release for prebuilt static binaries"
```

---

## Task 2: Release workflow — build musl binaries + tarballs

**Files:**
- Create: `.github/workflows/release.yml` (build job only in this task)

- [ ] **Step 1: Create the workflow with the build job**

`.github/workflows/release.yml`:
```yaml
name: release

on:
  push:
    tags: ["v*"]
  workflow_dispatch:
    inputs:
      publish:
        description: "Publish Release + push image to GHCR (false = dry run)"
        type: boolean
        default: false

permissions:
  contents: write   # create GitHub Release
  packages: write   # push to GHCR

env:
  CARGO_TERM_COLOR: always

jobs:
  build:
    name: build static musl binaries
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5

      - name: Install Rust + musl targets
        uses: dtolnay/rust-toolchain@stable
        with:
          targets: x86_64-unknown-linux-musl, aarch64-unknown-linux-musl

      - name: Install zig + cargo-zigbuild
        run: |
          pip install ziglang==0.13.0
          cargo install cargo-zigbuild --locked

      - name: Build (amd64 + arm64, static musl)
        run: |
          for t in x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
            cargo zigbuild --release --target "$t" \
              -p flanneld -p cni-host-local -p cni-flannel -p cni-bridge -p cni-portmap
          done

      - name: Stage binaries + tarballs
        id: pkg
        run: |
          VERSION="${GITHUB_REF_NAME}"
          [ "${{ github.event_name }}" = "workflow_dispatch" ] && VERSION="dev-${GITHUB_SHA::8}"
          echo "version=$VERSION" >> "$GITHUB_OUTPUT"
          mkdir -p dist/amd64 dist/arm64 out
          cp target/x86_64-unknown-linux-musl/release/{flanneld,flannel,bridge,host-local,portmap} dist/amd64/
          cp target/aarch64-unknown-linux-musl/release/{flanneld,flannel,bridge,host-local,portmap} dist/arm64/
          tar -czf "out/flannel-rs_${VERSION}_linux_amd64.tar.gz" -C dist/amd64 .
          tar -czf "out/flannel-rs_${VERSION}_linux_arm64.tar.gz" -C dist/arm64 .
          ( cd out && sha256sum *.tar.gz > "flannel-rs_${VERSION}_checksums.txt" )
          ls -l out

      - name: Upload build artifacts
        uses: actions/upload-artifact@v4
        with:
          name: flannel-rs-binaries
          path: |
            out/
            dist/
          retention-days: 7
```

- [ ] **Step 2: Lint the YAML**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml'))" && echo "YAML OK"`
Expected: `YAML OK`.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci: release workflow — build static musl binaries + tarballs"
```

---

## Task 3: Release workflow — publish Release + multi-arch GHCR image

**Files:**
- Modify: `.github/workflows/release.yml` (add a `publish` job)

- [ ] **Step 1: Append the publish job**

Add this job to `.github/workflows/release.yml` (after `build`):
```yaml
  publish:
    name: publish release + image
    needs: build
    runs-on: ubuntu-latest
    # Publish on a tag push, or on a manual run with publish=true. Dry runs skip.
    if: github.event_name == 'push' || inputs.publish
    steps:
      - uses: actions/checkout@v5

      - name: Download build artifacts
        uses: actions/download-artifact@v4
        with:
          name: flannel-rs-binaries

      - name: Compute version
        id: ver
        run: |
          VERSION="${GITHUB_REF_NAME}"
          [ "${{ github.event_name }}" = "workflow_dispatch" ] && VERSION="dev-${GITHUB_SHA::8}"
          echo "version=$VERSION" >> "$GITHUB_OUTPUT"

      - name: Create GitHub Release with binary tarballs
        if: github.event_name == 'push'
        uses: softprops/action-gh-release@v2
        with:
          files: |
            out/flannel-rs_*_linux_amd64.tar.gz
            out/flannel-rs_*_linux_arm64.tar.gz
            out/flannel-rs_*_checksums.txt
          generate_release_notes: true

      - name: Set up QEMU
        uses: docker/setup-qemu-action@v3

      - name: Set up Buildx
        uses: docker/setup-buildx-action@v3

      - name: Log in to GHCR
        uses: docker/login-action@v3
        with:
          registry: ghcr.io
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}

      - name: Build + push multi-arch image
        uses: docker/build-push-action@v6
        with:
          context: .
          file: Dockerfile.release
          platforms: linux/amd64,linux/arm64
          push: true
          tags: |
            ghcr.io/indyjonesnl/flannel-rs:${{ steps.ver.outputs.version }}
            ghcr.io/indyjonesnl/flannel-rs:latest
            ghcr.io/indyjonesnl/flannel-rs:${{ github.sha }}
```
Note: the downloaded artifact restores `dist/amd64` and `dist/arm64` at the repo root, which `Dockerfile.release`'s `COPY dist/${TARGETARCH}/...` consumes per platform. `:latest` is pushed on every publish (tag or manual publish=true) — acceptable; refine later if pre-releases need excluding.

- [ ] **Step 2: Lint the YAML**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml'))" && echo "YAML OK"`
Expected: `YAML OK`.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci: release workflow — publish GH Release + multi-arch GHCR image"
```

---

## Task 4: Released DaemonSet manifest

**Files:**
- Create: `deploy/flannel-rs-release.yaml`

- [ ] **Step 1: Create the released manifest**

Copy `deploy/flannel-rs.yaml` to `deploy/flannel-rs-release.yaml`, then change ONLY the image references and pull policy so it pulls the published GHCR image instead of the locally-loaded `flannel-rs:dev`:
```bash
cp deploy/flannel-rs.yaml deploy/flannel-rs-release.yaml
sed -i 's#image: flannel-rs:dev#image: ghcr.io/indyjonesnl/flannel-rs:latest#g' deploy/flannel-rs-release.yaml
sed -i '/imagePullPolicy: Never/d' deploy/flannel-rs-release.yaml
```
Then verify: every container/initContainer image in the file is now `ghcr.io/indyjonesnl/flannel-rs:latest` and no `imagePullPolicy: Never` remains:
```bash
grep -n "image:\|imagePullPolicy" deploy/flannel-rs-release.yaml
python3 -c "import yaml; list(yaml.safe_load_all(open('deploy/flannel-rs-release.yaml')))" && echo "YAML OK"
```
Expected: all images are the GHCR ref; no `Never`; YAML parses. (The smoke/conformance harness keeps using `deploy/flannel-rs.yaml` with `:dev` — this released manifest is for end users; do NOT point the harness at it.)

- [ ] **Step 2: Commit**

```bash
git add deploy/flannel-rs-release.yaml
git commit -m "deploy: released DaemonSet manifest using the GHCR image"
```

---

## Task 5: Dry-run verification + first release

**Files:** none (operational).

- [ ] **Step 1: Push the branch and dry-run the workflow**

After the branch is pushed (or merged to main), trigger a dry run that builds everything but publishes nothing:
```bash
git push -u origin dist-release
gh workflow run release.yml --ref dist-release -f publish=false
sleep 10
gh run list --workflow release.yml --limit 1
RUN=$(gh run list --workflow release.yml --limit 1 --json databaseId --jq '.[0].databaseId')
gh run watch "$RUN" --exit-status
```
Expected: the `build` job is green; `publish` is SKIPPED (dry run). Download + inspect the artifacts:
```bash
gh run download "$RUN" -n flannel-rs-binaries -D /tmp/rel
file /tmp/rel/dist/amd64/flanneld /tmp/rel/dist/arm64/flanneld
ls -l /tmp/rel/out
```
Expected: amd64 binary `statically linked`; arm64 binary `ELF 64-bit ... ARM aarch64 ... statically linked`; tarballs + checksums present.

- [ ] **Step 2: (After merge to main) cut the first real release**

Once merged to `main` and `ci.yml` is green:
```bash
git checkout main && git pull
git tag v0.1.0 && git push origin v0.1.0
RUN=$(gh run list --workflow release.yml --limit 1 --json databaseId --jq '.[0].databaseId')
gh run watch "$RUN" --exit-status
```
Expected: `build` + `publish` both green. Then verify the published artifacts:
```bash
gh release view v0.1.0
docker pull ghcr.io/indyjonesnl/flannel-rs:v0.1.0
docker run --rm --entrypoint ls ghcr.io/indyjonesnl/flannel-rs:v0.1.0 \
  /usr/local/bin/flanneld /opt/cni/bin/flannel /opt/cni/bin/bridge /opt/cni/bin/host-local /opt/cni/bin/portmap
docker buildx imagetools inspect ghcr.io/indyjonesnl/flannel-rs:v0.1.0   # shows amd64 + arm64
```
Expected: Release has the two tarballs + checksums; image pulls; all five binaries present; manifest lists both arches.

- [ ] **Step 3: One-time — make the GHCR package public**

The first push creates a private package. In GitHub → the `flannel-rs` package settings → change visibility to Public (or via API). Document this in the README later. (Not scriptable in this plan without extra auth; it's a one-time UI action.)

---

## Self-Review

**Spec coverage:**
- Static musl cross-compile (cargo-zigbuild, both arches) → Task 1 Step 1, Task 2. ✓
- Binary tarballs + sha256 checksums on GitHub Release → Tasks 2, 3. ✓
- Multi-arch GHCR image via buildx, COPY prebuilt → Tasks 1 (Dockerfile.release), 3. ✓
- Image layout matches current (flanneld + 4 plugins, entrypoint) → Task 1 Dockerfile.release. ✓
- Tag-triggered + workflow_dispatch dry run (publish=false) → Tasks 2, 3 (`if:`), 5. ✓
- `GITHUB_TOKEN` GHCR auth, packages: write → Task 3. ✓
- Released manifest separate from the `:dev` harness path → Task 4. ✓
- Existing Dockerfile + ci.yml test path untouched → confirmed (new files only). ✓
- Dry-run verification (static check, both arches) + first tag → Task 5. ✓
- GHCR public visibility one-time note → Task 5 Step 3. ✓

**Placeholder scan:** Task 5 Step 3 is a one-time manual UI action (GHCR visibility) — explicitly flagged, not a code placeholder. No TBD/TODO elsewhere; all YAML/Dockerfile/commands are concrete.

**Consistency:** binary set `{flanneld, flannel, bridge, host-local, portmap}`, staging layout `dist/<arch>/`, version derivation (tag name, or `dev-<sha8>` for dispatch), image ref `ghcr.io/indyjonesnl/flannel-rs`, and `Dockerfile.release`'s `COPY dist/${TARGETARCH}/` all match across Tasks 1–5.

**Known risk:** musl/aarch64 cross-compile of a dependency — Task 1 Step 1 validates locally before any CI work; the Task 5 dry run validates both arches in CI before a real tag.
