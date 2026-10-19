#!/usr/bin/python3
# Prints the result of Ping as seen by the calling identity (run through run-as.sh).
import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ptestlib import *
try:
    print('allowed, version', ping())
except HelperError as e:
    print('denied' if e.name == 'AccessDenied' else 'error', '-', e)
