# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = [
#   "numpy==2.5.4",
#   "scipy==1.18.1",
#   "scikit-learn==1.9.1",
#   "torch==2.14.1",
#   "transformers==4.57.6",
#   "tokenizers==0.22.2",
#   "safetensors==0.8.0",
# ]
# ///
"""Train the agent's write-understanding model and export it for the Rust
runtime (`crates/api/src/agent/classifier/understand.rs`).

One masked goal in, for every write type an act (none, request, mention,
question, report, retract) with calibrated probabilities, plus the value
of an alert, save, checklist mark or status, and the target (on_screen,
saved, selected, none). Features, shared by every head: one
multilingual-e5-small pass of "query: <goal>" (standardized, weighted),
char_wb 1-4 and jamo char_wb 3-6 TF-IDF of the goal, char_wb 1-5 of its
last 16 characters, and char_wb 1-4 of its last two words (the final
clause: a correction or retraction decides the act). Heads: linear
(multinomial logistic regression), each with a softmax temperature fitted
on dev. The card bar per type is the lowest write probability with
precision >= 0.90 on dev (a rare unasked card still needs the user's
confirmation; a miss costs the user); the report also gives the 0.97 bars.
For every value head (alert on/off, save/unsave, checklist done/undo,
the status) the value bar is the lowest margin between the two likeliest
values above which no asked dev row has the wrong value; below it the
policy gives the state-aware button, not a card; no bar is below VALUE_BAR_FLOOR (0.83). The status head has a
"rejected" value (core records no rejection): no card, the picker.

Data: data/intent_rows.json (build_data.py): train for fitting, dev for
calibration, thresholds and model selection, test for the report only.
Dev and test rows the guard bars (data/guard_barred.json, written by the
Rust test `export_guard_barred`) are left out of calibration and the
report: the policy bars them before the model reads them.
Every row's goal is checked against the sealed acceptance batch by hash
(--sealed); a hit is a hard error.

    uv run --script train.py --model-dir DIR --sealed HASHES.txt [--check]
"""
from __future__ import annotations

import argparse, base64, hashlib, importlib.util, json, re, sys, unicodedata
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
ROWS = HERE / "data/intent_rows.json"
ARTEFACT = ROOT / "crates/api/assets/understanding/understanding.json"
PARITY = ROOT / "crates/api/tests/fixtures/understanding_parity.json"
_spec = importlib.util.spec_from_file_location("intent_train", ROOT / "scripts/ai/intent-classifier/train.py")
intent = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(intent)
ENCODER, Encoder, jamo, sha256 = intent.ENCODER, intent.Encoder, intent.jamo, intent.sha256

FORMAT = "mazimi-understanding/4"
VERSION = "e5s-char-jamo-understand-v5"
TYPES = ["alert", "save", "status", "checklist", "calendar", "profile", "form", "page"]
ACTS = ["none", "request", "mention", "question", "report", "retract"]
VALUES = {"alert": ["on", "off"], "save": ["save", "unsave"], "checklist": ["done", "undo"],
          "status": ["planning", "applied", "waiting", "received", "abandoned", "rejected"]}
TARGETS = ["on_screen", "saved", "selected", "none"]
WRITES_FOR = {t: ["request", "report"] if t in ("status", "checklist") else ["request"] for t in TYPES}
CHAR, JAMO_R, TAIL_R, TAIL_CHARS, LAST_R, LAST_WORDS = (1, 4), (3, 6), (1, 5), 16, (1, 4), 2
GRID = [(c, w) for c in (4.0, 16.0) for w in (0.05, 0.15)]
PRECISION = 0.90
PRECISION_STRICT = 0.97
# Polarity pairs: a card's direction must clear a margin between the two
# values, the lowest margin above which no dev row has the wrong direction
# (a wrong direction does the opposite of the ask; a miss is a button).
POLARITY = ("alert", "save", "checklist", "status")
VALUE_PRECISION = 1.0
# One shared minimum for every value bar (delta review 3): with no wrong
# value on dev to calibrate on, a bar would fall to 0; it never goes below
# the last calibrated checklist bar.
VALUE_BAR_FLOOR = 0.83


