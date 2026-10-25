#!/bin/bash
# usage: m185-matrix5.sh <resfile> <reps>   variants: off, place, lease, place+lease ; run as root (sudo) for the lease.
# The lease is held by the bench process only; the kernel drops it when the process exits.
trap "sudo -n setfacl -x u:$(id -un) /dev/cpu_dma_latency" EXIT
sudo -n setfacl -m u:$(id -un):rw /dev/cpu_dma_latency
BIN=/var/tmp/m18a5/plugin_latency5
RES=$1; reps=$2
for rep in $(seq 1 $reps); do
 for sp in 24 200; do
  for var in off place lease place+lease; do
     for i in $(seq 1 18); do
       l=$(cut -d' ' -f1 /proc/loadavg); awk -v l=$l 'BEGIN{exit !(l<4)}' && break; sleep 5
     done
     pl=off; lease=""
     case $var in place) pl=auto;; lease) lease=1;; place+lease) pl=auto; lease=1;; esac
     env GORCA_PLUGIN_PLACEMENT=$pl ${lease:+GORCA_IDLE_LEASE=1} GORCA_PLUGIN_SCHEDULER=sps \
       GORCA_BENCH_DUMMY_TIER=first GORCA_BENCH_SPACING_MS=$sp GORCA_BENCH_STAGES=1 GORCA_BENCH_OUT=$RES timeout 900 $BIN --nocapture \
       > /var/tmp/m18a5/log5-$var-$sp.log 2>&1 &
     pid=$!
     if [ -n "$lease" ] && [ "$rep" = 1 ]; then
       sleep 12
       sudo -n python3 -I -c "import struct;print('cpu_dma_latency during run:',struct.unpack('i',open('/dev/cpu_dma_latency','rb').read(4))[0])" >> /var/tmp/m18a5/lease-probe.txt 2>&1
     fi
     wait $pid
     echo "== $var sps first ${sp}ms" >> $RES
  done
 done
done
echo DONE >> $RES
