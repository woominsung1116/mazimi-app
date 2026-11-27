"""Build the understanding model's rows (data/intent_rows.json) from
data/base_rows.json: augment train roots, cap synthetic rows, check splits.

Rules (architect design, "Training data, split by source"):
- augmentation only on train roots of real rows (author rounds 11/13/14/15,
  re-reviews 8-11); labels preserved by construction;
- compositional rows (compose_rows.py), written fresh, at most 1/2 of the
  synthetic budget;
- every request row gets a 1:1 negative with the same words in a mention,
  report-of-request, question or retract frame;
- at most 8 variants per root, each inheriting its root's group;
- synthetic rows (augmented + router train_v3 rows) at most 60% of train;
  real rows carry weight 2;
- no train goal at Jaccard >= 0.7 (character bigrams of the normalized goal)
  to a dev or test goal; no goal of the sealed acceptance batch (hashes only:
  NFKC, lowercase, [0-9a-z가-힣ㄱ-ㅎㅏ-ㅣ], sha256) anywhere.

    python build_data.py SEALED_HASHES.txt
"""
import hashlib, json, random, re, sys, unicodedata

import compose_rows
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent
RNG = random.Random(20261028)
REAL_SOURCES = {"rr8", "rr9", "rr10", "rr11", "au11", "au13", "au14", "au15", "fresh", "fresh2"}
MAX_VARIANTS = 8
SYNTH_SHARE = 0.60


def sealed_norm(g):
    g = unicodedata.normalize("NFKC", g).lower()
    return "".join(ch for ch in g if re.match(r"[0-9a-z가-힣ㄱ-ㅎㅏ-ㅣ]", ch))


def sha(s):
    return hashlib.sha256(s.encode()).hexdigest()


def norm(g):
    return "".join(ch for ch in unicodedata.normalize("NFKC", g).lower() if ch.isalnum())


def bigrams(g):
    n = norm(g)
    return {n[i:i + 2] for i in range(len(n) - 1)} or {n}


# ── augmentation operators (labels preserved) ──
ENDINGS = [("줘", ["줘요", "주세요", "주셈", "줘여", "주라", "줄래"]), ("해줘", ["해주세요", "해줘요", "ㄱㄱ"]),
           ("켜줘", ["켜주세요", "켜줘요", "켜기", "켜 ㄱㄱ"]), ("꺼줘", ["꺼주세요", "꺼줘요", "끄기"]),
           ("저장해줘", ["저장해 주세요", "저장 ㄱㄱ", "저장 부탁"]), ("기록해줘", ["기록해 주세요", "기록 부탁"])]
EMOJI = [" 🙏", " ㅎㅎ", " !!", " ㅠㅠ", " ~"]
NEAR = {"ㅏ": "ㅓ", "ㅓ": "ㅏ", "ㅗ": "ㅜ", "ㅜ": "ㅗ", "ㅐ": "ㅔ", "ㅔ": "ㅐ"}
CHO = "ㄱㄲㄴㄷㄸㄹㅁㅂㅃㅅㅆㅇㅈㅉㅊㅋㅌㅍㅎ"
JUNG = "ㅏㅐㅑㅒㅓㅔㅕㅖㅗㅘㅙㅚㅛㅜㅝㅞㅟㅠㅡㅢㅣ"


def register(g):
    out = []
    for end, alts in ENDINGS:
        if g.endswith(end):
            out += [g[: -len(end)] + a for a in alts]
    return out


def typo(g):
    chars = list(g)
    idx = [i for i, c in enumerate(chars) if 0 <= ord(c) - 0xAC00 < 11172]
    if not idx:
        return []
    i = RNG.choice(idx)
    o = ord(chars[i]) - 0xAC00
    cho, jung, jong = o // 588, (o % 588) // 28, o % 28
    v = JUNG[jung]
    if v not in NEAR:
        return []
    chars[i] = chr(0xAC00 + cho * 588 + JUNG.index(NEAR[v]) * 28 + jong)
    return ["".join(chars)]


def spacing(g):
    return [g.replace(" ", "")] if " " in g else []


def emoji(g):
    return [g + RNG.choice(EMOJI)]


NEG_FRAMES = [
    ("mention", "{} 하면 어떻게 돼?"),
    ("mention", "\"{}\"라고 했는데 아무 반응이 없어"),
    ("retract", "{} 아니다 됐어"),
    ("mention", "친구가 {} 라고 하래"),
]


def negative(row, k):
    act, frame = NEG_FRAMES[k % len(NEG_FRAMES)]
    cands = []
    for c in row["cands"]:
        c = dict(c)
        if c["act"] in ("request", "report"):
            c["act"] = act
        cands.append(c)
    return frame.format(row["goal"].rstrip("?!. ")), cands


