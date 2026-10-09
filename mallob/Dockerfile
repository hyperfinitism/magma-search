# SPDX-License-Identifier: Apache-2.0
FROM ubuntu:26.04@sha256:f144425ff09be612d6d9ad965196e9cdc23dae1f42110a8a11a3e9a8198759f7 AS build

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential ca-certificates cmake curl git libopenmpi-dev unzip zlib1g-dev \
    && rm -rf /var/lib/apt/lists/*

ARG MALLOB_BUILD_JOBS=2
COPY build-mallob.sh /tmp/build-mallob.sh
RUN MALLOB_BUILD_JOBS="$MALLOB_BUILD_JOBS" bash /tmp/build-mallob.sh

FROM ubuntu:26.04@sha256:f144425ff09be612d6d9ad965196e9cdc23dae1f42110a8a11a3e9a8198759f7

RUN apt-get update && apt-get install -y --no-install-recommends \
        libgomp1 openmpi-bin procps zlib1g \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /runtime/mallob /mallob
WORKDIR /mallob
ENV RDMAV_FORK_SAFE=1 \
    OMPI_ALLOW_RUN_AS_ROOT=1 \
    OMPI_ALLOW_RUN_AS_ROOT_CONFIRM=1
