#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail

readonly mallob_revision=4a3b8daea1805c57729b10f58730723e786e0336
readonly satsuma_revision=7ca68dbf2e18f818ee81f2350e4a8f8efa3c9274
readonly dejavu_revision=4c275e9ebac4fe51a26aac406682b9bcbfd03a9d
readonly mallob_root=/mallob
readonly build_jobs="${MALLOB_BUILD_JOBS:-2}"
if [[ ! "$build_jobs" =~ ^[1-9][0-9]*$ ]]; then
    echo 'MALLOB_BUILD_JOBS must be a positive integer' >&2
    exit 1
fi

install -d "$mallob_root"
git -C "$mallob_root" init --quiet
git -C "$mallob_root" fetch --quiet --depth 1 \
    https://github.com/domschrei/mallob.git "$mallob_revision"
git -C "$mallob_root" checkout --quiet --detach FETCH_HEAD
cd "$mallob_root"

curl --fail --location --retry 3 \
    https://fmv.jku.at/yalsat/yalsat-03v.zip --output lib/yalsat/yalsat.zip
echo '596ad9eb729ddfd67c2256adb20ff8d4d9d7018004d838f6b4e449b8b5be426a  lib/yalsat/yalsat.zip' \
    | sha256sum --check
unzip -q -o lib/yalsat/yalsat.zip -d lib/yalsat
cp -a lib/yalsat/yalsat-03v/. lib/yalsat/

source_dir="$(mktemp -d)"
trap 'rm -rf -- "$source_dir"' EXIT
git -C "$source_dir" init --quiet
git -C "$source_dir" fetch --quiet --depth 1 \
    https://github.com/domschrei/satsuma-with-cliquer.git "$satsuma_revision"
git -C "$source_dir" checkout --quiet --detach FETCH_HEAD
cp -a "$source_dir"/. lib/extsatsuma/
install -d "$source_dir/dejavu"
git -C "$source_dir/dejavu" init --quiet
git -C "$source_dir/dejavu" fetch --quiet --depth 1 \
    https://github.com/markusa4/dejavu.git "$dejavu_revision"
git -C "$source_dir/dejavu" checkout --quiet --detach FETCH_HEAD
cp -a "$source_dir/dejavu"/. lib/extsatsuma/src/dejavu/
sed -i "s/GIT_TAG \"origin\/main\"/GIT_TAG \"$dejavu_revision\"/" lib/extsatsuma/CMakeLists.txt
sed -i '/add_compile_options("-march=native")/d' lib/extsatsuma/CMakeLists.txt
sed -i '/add_compile_options("-march=native")/d' lib/extsatsuma/src/dejavu/CMakeLists.txt
sed -i "s|cmake -DCMAKE_BUILD_TYPE=RELEASE ..|cmake -DCMAKE_BUILD_TYPE=RELEASE -DFETCHCONTENT_SOURCE_DIR_DEJAVU=$mallob_root/lib/extsatsuma/src/dejavu ..|" \
    lib/extsatsuma/fetch-and-build.sh

for dependency in kissat cadical extsatsuma; do
    sed -i "s/^make -j$/make -j $build_jobs/" "lib/$dependency/fetch-and-build.sh"
done
# Recent GCC cannot build Lingeling's unused treengeling executable.
sed -i "s/^make$/make -j $build_jobs liblgl.a/" lib/lingeling/fetch-and-build.sh
install -d build
(cd lib/extsatsuma && bash fetch-and-build.sh "$mallob_root/build")

cmake -S . -B build \
    -DCMAKE_BUILD_TYPE=Release \
    '-DMALLOB_SUBPROC_DISPATCH_PATH="/mallob/build/"' \
    -DMALLOB_MAX_N_APPTHREADS_PER_PROCESS=64 \
    -DMALLOB_APP_INCSAT=0 -DMALLOB_APP_PALRUPCHECK=0 \
    -DMALLOB_APP_MAXSAT=0 -DMALLOB_APP_SMT=0 -DMALLOB_APP_SWEEP=0 \
    -DMALLOB_APP_SATWITHPRE=1 \
    -DMALLOB_BUILD_IMPCHECK=0 -DMALLOB_BUILD_CHECKER=0 -DMALLOB_BUILD_CHAINCHECK=0 \
    -DMALLOB_USE_ASAN=0 -DMALLOB_USE_JEMALLOC=0 -DMALLOB_USE_MINISAT=0 \
    -DMALLOB_USE_CADICAL=1 -DMALLOB_USE_KISSAT=1 -DMALLOB_USE_LINGELING=1 \
    -DMALLOB_USE_RUSTSAT=0 -DMALLOB_USE_MAXPRE=0 -DMALLOB_USE_SATSUMA=2
cmake --build build --parallel "$build_jobs" \
    --target mallob mallob_process_dispatcher mallob_sat_process

install -d /runtime/mallob/build
install -m755 build/mallob build/mallob_process_dispatcher build/mallob_sat_process \
    build/satsuma build/run-satsuma.sh /runtime/mallob/build/
cp -a config /runtime/mallob/
install -m644 LICENSE_MIT LICENSE_LGPL /runtime/mallob/
printf '%s\n' "$mallob_revision" > /runtime/mallob/REVISION