def sealed_norm(g):
    g = unicodedata.normalize("NFKC", g).lower()
    return "".join(ch for ch in g if re.match(r"[0-9a-z가-힣ㄱ-ㅎㅏ-ㅣ]", ch))


def tail(goal):
    return goal[-TAIL_CHARS:]


def last(goal):
    """The last two whitespace-separated words (Rust: split_whitespace)."""
    return " ".join(goal.split()[-LAST_WORDS:])


def act_of(row, t):
    for c in row["cands"]:
        if c["type"] == t:
            return c["act"]
    return "none"


class Model:
    def __init__(self, c, w):
        self.c, self.w = c, w

    def fit_features(self, goals, emb):
        from sklearn.feature_extraction.text import TfidfVectorizer
        from sklearn.preprocessing import StandardScaler
        self.char = TfidfVectorizer(analyzer="char_wb", ngram_range=CHAR, sublinear_tf=True).fit(goals)
        self.jamo = TfidfVectorizer(analyzer="char_wb", ngram_range=JAMO_R, sublinear_tf=True, preprocessor=jamo).fit(goals)
        self.tail = TfidfVectorizer(analyzer="char_wb", ngram_range=TAIL_R, sublinear_tf=True).fit([tail(g) for g in goals])
        self.last = TfidfVectorizer(analyzer="char_wb", ngram_range=LAST_R, sublinear_tf=True).fit([last(g) for g in goals])
        self.scaler = StandardScaler().fit(emb)

    def X(self, goals, emb):
        import scipy.sparse as sp
        return sp.hstack([self.char.transform(goals), self.jamo.transform(goals),
                          self.tail.transform([tail(g) for g in goals]),
                          self.last.transform([last(g) for g in goals]),
                          sp.csr_matrix(self.scaler.transform(emb) * self.w)]).tocsr()

    def fit_head(self, X, y, weights, labels):
        from sklearn.linear_model import LogisticRegression
        present = sorted(set(y), key=labels.index)
        if len(present) < 2:
            return None
        lr = LogisticRegression(C=self.c, max_iter=8000, class_weight="balanced").fit(X, y, sample_weight=weights)
        return lr

    def fit(self, rows, emb):
        goals = [r["goal"] for r in rows]
        self.fit_features(goals, emb)
        X = self.X(goals, emb)
        wts = np.array([r.get("weight", 1) for r in rows], dtype=float)
        self.heads = {}
        for t in TYPES:
            self.heads[("act", t)] = self.fit_head(X, [act_of(r, t) for r in rows], wts, ACTS)
        for t, labels in VALUES.items():
            idx = [i for i, r in enumerate(rows) for c in r["cands"]
                   if c["type"] == t and c["act"] in WRITES_FOR[t] and c["value"] in labels]
            y = [next(c["value"] for c in rows[i]["cands"] if c["type"] == t) for i in idx]
            self.heads[("value", t)] = self.fit_head(X[idx], y, wts[idx], labels) if idx else None
        idx, y = [], []
        for i, r in enumerate(rows):
            tg = [c["target"] for c in r["cands"] if c["act"] in WRITES_FOR[c["type"]] and c["target"] in TARGETS
                  and c["type"] != "profile"]
            if tg:
                idx.append(i); y.append(tg[0])
        self.heads[("target", "")] = self.fit_head(X[idx], y, wts[idx], TARGETS)
        return self

    @staticmethod
    def labels_of(kind, t):
        return ACTS if kind == "act" else VALUES[t] if kind == "value" else TARGETS

    def logits(self, X, key):
        lr = self.heads[key]
        labels = self.labels_of(*key)
        out = np.full((X.shape[0], len(labels)), -30.0)
        raw = X @ lr.coef_.T + lr.intercept_ if len(lr.classes_) > 2 else None
        if raw is None:  # binary: sklearn keeps one row
            z = (X @ lr.coef_.T).ravel() + lr.intercept_[0]
            raw = np.stack([-z / 2, z / 2], axis=1)
        for j, cl in enumerate(lr.classes_):
            out[:, labels.index(cl)] = raw[:, j]
        return out