def main():
    base = json.loads((HERE / "data/base_rows.json").read_text(encoding="utf-8"))["rows"]
    sealed = {l.strip() for l in Path(sys.argv[1]).read_text().split() if len(l.strip()) == 64}
    is_sealed = lambda g: sha(sealed_norm(g)) in sealed
    held = [r for r in base if r["split"] in ("dev", "test")]
    # The agent evaluation sets (dev, holdout v1-v4) are held out too: no
    # train goal at or near one of their goals.
    fixtures = sorted((HERE.parents[2] / "crates/api/tests/fixtures").glob("agent_goals*.json"))
    eval_goals = [c["goal"] for f in fixtures for c in json.loads(f.read_text(encoding="utf-8"))["cases"]]
    held_b = [bigrams(r["goal"]) for r in held] + [bigrams(g) for g in eval_goals]

    def near_held(g):
        b = bigrams(g)
        return any(len(b & h) / len(b | h) >= 0.7 for h in held_b)

    train = [r for r in base if r["split"] == "train"]
    real = [dict(r, weight=2, synthetic=False) for r in train if r["source"] in REAL_SOURCES]
    router = [r for r in train if r["source"] == "tv3"]
    budget = int(len(real) * SYNTH_SHARE / (1 - SYNTH_SHARE))
    writes = [r for r in router if r["cands"]]
    reads = [r for r in router if not r["cands"]]
    RNG.shuffle(reads)
    aug = []
    seen = {norm(r["goal"]) for r in base}
    for r in real:
        variants = []
        if any(c["act"] == "request" for c in r["cands"]):
            g, cands = negative(r, len(aug))
            variants.append((g, cands))
        for op in (register, typo, spacing, emoji):
            for g in op(r["goal"]):
                variants.append((g, r["cands"]))
        n = 0
        for g, cands in variants:
            if n >= MAX_VARIANTS or norm(g) in seen or is_sealed(g) or near_held(g):
                continue
            seen.add(norm(g))
            aug.append({"goal": g, "ctx": r["ctx"], "cands": cands, "source": "aug:" + r["source"],
                        "group": r["group"], "root": r["root"], "split": "train", "weight": 1, "synthetic": True})
            n += 1
    # Compositional rows (compose_rows.py): fresh, never from dev or test;
    # the same sealed and near-held filters below.
    gen = []
    for r in compose_rows.rows():
        if norm(r["goal"]) in seen or is_sealed(r["goal"]) or near_held(r["goal"]):
            continue
        seen.add(norm(r["goal"]))
        gen.append(dict(r, split="train", weight=1, synthetic=True))
    # Budget shares: compositional rows first (at most 1/2), then augmented
    # real roots (1/3), router write rows (1/16), router read rows (the "no
    # candidate" negatives) fill the rest.
    RNG.shuffle(gen)
    RNG.shuffle(aug)
    RNG.shuffle(writes)
    synth = gen[: budget // 2]
    synth += aug[: budget // 3]
    synth += [dict(r, weight=1, synthetic=True) for r in writes[: budget // 16]]
    synth += [dict(r, weight=1, synthetic=True) for r in reads[: budget - len(synth)]]
    rows = real + synth + [dict(r, weight=1, synthetic=False) for r in held]
    dropped = [r["goal"] for r in rows if r["split"] == "train" and (is_sealed(r["goal"]) or near_held(r["goal"]))]
    rows = [r for r in rows if not (r["split"] == "train" and (is_sealed(r["goal"]) or near_held(r["goal"])))]
    assert not any(is_sealed(r["goal"]) for r in rows), "a sealed goal is in the rows"
    groups = {}
    for r in rows:
        groups.setdefault(r["group"], set()).add(r["split"])
    assert all(len(s) == 1 for s in groups.values()), "a group crosses splits"
    train_n = sum(r["split"] == "train" for r in rows)
    synth_n = sum(r["split"] == "train" and r["synthetic"] for r in rows)
    meta = {"rows": len(rows), "splits": Counter(r["split"] for r in rows), "train_real": train_n - synth_n,
            "train_synthetic": synth_n, "synthetic_share": round(synth_n / max(1, train_n), 3),
            "dropped_near_held_or_sealed": len(dropped), "sealed_hashes": len(sealed),
            "eval_goals_held_out": len(eval_goals),
            "compositional": sum(r["source"] == "gen2" for r in rows)}
    (HERE / "data/intent_rows.json").write_text(json.dumps({"meta": meta, "rows": rows}, ensure_ascii=False, indent=0))
    print(json.dumps(meta, ensure_ascii=False, default=dict))


if __name__ == "__main__":
    main()
