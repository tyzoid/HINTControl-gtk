#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"
image="${DEB_BUILD_IMAGE:-ubuntu:26.04}"

podman run --rm \
  -v "$repo_root:/work" \
  -w /work \
  "$image" \
  bash -lc '
    set -euo pipefail
    apt-get -o APT::Sandbox::User=root update
    DEBIAN_FRONTEND=noninteractive apt-get -o APT::Sandbox::User=root install -y --no-install-recommends \
      ca-certificates \
      cargo \
      rustc \
      pkg-config \
      build-essential \
      libgtk-4-dev \
      libssl-dev \
      dpkg-dev
    ./releng/ubuntu/build-deb.sh
  '
