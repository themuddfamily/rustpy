#!/usr/bin/env python3
"""usage: scripts/dev/conflict_hunks.py FILE [first [last]]

Prints the conflict hunks of FILE (1-based, during a `git merge`) with the
OURS (HEAD) and THEIRS sides, so each can be resolved deliberately."""
import re, sys
path=sys.argv[1]; first=int(sys.argv[2]) if len(sys.argv)>2 else 1; last=int(sys.argv[3]) if len(sys.argv)>3 else 10**9
s=open(path).read()
ms=list(re.finditer(r'<<<<<<< [^\n]*\n(.*?)=======\n(.*?)>>>>>>> [^\n]*\n', s, re.S))
for i,m in enumerate(ms,1):
    if i<first or i>last: continue
    line=s[:m.start()].count('\n')+1
    print(f"===== hunk {i} (line {line}) OURS:")
    print(m.group(1).rstrip('\n'))
    print("----- THEIRS:")
    print(m.group(2).rstrip('\n'))