def softmax(z, T=1.0):
    z = z / T
    z = z - z.max(1, keepdims=True)
    p = np.exp(z)
    return p / p.sum(1, keepdims=True)


def fit_temperature(z, y_idx):
    best = (1e9, 1.0)
    for T in np.exp(np.linspace(np.log(0.3), np.log(5.0), 60)):
        p = softmax(z, T)
        nll = -np.mean(np.log(p[np.arange(len(y_idx)), y_idx] + 1e-12))
        best = min(best, (nll, float(T)))
    return round(best[1], 4)


def card_bar(mass, gold, precision=PRECISION):
    for thr in np.arange(0.30, 0.99, 0.01):
        pred = mass >= thr
        tp, fp = int((pred & gold).sum()), int((pred & ~gold).sum())
        if tp and tp / (tp + fp) >= precision:
            return round(float(thr), 2)
    return 0.99


def value_margins(model, rows, emb, t):
    """(margin, right) for the asked rows of a polarity type."""
    X = model.X([r["goal"] for r in rows], emb)
    p = softmax(model.logits(X, ("value", t)), 1.0)
    out = []
    for i, r in enumerate(rows):
        gold = next((c["value"] for c in r["cands"] if c["type"] == t and c["act"] in WRITES_FOR[t]), None)
        if gold in VALUES[t]:
            top = np.sort(p[i])[::-1]
            out.append((float(top[0] - top[1]), VALUES[t][int(p[i].argmax())] == gold))
    return out


def value_bar(margins):
    for thr in np.arange(0.0, 1.0, 0.01):
        kept = [ok for m, ok in margins if m >= thr]
        if not kept or sum(kept) / len(kept) >= VALUE_PRECISION:
            return round(float(thr), 2)
    return 0.99


def value_report(margins, bar):
    grid = [0.0, 0.2, 0.4, 0.6, 0.8, 0.9, 0.95, bar]
    return {f"{g:.2f}": {"carded": sum(m >= g for m, _ in margins),
                         "wrong_direction": sum(m >= g and not ok for m, ok in margins)} for g in grid}


def evaluate(model, rows, emb, temps, bars):
    goals = [r["goal"] for r in rows]
    X = model.X(goals, emb)
    rep = {"n": len(rows), "types": {}}
    for t in TYPES:
        z = model.logits(X, ("act", t))
        p = softmax(z, temps[("act", t)])
        mass = sum(p[:, ACTS.index(a)] for a in WRITES_FOR[t])
        gold = np.array([act_of(r, t) in WRITES_FOR[t] for r in rows])
        pred = mass >= bars[t]
        tp, fp, fn = int((pred & gold).sum()), int((pred & ~gold).sum()), int((~pred & gold).sum())
        acts_gold = [act_of(r, t) for r in rows]
        acc = float(np.mean([ACTS[i] == a for i, a in zip(p.argmax(1), acts_gold)]))
        rep["types"][t] = {"gold": int(gold.sum()), "tp": tp, "fp": fp, "fn": fn,
                           "precision": tp / max(1, tp + fp), "recall": tp / max(1, tp + fn), "act_accuracy": acc}
    tot = {k: sum(v[k] for v in rep["types"].values()) for k in ("gold", "tp", "fp", "fn")}
    tot["precision"] = tot["tp"] / max(1, tot["tp"] + tot["fp"])
    tot["recall"] = tot["tp"] / max(1, tot["tp"] + tot["fn"])
    rep["all"] = tot
    # target accuracy on rows with a write candidate
    z = model.logits(X, ("target", ""))
    p = softmax(z, temps[("target", "")])
    pairs = [(TARGETS[p[i].argmax()], c["target"]) for i, r in enumerate(rows) for c in r["cands"][:1]
             if c["act"] in WRITES_FOR[c["type"]] and c["target"] in TARGETS and c["type"] != "profile"]
    rep["target_accuracy"] = float(np.mean([a == b for a, b in pairs])) if pairs else None
    return rep


