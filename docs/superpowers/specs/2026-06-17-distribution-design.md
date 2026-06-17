# flannel-rs Distribution — Design

**Date:** 2026-06-17
**Status:** Approved (brainstorming)

## Context

flannel-rs is feature-complete for IPv4: the whole stack (flanneld + the four CNI
plugins) is Rust and CI-green (smoke parity + sig-network conformance). It has no
published artifacts — there is only a local `flannel-rs:dev` image built ad hoc.
This adds a real distribution pipeline so the project can be consumed.

## Decisions (locked with user)

- **Artifacts:** both a multi-arch container image AND per-arch binary tarballs.
- **Registry:** GHCR — `ghcr.io/indyjonesnl/flannel-rs`.
- **Linking:** static **musl** binaries (avoids the glibc-mismatch class of bugs —
  we hit `GLIBC_2.39 not found` copying host-built binaries onto bookworm nodes;
  CNI plugins exec in arbitrary node userspace, so static is the safe choice).
- **Arches:** `linux/amd64` + `linux/arm64`.

## Build strategy

Cross-compile static musl with **`cargo-zigbuild`** on a single x86_64 CI runner
(zig as the cross-linker — no QEMU, no per-arch gcc toolchains). Targets:
`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`. Deps are pure-Rust /
rustls (no OpenSSL), so static musl links cleanly. Produces the five static
binaries per arch: `flanneld`, `flannel`, `bridge`, `host-local`, `portmap`.

## Artifacts

### 1. GitHub Release (binary tarballs)
On a `v*` tag: `flannel-rs_<version>_linux_amd64.tar.gz` and
`..._linux_arm64.tar.gz`, each containing the five binaries, plus
`flannel-rs_<version>_checksums.txt` (sha256 of each tarball). Mirrors how
`containernetworking/plugins` / `hydrophone` ship.

### 2. Multi-arch image (GHCR)
`ghcr.io/indyjonesnl/flannel-rs` tagged `:<version>`, `:latest`, and `:<git-sha>`.
Built with `docker buildx --platform linux/amd64,linux/arm64`, **COPYing the
pre-built static binaries** into a minimal runtime base per arch (no in-image
cargo build → fast, no QEMU compile). Same layout as today so the DaemonSet keeps
working:
- `flanneld` → `/usr/local/bin/flanneld` (entrypoint)
- `flannel`, `bridge`, `host-local`, `portmap` → `/opt/cni/bin/`
- runtime base includes `iptables` + `iproute2` + `ca-certificates` (flanneld
  shells `iptables`; the rest is static).

A new `Dockerfile.release` (build-arg `TARGETARCH`) copies the matching arch's
prebuilt binaries — the existing `Dockerfile` (cargo build in-image) stays for
local `flannel-rs:dev` dev/test.

## Release workflow

New `.github/workflows/release.yml`:
- **Triggers:** push of a `v*` tag; plus `workflow_dispatch` (manual, for dry
  runs — build artifacts without publishing when a `publish=false` input is set).
- **Permissions:** `contents: write` (Release), `packages: write` (GHCR).
- **Steps:** install Rust + `cargo-zigbuild` + the two musl targets → build both
  arches → assemble tarballs + checksums → (if publishing) create the GitHub
  Release with the assets → `docker buildx` build+push the multi-arch image to
  GHCR (login via `GITHUB_TOKEN`).
- `ci.yml` is unchanged (push/PR lint+smoke+conformance).

## Manifests

`deploy/flannel-rs.yaml` currently uses `image: flannel-rs:dev` +
`imagePullPolicy: Never` (local kind). Add a released variant: a
`deploy/flannel-rs-release.yaml` (or a documented image override) pointing at
`ghcr.io/indyjonesnl/flannel-rs:<version>` with default pull policy, so the smoke
harness keeps using the local `:dev` image while end users get a pull-able
manifest. The smoke/conformance harness continues to build + load `flannel-rs:dev`
locally — released images do not change the test path.

## Verification

- The release workflow is validated via `workflow_dispatch` with `publish=false`
  (a dry run): confirm both arches build static musl, tarballs + checksums are
  produced, and the buildx image assembles — without pushing/releasing.
- `file` on a produced binary shows `statically linked`.
- A real `vX.Y.Z` tag then produces the Release assets + GHCR image; pull the
  image and `docker run --entrypoint ls` to confirm all five binaries are present
  for each arch.
- Existing `ci.yml` (smoke + conformance on `flannel-rs:dev`) stays green —
  distribution changes must not touch the test path.

## Out of scope / later

- Image/artifact signing (cosign), SBOM/provenance attestation.
- Docker Hub mirror.
- Pinning `deploy/` manifests to a specific released tag by default.
- Homebrew/krew or distro packaging.

## Risks

- **Cross-compile gotchas** — a dep may not build for musl/aarch64. Mitigation:
  the `workflow_dispatch` dry run surfaces this before any tag; deps are
  pure-Rust/rustls so risk is low. If a crate needs a C toolchain, zigbuild
  generally handles it; fallback is `cross`.
- **GHCR package visibility** — first push creates a private package; must be set
  public (one-time, documented in the plan).
