#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail

readonly threads="$1"
cd /mallob
readonly work="${MAGMA_MALLOB_WORK_DIR:-$(mktemp -d /tmp/magma-mallob-XXXXXXXXXX)}"
mpi_pid=''

cleanup() {
    rm -rf -- "$work"
}
trap cleanup EXIT

terminate() {
    trap - TERM INT
    if [[ -n "$mpi_pid" ]]; then
        kill -TERM "$mpi_pid" 2>/dev/null || true
        wait "$mpi_pid" || true
    fi
    exit "$1"
}
trap 'terminate 143' TERM
trap 'terminate 130' INT

cat > "$work/input.cnf"

export MAGMA_MALLOB_RUN_ID="${work##*/}"
export OMPI_ALLOW_RUN_AS_ROOT=1
export OMPI_ALLOW_RUN_AS_ROOT_CONFIRM=1
export RDMAV_FORK_SAFE=1

ranks=1
preset=(-mono-app=SAT -satsolver=k)
if ((threads > 1)); then
    ranks=2
    preset=()
    read -r -a options <<< "$(config/presets/sat-cascading-quick | tr '\n' ' ')"
    for ((i = 0; i < ${#options[@]}; i++)); do
        # minprocs is a mallob_local.sh directive, not a Mallob option.
        if [[ "${options[i]}" == -minprocs ]]; then
            ((i += 1))
        else
            preset+=("${options[i]}")
        fi
    done
fi

mpirun --bind-to none --oversubscribe -np "$ranks" build/mallob \
    "${preset[@]}" "-t=$((threads / ranks))" "-pph=$ranks" -rpa=1 \
    "-mono=$work/input.cnf" "-s2f=$work/solution.txt" "-tmp=$work" \
    "-apidir=$work/api" "-trace-dir=$work" \
    -q=1 -v=0 -os=0 -interface-fs=0 -pre-cleanup=0 -terminate-abruptly=0 \
    </dev/null >"$work/stdout.txt" &
mpi_pid=$!
status=0
wait "$mpi_pid" || status=$?
mpi_pid=''
trap - TERM INT

cat "$work/stdout.txt" >&2
if [[ -f "$work/solution.txt" ]]; then
    cat "$work/solution.txt"
else
    cat "$work/stdout.txt"
fi
exit "$status"
