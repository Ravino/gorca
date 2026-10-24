#!/usr/bin/env bash
# Evict ONE cache file from the page cache with posix_fadvise(DONTNEED). Never drop_caches.
set -euo pipefail
exec python3 -I -c 'import os,sys; fd=os.open(sys.argv[1],os.O_RDONLY); os.posix_fadvise(fd,0,0,os.POSIX_FADV_DONTNEED); print("evicted",sys.argv[1])' "$1"
