#!/bin/sh

if [ "$#" -ne 5 ]; then
    printf "usage: outer_harness.sh <output-file> <executor> <benchmark> <inproc_iters> <param>"
    exit 1
fi

outf=$1; shift
executor=$1; shift
bmark=$1; shift
inproc_iters=$1; shift
param=$1; shift

output=$("$executor" inner_harness.sh "$bmark" "$inproc_iters" "$param" 2>&1)
s=$?

# shellcheck disable=SC2181
if [ $s -ne 0 ]; then
    echo "error: failed to run inner harness"
    exit $s
fi

msecs=$(echo "$output" | awk '$1 == "PEXEC_WALLCLOCK_MS" { print $2 }')
printf "PEXEC_WALLCLOCK_MS=%f" "$msecs" > "$outf"
