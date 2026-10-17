#!/usr/bin/env bash
S=/tmp/scratch/f
$S/e2f.sh base-1 new
$S/e2f.sh fix-1 fix
echo fin >> $S/e2f.log
