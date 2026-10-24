#!/usr/bin/env python3
"""Regenerates ``src/words.txt``, the bundled dictionary of ``rustpy-spell``.

Sources
-------
* ``words_alpha.txt`` from https://github.com/dwyl/english-words (Unlicense):
  kept are the words of length 2..=20 that consist of lowercase ASCII letters
  only, minus a short list of well-known misspellings that the list contains.
* Identifier parts harvested from the typeshed stdlib stubs bundled with
  ``rustpy-semantic`` (Apache-2.0): every identifier is split like
  ``rustpy_spell::split_identifier`` does and the lowercase parts that are not
  English words are added, so that standard-library API vocabulary such as
  ``argv``, ``abspath`` or ``getcwd`` is never reported as a typo.

Output format ("front coding")
------------------------------
One word per line, sorted bytewise. Each line is ``<P><suffix>`` where ``P``
is a single character giving the number of leading characters shared with the
previous word (``0``-``9`` for 0..=9, ``A``-``Z`` for 10..=35) and ``suffix``
is the rest of the word. Words only contain lowercase ASCII letters, so the
prefix-length character is unambiguous. ``Dictionary::bundled()`` decodes this
at first use.

Usage: ``python3 tools/build_words.py [--words words_alpha.txt] [--typeshed DIR] [--out FILE]``
"""

from __future__ import annotations

import argparse
import os
import re
import sys
import urllib.request

WORDS_URL = "https://raw.githubusercontent.com/dwyl/english-words/master/words_alpha.txt"
MIN_LEN, MAX_LEN = 2, 20

# Entries that are far more often typos than the obscure or archaic words
# they technically are. They are removed from the final list, whichever source
# they come from (`creat` would otherwise come back through `os.O_CREAT`), so
# they are flagged; users can always add them back through their personal
# dictionary.
#
# Deliberately *kept* although sometimes flagged elsewhere: legitimate variants
# or rare words such as calender, dependant, fulfil, judgement, knowledgable,
# licence, loosing, supercede, therefor, wether and writeable, and the
# apostrophe-less contractions dont, doesnt, isnt and wont.
KNOWN_MISSPELLINGS = set(
    """
accessable accomodate accomodation acheive acknowlege acquaintence adress
agressive alledge allready alway arround aswell beleave beleif beleive belive
carefull catagories catched cemetary changable choise commitee concensus
constuctor creat definately dependance dependancy desireable desparate develope
developement diable dissapoint enviroment excercise existance existant
florescent flourescent formated formating forseeable frist fullfil goverment
handel hight identifer idiosyncracy immediatly incidently independant
indispensible inital innoculate langauge lenth liason manuever millenium
miniscule mispell momento neccessary noticable occurance occured occurence
occurrance ocurred orignal perseverence persue posession preceeding prefered
privelege priviledge promiss propoganda publically recieve recieved reciever
refered relevent resistence rythm seige sence seperate simular stoped succes
succesful succesfully suprise tabel tatoo tigger tommorow underate unforseen
untill wich wierd withing
""".split()
)

HERE = os.path.dirname(os.path.abspath(__file__))
CRATE = os.path.dirname(HERE)
DEFAULT_TYPESHED = os.path.join(CRATE, "..", "rustpy-semantic", "typeshed", "stdlib")

IDENT_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
PART_RE = re.compile(r"[A-Z]+(?![a-z])|[A-Z]?[a-z]+|[A-Z]+")


def load_english(path: str | None) -> set[str]:
    if path:
        with open(path, "rb") as f:
            raw = f.read()
    else:
        print(f"downloading {WORDS_URL} ...", file=sys.stderr)
        with urllib.request.urlopen(WORDS_URL, timeout=60) as resp:
            raw = resp.read()
    words = set()
    for line in raw.decode("utf-8", "replace").splitlines():
        w = line.strip()
        if MIN_LEN <= len(w) <= MAX_LEN and w.isascii() and w.isalpha() and w.islower():
            words.add(w)
    return words


def split_ident(ident: str) -> list[str]:
    parts = []
    for chunk in re.split(r"[^A-Za-z]+", ident):
        if chunk:
            parts.extend(m.group(0) for m in PART_RE.finditer(chunk))
    return parts


def harvest_typeshed(root: str) -> set[str]:
    names: set[str] = set()
    if not os.path.isdir(root):
        print(f"typeshed directory {root} not found; skipping harvest", file=sys.stderr)
        return names
    for dirpath, _dirs, files in os.walk(root):
        for name in files:
            if not name.endswith(".pyi"):
                continue
            try:
                with open(os.path.join(dirpath, name), encoding="utf-8") as f:
                    text = f.read()
            except OSError:
                continue
            # Only identifiers: drop comments and string literals first.
            text = re.sub(r"#.*", "", text)
            text = re.sub(r'"""[\s\S]*?"""', "", text)
            text = re.sub(r'"[^"\n]*"', "", text)
            text = re.sub(r"'[^'\n]*'", "", text)
            for m in IDENT_RE.finditer(text):
                for part in split_ident(m.group(0)):
                    w = part.lower()
                    if MIN_LEN <= len(w) <= MAX_LEN:
                        names.add(w)
    return names


def front_code(words: list[str]) -> str:
    out = []
    prev = ""
    for w in words:
        n = 0
        limit = min(len(prev), len(w), 35)
        while n < limit and prev[n] == w[n]:
            n += 1
        code = chr(ord("0") + n) if n < 10 else chr(ord("A") + n - 10)
        out.append(code + w[n:])
        prev = w
    return "\n".join(out) + "\n"


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--words", help="local copy of words_alpha.txt (downloaded when omitted)")
    ap.add_argument("--typeshed", default=DEFAULT_TYPESHED, help="typeshed stdlib directory to harvest")
    ap.add_argument("--out", default=os.path.join(CRATE, "src", "words.txt"))
    args = ap.parse_args()

    english = load_english(args.words) - KNOWN_MISSPELLINGS
    stdlib = harvest_typeshed(args.typeshed) - KNOWN_MISSPELLINGS
    extra = stdlib - english
    words = sorted(english | extra)
    encoded = front_code(words)
    with open(args.out, "w", encoding="utf-8", newline="\n") as f:
        f.write(encoded)
    print(
        f"{len(words)} words ({len(english)} English + {len(extra)} stdlib names), "
        f"{len(encoded.encode())} bytes -> {args.out}",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