def export(model, temps, bars, vbars):
    def vocab(vec):
        out = [None] * len(vec.vocabulary_)
        for g, i in vec.vocabulary_.items():
            out[i] = g
        return out
    heads = []
    for (kind, t), lr in model.heads.items():
        labels = Model.labels_of(kind, t)
        width = (len(model.char.vocabulary_) + len(model.jamo.vocabulary_) + len(model.tail.vocabulary_)
                 + len(model.last.vocabulary_) + 384)
        coef = np.zeros((len(labels), width), dtype=np.float64)
        icpt = np.full(len(labels), -30.0)
        if len(lr.classes_) == 2:
            w = lr.coef_[0]; b = lr.intercept_[0]
            rows = {lr.classes_[0]: (-w / 2, -b / 2), lr.classes_[1]: (w / 2, b / 2)}
        else:
            rows = {cl: (lr.coef_[j], lr.intercept_[j]) for j, cl in enumerate(lr.classes_)}
        for cl, (w, b) in rows.items():
            coef[labels.index(cl)] = np.asarray(w).ravel()
            icpt[labels.index(cl)] = b
        heads.append({"kind": kind, "type": t, "labels": labels, "temperature": temps[(kind, t)],
                      "coef_f32_b64": base64.b64encode(coef.astype("<f4").tobytes()).decode(),
                      "intercept": [float(v) for v in icpt]})
    return {
        "format": FORMAT, "version": VERSION, "encoder": ENCODER,
        "types": TYPES, "acts": ACTS, "values": VALUES, "targets": TARGETS, "writes_for": WRITES_FOR,
        "features": {"char": list(CHAR), "jamo": list(JAMO_R), "tail": list(TAIL_R), "tail_chars": TAIL_CHARS,
                     "last": list(LAST_R), "last_words": LAST_WORDS,
                     "embedding_weight": model.w, "sublinear_tf": True, "norm": "l2"},
        "card_bar": bars,
        "value_bar": vbars,
        "char_ngrams": vocab(model.char), "char_idf": [float(v) for v in model.char.idf_],
        "jamo_ngrams": vocab(model.jamo), "jamo_idf": [float(v) for v in model.jamo.idf_],
        "tail_ngrams": vocab(model.tail), "tail_idf": [float(v) for v in model.tail.idf_],
        "last_ngrams": vocab(model.last), "last_idf": [float(v) for v in model.last.idf_],
        "emb_mean": [float(v) for v in model.scaler.mean_], "emb_scale": [float(v) for v in model.scaler.scale_],
        "heads": heads,
    }


