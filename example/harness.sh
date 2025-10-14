#!/bin/sh

set -eu

usage() {
    printf "usage: harness.sh <benchmark> <inproc-iters> <param>\n\n"
    printf "available benchmarks: bigloop"
}

if [ "$#" -ne 3 ]; then
    usage
    exit 1
fi

bmark=$1; shift
inproc_iters=$1; shift
param=$1; shift

# A simple "loop and sum" benchmark.
bigloop() {
    sum=0;
    i=$1
    while [ "$i" -ne 0 ]; do
        sum=$((sum + 1))
        i=$((i - 1))
    done
    echo $sum
}

# Runs the specified benchmark with the requested number of in-process
# iterations and benchmark parameter.
run() {
    r_bmark=$1; shift
    r_inproc_iters=$1; shift
    r_param=$1; shift

    while [ "$r_inproc_iters" -ne 0 ]; do
        "$r_bmark" "$r_param"
        r_inproc_iters=$((r_inproc_iters - 1))
    done
}

case $bmark in
    bigloop)
        run "$bmark" "$inproc_iters" "$param"
        ;;
    *)
        printf "unknown benchmark: %s" "$bmark"
        usage
        exit1
        ;;
esac
