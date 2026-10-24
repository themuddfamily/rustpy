#!/usr/bin/env python3
"""usage: scripts/dev/merge_struct_fields.py FILE PREFIX  (run from the repository root during a merge)

Example: scripts/dev/merge_struct_fields.py crates/rustpy-ui/src/app.rs "let mut app = Self {"

Resolves a conflict hunk whose sides are each one line starting with PREFIX (e.g. a struct
literal `let mut app = Self {`), by three-way merging the comma-separated `field: value` list
using the merge base (git stage 1). Fields added on either side are kept; fields removed on
either side (present in base) are dropped; values changed on one side win."""
import re, subprocess, sys

path, prefix = sys.argv[1], sys.argv[2]

def split_fields(line):
    a = line.index('{') + 1
    b = line.rindex('}')
    body = line[a:b]
    parts, depth, cur, instr = [], 0, '', False
    i = 0
    while i < len(body):
        c = body[i]
        if instr:
            cur += c
            if c == '\\':
                cur += body[i + 1]; i += 2; continue
            if c == '"':
                instr = False
        elif c == '"':
            instr = True; cur += c
        elif c in '([{<':
            depth += 1; cur += c
        elif c in ')]}>':
            depth -= 1; cur += c
        elif c == ',' and depth == 0:
            parts.append(cur.strip()); cur = ''
        else:
            cur += c
        i += 1
    if cur.strip():
        parts.append(cur.strip())
    out = []
    for p in parts:
        m = re.match(r'([A-Za-z_][A-Za-z0-9_]*)\s*(?::\s*(.*))?$', p, re.S)
        out.append((m.group(1), p))
    return line[:a], out, line[b:]

def find_line(text):
    for l in text.splitlines():
        if l.strip().startswith(prefix):
            return l
    raise SystemExit(f'no line starting with {prefix!r}')

base = find_line(subprocess.run(['git', 'show', f':1:{path}'], capture_output=True, text=True, check=True).stdout)
s = open(path).read()
m = re.search(r'<<<<<<< [^\n]*\n(\s*' + re.escape(prefix) + r'[^\n]*)\n=======\n(\s*' + re.escape(prefix) + r'[^\n]*)\n>>>>>>> [^\n]*\n', s)
if not m:
    raise SystemExit('conflict hunk not found')
ours, theirs = m.group(1), m.group(2)
head, bf, tail = split_fields(base)
_, of, _ = split_fields(ours)
_, tf, _ = split_fields(theirs)
bd, od, td = dict(bf), dict(of), dict(tf)
names = [n for n, _ in of] + [n for n, _ in tf if n not in od]
res = []
for n in names:
    inb, ino, int_ = n in bd, n in od, n in td
    if inb and (not ino or not int_):
        continue  # removed on one side
    if ino and int_:
        v = od[n] if od[n] != bd.get(n) else td[n]
    else:
        v = od[n] if ino else td[n]
    res.append(v)
lead = ours[:len(ours) - len(ours.lstrip())]
oh, _, ot = split_fields(ours)
merged = oh + ' ' + ', '.join(res) + ' ' + ot.lstrip()
merged = merged.replace('{  ', '{ ')
s = s[:m.start()] + merged + '\n' + s[m.end():]
open(path, 'w').write(s)
print('merged fields:', len(res))