def exported_probs(art, model, goals, emb):
    """Probabilities from the exported (f32) weights, f64 math: what Rust computes."""
    X = model.X(goals, emb)
    out = {}
    for h in art["heads"]:
        coef = np.frombuffer(base64.b64decode(h["coef_f32_b64"]), dtype="<f4").astype(np.float64)
        coef = coef.reshape(len(h["labels"]), -1)
        z = X @ coef.T + np.array(h["intercept"])
        out[(h["kind"], h["type"])] = softmax(np.asarray(z), h["temperature"])
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model-dir", type=Path, required=True)
    ap.add_argument("--sealed", type=Path, required=True)
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--report", type=Path)
    a = ap.parse_args()
    rows = json.loads(ROWS.read_text())["rows"]
    sealed = {l.strip() for l in a.sealed.read_text().split() if len(l.strip()) == 64}
    hits = sum(hashlib.sha256(sealed_norm(r["goal"]).encode()).hexdigest() in sealed for r in rows)
    if hits:
        sys.exit(f"{hits} rows are sealed-batch goals")
    barred = set(json.loads((HERE / "data/guard_barred.json").read_text())["goals"])
    tr = [r for r in rows if r["split"] == "train"]
    dv = [r for r in rows if r["split"] == "dev" and r["goal"] not in barred]
    te = [r for r in rows if r["split"] == "test" and r["goal"] not in barred]
    print(f"dev {len(dv)}, test {len(te)} after the guard's {len(barred)} barred goals", file=sys.stderr)
    enc = Encoder(a.model_dir)
    E = {s: enc.encode([r["goal"] for r in rs]) for s, rs in (("train", tr), ("dev", dv), ("test", te))}
    best = None
    for c, w in GRID:
        m = Model(c, w).fit(tr, E["train"])
        Xd = m.X([r["goal"] for r in dv], E["dev"])
        temps = {}
        for key in m.heads:
            kind, t = key
            labels = Model.labels_of(kind, t)
            if kind == "act":
                y = [labels.index(act_of(r, t)) for r in dv]
                temps[key] = fit_temperature(m.logits(Xd, key), np.array(y))
            else:
                temps[key] = 1.0
        bars, strict = {}, {}
        for t in TYPES:
            p = softmax(m.logits(Xd, ("act", t)), temps[("act", t)])
            mass = sum(p[:, ACTS.index(x)] for x in WRITES_FOR[t])
            gold = np.array([act_of(r, t) in WRITES_FOR[t] for r in dv])
            bars[t] = card_bar(mass, gold)
            strict[t] = card_bar(mass, gold, PRECISION_STRICT)
        rep = evaluate(m, dv, E["dev"], temps, bars)
        print(f"C={c} w={w}: dev recall {rep['all']['recall']:.3f} precision {rep['all']['precision']:.3f}", file=sys.stderr)
        if best is None or rep["all"]["recall"] > best[0]["all"]["recall"]:
            best = (rep, c, w, m, temps, bars, strict)
    dev_rep, c, w, m, temps, bars, strict = best
    test_rep = evaluate(m, te, E["test"], temps, bars)
    strict_rep = {"bars": strict, "dev": evaluate(m, dv, E["dev"], temps, strict)["all"],
                  "test": evaluate(m, te, E["test"], temps, strict)["all"]}
    vbars, vreport = {}, {}
    for t in POLARITY:
        if ("value", t) not in m.heads or m.heads[("value", t)] is None:
            continue
        dm = value_margins(m, dv, E["dev"], t)
        vbars[t] = max(value_bar(dm), VALUE_BAR_FLOOR)
        vreport[t] = {"dev": value_report(dm, vbars[t]),
                      "test": value_report(value_margins(m, te, E["test"], t), vbars[t])}
    art = export(m, temps, bars, vbars)
    blob = json.dumps(art, ensure_ascii=False, separators=(",", ":")).encode()
    probe = [r["goal"] for r in dv[::max(1, len(dv) // 40)][:40]]
    # The final-clause feature's edges: one word, runs of spaces and tabs,
    # a correction, a non-breaking space.
    probe += ["알림", "  이거   알림 꺼줘\t아니  켜줘 ", "저장해줘 ㄴㄴ", "이거\u00a0찜 해제"]
    pe = enc.encode(probe)
    probs = exported_probs(art, m, probe, pe)
    parity = {"artefact_sha256": sha256(blob), "cases": [
        {"goal": g, "ids": enc.ids(g), "heads": {f"{k}:{t}": [float(x) for x in probs[(k, t)][i]] for (k, t) in probs}}
        for i, g in enumerate(probe)]}
    summary = {"selected": {"C": c, "embedding_weight": w}, "card_bar": bars,
               "temperatures": {f"{k}:{t}": v for (k, t), v in temps.items()}, "dev": dev_rep, "test": test_rep,
               "strict_0.97": strict_rep, "value_bar": vbars, "value_errors": vreport}
    if a.report:
        a.report.write_text(json.dumps(summary, ensure_ascii=False, indent=1))
    print(json.dumps({"selected": summary["selected"], "card_bar": bars, "dev_all": dev_rep["all"],
                      "test_all": test_rep["all"], "dev_target": dev_rep["target_accuracy"], "strict_0.97": strict_rep,
                      "value_bar": vbars, "value_errors": vreport}))
    if a.check:
        same = ARTEFACT.read_bytes() == blob
        print(json.dumps({"bytes_identical": same}))
        sys.exit(0 if same else 1)
    ARTEFACT.parent.mkdir(parents=True, exist_ok=True)
    ARTEFACT.write_bytes(blob)
    PARITY.write_text(json.dumps(parity, ensure_ascii=False, indent=0))
    print(f"wrote {ARTEFACT.relative_to(ROOT)} sha256={sha256(blob)} ({len(blob)} bytes)", file=sys.stderr)


if __name__ == "__main__":
    main()
