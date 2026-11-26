#!/usr/bin/env bash
BR=${BR:-target/release/cameleer-bench.exe}          # this branch
MA=${MA:-../otel-bench-main/target/release/cameleer-bench.exe}  # main at b7c0e73
OUT="$1"
: > "$OUT"
for round in 1 2 3; do
  for sc in direct seda cbr splitagg; do
    for side in main off micrometer opentelemetry; do
      if [ $side = main ]; then bin=$MA; mode=off; else bin=$BR; mode=$side; fi
      res=$(CAMEL_BENCH_METRICS=$mode "$bin" run $sc 2 3 3 2>/tmp/otel-err.txt | grep RESULT)
      tr=$(grep TRACING /tmp/otel-err.txt)
      echo "round=$round side=$side $res $tr" >> "$OUT"
    done
  done
done
echo DONE >> "$OUT"
