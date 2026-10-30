#!/usr/bin/env python3
"""Build assets/rhymes.txt from the CMU Pronouncing Dictionary.

    python3 -I make_rhymes.py path/to/cmudict.dict rhymes.txt

Source: https://github.com/cmusphinx/cmudict (BSD-style licence, see CMUDICT-LICENSE).

Output format (read by src/rhyme.rs):
    line 1        N, the number of rhyme tails
    next N lines  one rhyme tail each, e.g. "AY N"; a tail's code is its line index in base 36
    rest          "word code" lines, sorted by byte order (so they can be binary-searched)
A rhyme tail = phonemes from the last stressed vowel (stress 1 or 2, else the last
vowel) to the end, stress digits stripped. Its first phoneme is the final stressed vowel.
"""
import re
import sys

# Slang the dictionary lacks (same notation as cmudict.dict).
EXTRA = {
    "tryna": "T R AY1 N AH0", "finna": "F IH1 N AH0", "imma": "AY1 M AH0",
    "lil": "L IH1 L", "bruh": "B R AH1", "tho": "DH OW1", "cuz": "K AH1 Z",
    "fo": "F AO1 R", "ion": "AY1 AA0 N", "bout": "B AW1 T",
}


def b36(n):
    s = ""
    while True:
        n, r = divmod(n, 36)
        s = "0123456789abcdefghijklmnopqrstuvwxyz"[r] + s
        if n == 0:
            return s


def tail(phones):
    # the last vowel with primary or secondary stress, so alkaline (AE1 .. AY2 N) rhymes with nine
    idx = None
    for i in range(len(phones) - 1, -1, -1):
        if phones[i].endswith(("1", "2")):
            idx = i
            break
    if idx is None:
        for i in range(len(phones) - 1, -1, -1):
            if phones[i][-1].isdigit():
                idx = i
                break
    if idx is None:
        return None
    return " ".join(re.sub(r"\d", "", p) for p in phones[idx:])


def main(src, dst):
    words = {}
    for line in open(src, encoding="utf-8"):
        line = line.split("#")[0].strip()
        if not line:
            continue
        w, _, rest = line.partition(" ")
        if not re.fullmatch(r"[a-z']+", w) or w in words:
            continue
        words[w] = rest.split()
    for w, p in EXTRA.items():
        words.setdefault(w, p.split())
    from collections import Counter
    tl = {w: tail(p) for w, p in words.items()}
    kept = [(w, t) for w, t in tl.items() if t]
    # Most common tails get the shortest codes.
    freq = Counter(t for _, t in kept)
    tails = {t: i for i, (t, _) in enumerate(freq.most_common())}
    entries = sorted((w.encode(), tails[t]) for w, t in kept)
    out = [str(len(tails))] + list(tails)
    out += [b.decode() + " " + b36(c) for b, c in entries]
    open(dst, "w", encoding="utf-8").write("\n".join(out) + "\n")
    print(len(tails), "tails,", len(entries), "words")


main(sys.argv[1], sys.argv[2])
