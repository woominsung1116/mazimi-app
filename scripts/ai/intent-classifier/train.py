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
"""Train the agent intent classifier and export it for the Rust runtime
(`crates/api/src/agent/classifier/`).

Features: char_wb 1-4-gram TF-IDF (sublinear tf, l2), char_wb 3-6-gram TF-IDF
of the goal spelled in jamo (each Hangul syllable as its initial, medial and
final letters, so `잇어`/`있어` and `지언`/`지원` share n-grams), and the
standardized multilingual-e5-small sentence embedding ("query: " prefix, mean
pooling, l2 norm) scaled by 0.15. Classifier: multinomial logistic regression,
C=32, balanced class weights. Labels: the tool sequence (`check_eligibility`
alone also covers check + notice quote) or `refuse`.

Topic head: a logistic regression (C=16, balanced) on the l2-normalized
embedding picks the catalogue field of a search goal (housing, employment,
education, finance, living, welfare) or `none` (a generic search). Routing
uses it only when the lexicon (`categories::fields_named`) names no field and
the top probability leads the second by the margin (0.3).

Training data: `data/train_v3.json` only (`data/gen_train.py`: template slots,
hand-written paraphrases, programmatic noise, and the older hand-written
`data/train_aug.json`), never an eval set. Every fixture under
`crates/api/tests/fixtures`, the fd dev set and any `--exclude` file are hashed
(exact, whitespace-free and NFKC letters-and-digits forms); a training goal
that collides is a hard error. Excluded sets are hashed only: the script
prints counts, never goal text. The dev split for model selection is
`agent_goals.json` plus the fd set (and `data/topic_dev.json` for the topic
head); the holdouts are for reports only.

    uv run --script scripts/ai/intent-classifier/train.py \
        --model-dir DIR_WITH_model.safetensors_AND_tokenizer.json \
        [--exclude /path/holdout_v4.json] [--check]

Writes `crates/api/assets/intent-classifier/classifier.json` (the artefact the
API loads, sha256-pinned in `classifier/mod.rs`) and
`crates/api/tests/fixtures/intent_classifier_parity.json` (the Rust parity
test's reference: token ids and probabilities on goals outside training).
`--check` retrains and compares with the committed artefact instead of
writing (reproducibility check).
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
import re
import sys
from collections import Counter
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parents[3]
HERE = Path(__file__).resolve().parent
FIX = ROOT / "crates/api/tests/fixtures"
TRAIN = HERE / "data/train_v3.json"
EVAL_FIXTURES = ["agent_goals.json", "agent_goals_holdout.json", "agent_goals_holdout_v2.json",
                 "agent_goals_holdout_v3.json", "agent_goals_holdout_v4.json", "assistant_topic_eval.json",
                 "agent_guard_corpus.json",
                 "agent_card_requests.json"]
DEFAULT_EXCLUDE = ([HERE / "data/fast_intent_dev.json", HERE / "data/topic_dev.json", HERE / "data/fraud_dev.json"]
                   + [FIX / n for n in EVAL_FIXTURES])
# sha256 of the NFKC letters-and-digits form of eval goals kept outside the
# repository (the blind holdout): hashes only.
EXTERNAL_HASHES = HERE / "data/external_eval_hashes.txt"
# The card-request corpora outside the repository (round corpora, re-review
# rows): the act head's train, dev and test goals, hashes only.
EXTERNAL_CARD_HASHES = HERE / "data/external_card_hashes.txt"
ARTEFACT = ROOT / "crates/api/assets/intent-classifier/classifier.json"
PARITY = FIX / "intent_classifier_parity.json"

# Official Hugging Face files, pinned (also in the Dockerfile and the Rust loader).
ENCODER = {
    "repo": "intfloat/multilingual-e5-small",
    "revision": "614241f622f53c4eeff9890bdc4f31cfecc418b3",
    "licence": "MIT",
    "model_sha256": "1a55775f53449dac10a2bcbc312469fac40b96d53198c407081a831f81c98477",
    "tokenizer_sha256": "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39",
    "prefix": "query: ",
    "max_tokens": 512,
}
FORMAT = "mazimi-intent-classifier/2"
VERSION = "e5s-char-jamo-lr-v3"
C = 32.0
EMB_WEIGHT = 0.15
THRESHOLD = 0.7
# The bar for a classifier refusal (summed refuse + refuse_fraud probability),
# set from the dev sweep on data/fraud_dev.json (0 false refusals on honest
# goals that name fraud terms; see the router-tuning handoff).
REFUSE_THRESHOLD = 0.99
JAMO_RANGE = (3, 6)
TOPIC_C = 16.0
TOPIC_MARGIN = 0.3
TOPIC_LABELS = ["housing", "employment", "education", "finance", "living", "welfare", "none"]
WRITES = ("bookmark", "set_alert", "toggle_checklist_item", "record_application_status", "open_form_fill")
CTX_TOOLS = {"check_eligibility", "get_preparation", "open_form_fill", "bookmark", "set_alert",
             "toggle_checklist_item", "record_application_status", "answer_from_notice"}
# Shapes the Rust classifier must also handle (parity only, never trained on).
EDGE_GOALS = [
    "", " ", "?", "ㅋㅋㅋ", "알림", "SQLD 시험 지원금", "any rent support for students in Daegu?",
    "부산장학금찾아줘", "  여러   칸   띄운   목표  ", "이모지 🎓 장학금 💸", "ＦＵＬＬＷＩＤＴＨ 장학금",
    "İstanbul DİKKAT ß ﬁ", "<s> 태그 </s> 섞인 목표", "\t탭과\n줄바꿈\r\n섞인 목표",
    "가" * 300,
]
# Tokenizer-only parity: Unicode shapes the normalizer and Viterbi must agree on.
TOKEN_EDGE = [
    "e\u0301 café CAFÉ", "한글 자모 ㅎㅏㄴㄱㅡㄹ ᄒᆞᆫ", "👩\u200d👩\u200d👧 가족 🇰🇷", "\u00a0nbsp\u3000ideo", "a\u200bb\u200cc\u200dd",
    "<mask><s></s><pad><unk>", "<s><s>x</s>", "a</s>b<s", "<<s>>", "\x00\x01\x7f 제어", "東京 北京 ア ｱ",
    "مرحبا שלום Привет Γειά", "x" * 40 + "y" * 3, "▁ ▁▁ 메타", "ㅋㅋㅋㅋㅋㅋㅋㅋ!!!???", "１２３ ①②③ Ⅷ ㎏ ℃", "   ",
    "ﾊﾝｶｸ ｶﾀｶﾅ", "\ud7a3\uac00", "𝓜𝓪𝓽𝓱 𝟙𝟚", "tab\tsep\vvt\fff", "a  b   c    d", "x" * 2000,
]


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def goal_hashes(goal: str) -> set[str]:
    return {sha256(goal.strip().encode()), sha256(re.sub(r"\s+", "", goal).encode())}


def norm_hash(goal: str) -> str:
    """NFKC, lowercase, letters and digits only (`data/gen_train.py`'s dedupe)."""
    import unicodedata
    g = unicodedata.normalize("NFKC", goal).lower()
    return sha256("".join(ch for ch in g if ch.isalnum()).encode())


CHO = "ㄱㄲㄴㄷㄸㄹㅁㅂㅃㅅㅆㅇㅈㅉㅊㅋㅌㅍㅎ"
JUNG = "ㅏㅐㅑㅒㅓㅔㅕㅖㅗㅘㅙㅚㅛㅜㅝㅞㅟㅠㅡㅢㅣ"
JONG = [""] + list("ㄱㄲㄳㄴㄵㄶㄷㄹㄺㄻㄼㄽㄾㄿㅀㅁㅂㅄㅅㅆㅇㅈㅊㅋㅌㅍㅎ")


def jamo(text: str) -> str:
    """Lowercase, then every precomposed Hangul syllable as its letters
    (compatibility jamo); other characters unchanged. The Rust port is
    `classifier::jamo`."""
    out = []
    for ch in text.lower():
        o = ord(ch) - 0xAC00
        out.append(CHO[o // 588] + JUNG[(o % 588) // 28] + JONG[o % 28] if 0 <= o < 11172 else ch)
    return "".join(out)


def build_pool(excludes: list[Path]):
    """Training rows from `data/train_v3.json`; a goal that collides with an
    eval set is an error (the generator already dropped them)."""
    banned: set[str] = set()
    n_eval = 0
    for path in excludes:
        if path.exists():
            data = json.loads(path.read_text())
            cases = data if not isinstance(data, dict) else data.get("cases", data.get("rows", []))
            for case in cases:
                if not isinstance(case, dict):
                    continue
                goal = case.get("goal") or case.get("message")
                if isinstance(goal, str):
                    banned |= goal_hashes(goal) | {norm_hash(goal)}
                    n_eval += 1
        else:
            print(f"exclude fixture not found (skipped): {path.name}", file=sys.stderr)
    for hashes in (EXTERNAL_HASHES, EXTERNAL_CARD_HASHES):
        if hashes.exists():
            external = {h for h in hashes.read_text().split() if len(h) == 64}
            banned |= external
            n_eval += len(external)
    data = json.loads(TRAIN.read_text())
    rows = [(r["goal"], r["label"], r["fields"]) for r in data["rows"]]
    overlap = sum(1 for g, _, _ in rows if not (goal_hashes(g) | {norm_hash(g)}).isdisjoint(banned))
    if overlap:
        sys.exit(f"{overlap} training goals collide with an eval set: regenerate data/train_v3.json")
    return rows, n_eval


class Encoder:
    """fp32 BERT forward (transformers), the reference the Rust port matches."""

    def __init__(self, model_dir: Path):
        import torch
        from safetensors.torch import load_file
        from tokenizers import Tokenizer
        from transformers import BertConfig, BertModel

        for name, key in (("model.safetensors", "model_sha256"), ("tokenizer.json", "tokenizer_sha256")):
            got = sha256((model_dir / name).read_bytes())
            if got != ENCODER[key]:
                sys.exit(f"{name}: sha256 {got} is not the pinned {ENCODER[key]}")
        torch.set_num_threads(4)
        config = BertConfig(vocab_size=250037, hidden_size=384, num_hidden_layers=12, num_attention_heads=12,
                            intermediate_size=1536, hidden_act="gelu", max_position_embeddings=512,
                            type_vocab_size=2, layer_norm_eps=1e-12, attn_implementation="eager")
        self.model = BertModel(config, add_pooling_layer=False).eval()
        state = {k: v for k, v in load_file(str(model_dir / "model.safetensors")).items()
                 if not k.startswith("pooler.") and k != "embeddings.position_ids"}
        missing, unexpected = self.model.load_state_dict(state, strict=False)
        assert not unexpected and not missing, (missing, unexpected)
        self.tok = Tokenizer.from_file(str(model_dir / "tokenizer.json"))
        self.tok.enable_truncation(ENCODER["max_tokens"])
        self.torch = torch

    def ids(self, text: str) -> list[int]:
        return self.tok.encode(ENCODER["prefix"] + text).ids

    def encode(self, texts: list[str]) -> np.ndarray:
        torch = self.torch
        out = []
        with torch.no_grad():
            for text in texts:  # batch 1: the serving shape, no padding
                ids = torch.tensor([self.ids(text)])
                hidden = self.model(input_ids=ids, attention_mask=torch.ones_like(ids),
                                    token_type_ids=torch.zeros_like(ids)).last_hidden_state[0]
                v = hidden.mean(0)
                out.append((v / v.norm()).numpy())
        return np.array(out, dtype=np.float64)


class Model:
    """char + jamo TF-IDF + scaled embedding, logistic regression; and the
    topic head on the embedding."""

    def __init__(self, encoder: Encoder):
        self.encoder = encoder

    def fit(self, texts, labels, emb=None):
        from sklearn.feature_extraction.text import TfidfVectorizer
        from sklearn.linear_model import LogisticRegression
        from sklearn.preprocessing import StandardScaler
        self.char = TfidfVectorizer(analyzer="char_wb", ngram_range=(1, 4), sublinear_tf=True).fit(texts)
        self.jamo = TfidfVectorizer(analyzer="char_wb", ngram_range=JAMO_RANGE, sublinear_tf=True,
                                    preprocessor=jamo).fit(texts)
        emb = self.encoder.encode(texts) if emb is None else emb
        self.scaler = StandardScaler().fit(emb)
        self.lr = LogisticRegression(C=C, max_iter=5000, class_weight="balanced").fit(self.features(texts, emb),
                                                                                       labels)
        return self

    def features(self, texts, emb=None):
        import scipy.sparse as sp
        emb = self.encoder.encode(texts) if emb is None else emb
        dense = sp.csr_matrix(self.scaler.transform(emb) * EMB_WEIGHT)
        return sp.hstack([self.char.transform(texts), self.jamo.transform(texts), dense]).tocsr()

    def proba(self, texts, emb=None):
        return self.lr.predict_proba(self.features(texts, emb))


def topic_rows(rows, prototypes):
    """(text, label): search rows by their first field, generic searches as
    `none`, and the curated field descriptions."""
    out = [(g, fields[0] if fields else "none") for g, label, fields in rows if label == "search_benefits"]
    out += [(p, f) for f in TOPIC_LABELS[:-1] for p in prototypes[f]]
    return out


def fit_topic(emb):
    from sklearn.linear_model import LogisticRegression
    return lambda y: LogisticRegression(C=TOPIC_C, max_iter=5000, class_weight="balanced").fit(emb, y)


def topic_pick(p, labels):
    """The field, or None (`none`, or below the margin): the Rust rule."""
    order = np.argsort(-p)
    best = labels[order[0]]
    return best if best != "none" and p[order[0]] - p[order[1]] >= TOPIC_MARGIN else None


def fd_ok(label: str, case) -> bool:
    exp = case["expect"]
    label = "refuse" if label.startswith("refuse") else label  # refuse_fraud is a refusal
    if exp == "needs_program":
        return label != "refuse" and any(t in CTX_TOOLS for t in label.split("+"))
    return label == seq_label(exp)


def seq_label(expect) -> str | None:
    if isinstance(expect, str):
        expect = {expect: None}
    if "plan" not in expect:
        k = next(iter(expect))
        return "refuse" if k in ("refuse", "no_plan") else None
    tools = [s["tool"] for s in expect["plan"]["steps"]]
    if tools == ["check_eligibility", "answer_from_notice"]:
        tools = ["check_eligibility"]
    return "+".join(tools)


def dev_report(art, model, encoder):
    """Accuracy and threshold coverage on the dev split (dev fixture + fd);
    profile and v3-tool rows are the rules' and are left out."""
    cases = [c for name in ("agent_goals.json",) for c in json.loads((FIX / name).read_text())["cases"]]
    cases += json.loads((HERE / "data/fast_intent_dev.json").read_text())["cases"]
    skip = ("profile_patch", "check_official_page", "document_issue_link", "add_to_calendar")
    cases = [c for c in cases if not (isinstance(c["expect"], dict) and "plan" in c["expect"]
                                      and any(s["tool"] in skip for s in c["expect"]["plan"]["steps"]))]
    goals = [c["goal"] for c in cases]
    p = exported_proba(art, model, goals, encoder.encode(goals))
    lab = [art["labels"][i] for i in p.argmax(1)]
    good = np.array([fd_ok(label, c) for label, c in zip(lab, cases)])
    conf = p.max(1)
    acc = conf >= THRESHOLD
    return {"dev_n": len(cases), "dev_accuracy": round(float(good.mean()), 4),
            "dev_coverage_at_threshold": round(float(acc.mean()), 4),
            "dev_accuracy_at_threshold": round(float(good[acc].mean()), 4),
            "dev_bad_to_write": int(sum(any(w in label for w in WRITES) for label, c in zip(lab, cases)
                                        if c["category"] in ("injection", "negative", "clarify")))}


def export(model: Model, topic, texts, labels, n_eval, excludes) -> dict:
    def vocab_of(vec):
        out = [None] * len(vec.vocabulary_)
        for ngram, i in vec.vocabulary_.items():
            out[i] = ngram
        return out
    coef = model.lr.coef_.astype("<f4")
    pool = json.dumps(sorted([g, label] for g, label in zip(texts, labels)), ensure_ascii=False).encode()
    import sklearn
    import tokenizers
    import torch
    import transformers
    topic_labels = [str(c) for c in topic.classes_]
    assert sorted(topic_labels) == sorted(TOPIC_LABELS)
    return {
        "format": FORMAT,
        "version": VERSION,
        "encoder": ENCODER,
        "features": {"char": {"analyzer": "char_wb", "ngram_range": [1, 4], "lowercase": True,
                              "sublinear_tf": True, "norm": "l2", "smooth_idf": True},
                     "jamo": {"analyzer": "char_wb", "ngram_range": list(JAMO_RANGE), "preprocessor": "jamo",
                              "sublinear_tf": True, "norm": "l2", "smooth_idf": True},
                     "embedding_weight": EMB_WEIGHT},
        "classifier": {"kind": "multinomial_logistic_regression", "C": C, "class_weight": "balanced",
                       "threshold": THRESHOLD, "refuse_threshold": REFUSE_THRESHOLD},
        "training": {
            "goals": len(texts),
            "classes": len(model.lr.classes_),
            # Checked against every eval set at training time (build_pool);
            # counts are printed, not stored, so new eval rows upstream do not
            # change the artefact.
            "train_overlap": 0,
            "excluded_fixtures": [p.name for p in excludes] + ([EXTERNAL_HASHES.name] if EXTERNAL_HASHES.exists() else []),
            "sources": ["data/train_v3.json"],
            "pool_sha256": sha256(pool),
            "goal_sha256": sorted({h for g in texts for h in goal_hashes(g)}),
            "libraries": {"scikit-learn": sklearn.__version__, "torch": torch.__version__,
                          "transformers": transformers.__version__, "tokenizers": tokenizers.__version__,
                          "numpy": np.__version__},
        },
        "labels": [str(c) for c in model.lr.classes_],
        "char_ngrams": vocab_of(model.char),
        "char_idf": [float(v) for v in model.char.idf_],
        "jamo_ngrams": vocab_of(model.jamo),
        "jamo_idf": [float(v) for v in model.jamo.idf_],
        "emb_mean": [float(v) for v in model.scaler.mean_],
        "emb_scale": [float(v) for v in model.scaler.scale_],
        "coef_f32_b64": base64.b64encode(coef.tobytes()).decode(),
        "intercept": [float(v) for v in model.lr.intercept_],
        "topic": {"labels": topic_labels, "C": TOPIC_C, "margin": TOPIC_MARGIN,
                  "coef_f32_b64": base64.b64encode(topic.coef_.astype("<f4").tobytes()).decode(),
                  "intercept": [float(v) for v in topic.intercept_]},
    }


def exported_proba(art: dict, model: Model, texts, emb) -> np.ndarray:
    """Probabilities from the exported (f32) weights, f64 math: what Rust computes."""
    width = len(art["char_ngrams"]) + len(art["jamo_ngrams"]) + len(art["emb_mean"])
    coef = np.frombuffer(base64.b64decode(art["coef_f32_b64"]), dtype="<f4").astype(np.float64)
    coef = coef.reshape(len(art["labels"]), width)
    logits = model.features(texts, emb) @ coef.T + np.array(art["intercept"])
    logits -= logits.max(1, keepdims=True)
    p = np.exp(logits)
    return p / p.sum(1, keepdims=True)


def exported_topic(art: dict, emb) -> np.ndarray:
    t = art["topic"]
    coef = np.frombuffer(base64.b64decode(t["coef_f32_b64"]), dtype="<f4").astype(np.float64)
    coef = coef.reshape(len(t["labels"]), emb.shape[1])
    logits = emb @ coef.T + np.array(t["intercept"])
    logits -= logits.max(1, keepdims=True)
    p = np.exp(logits)
    return p / p.sum(1, keepdims=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model-dir", type=Path, required=True)
    ap.add_argument("--exclude", type=Path, action="append", default=[])
    ap.add_argument("--check", action="store_true", help="compare with the committed artefact, write nothing")
    args = ap.parse_args()
    excludes = DEFAULT_EXCLUDE + args.exclude
    rows, n_eval = build_pool(excludes)
    texts, labels = [g for g, _, _ in rows], [label for _, label, _ in rows]
    print(f"pool {len(texts)} goals, {len(set(labels))} classes, {n_eval} eval goals hashed, overlap 0")
    encoder = Encoder(args.model_dir)
    emb = encoder.encode(texts)
    model = Model(encoder).fit(texts, labels, emb)
    prototypes = json.loads((HERE / "data/topic_prototypes.json").read_text())
    trows = topic_rows(rows, prototypes)
    by_text = dict(zip(texts, emb))
    t_emb = np.array([by_text[g] if g in by_text else encoder.encode([g])[0] for g, _ in trows])
    topic = fit_topic(t_emb)([y for _, y in trows])
    art = export(model, topic, texts, labels, n_eval, excludes)

    # The export reproduces sklearn's probabilities.
    check = exported_proba(art, model, texts, emb)
    assert np.abs(check - model.proba(texts, emb)).max() < 1e-5
    assert np.abs(exported_topic(art, t_emb) - topic.predict_proba(t_emb)).max() < 1e-5

    metrics = dev_report(art, model, encoder)
    tdev = json.loads((HERE / "data/topic_dev.json").read_text())
    tp = exported_topic(art, encoder.encode([d["goal"] for d in tdev]))
    picks = [topic_pick(p, art["topic"]["labels"]) for p in tp]
    answered = [(f, d) for f, d in zip(picks, tdev) if f]
    metrics |= {"topic_dev_n": len(tdev), "topic_dev_coverage": round(len(answered) / len(tdev), 4),
                "topic_dev_accuracy_answered": round(sum(f in d["fields"] for f, d in answered) / max(1, len(answered)), 4)}
    art["training"]["metrics"] = metrics
    print(json.dumps(metrics))

    fd = json.loads((HERE / "data/fast_intent_dev.json").read_text())["cases"]
    parity_goals = [c["goal"] for c in fd] + [d["goal"] for d in tdev] + EDGE_GOALS
    parity_emb = encoder.encode(parity_goals)
    parity_p = exported_proba(art, model, parity_goals, parity_emb)
    parity_t = exported_topic(art, parity_emb)
    parity = {
        "about": "Reference outputs of scripts/ai/intent-classifier/train.py for the Rust parity test "
                 "(cases: fd dev goals, topic dev goals and edge shapes, none a training goal; token_cases: "
                 "tokenizer ids for every training goal and Unicode edge strings).",
        "artefact_version": VERSION,
        "token_cases": [{"text": t, "ids": encoder.ids(t)} for t in texts + TOKEN_EDGE],
        "cases": [{"goal": g, "ids": encoder.ids(g), "label": art["labels"][int(p.argmax())],
                   "probabilities": [round(float(v), 8) for v in p],
                   "topic": topic_pick(q, art["topic"]["labels"]),
                   "topic_probabilities": [round(float(v), 8) for v in q]}
                  for g, p, q in zip(parity_goals, parity_p, parity_t)],
    }
    text = json.dumps(art, ensure_ascii=False, separators=(",", ":")) + "\n"
    if args.check:
        old = json.loads(ARTEFACT.read_text())
        keys = ("labels", "char_ngrams", "char_idf", "jamo_ngrams", "jamo_idf", "emb_mean", "emb_scale")
        same = {k: old.get(k) == art[k] for k in keys}
        diffs = {}
        for name, a, b in (("coef", old["coef_f32_b64"], art["coef_f32_b64"]),
                           ("topic", old["topic"]["coef_f32_b64"], art["topic"]["coef_f32_b64"])):
            x = np.frombuffer(base64.b64decode(a), dtype="<f4")
            y = np.frombuffer(base64.b64decode(b), dtype="<f4")
            diffs[name] = float(np.abs(x - y).max()) if x.shape == y.shape else "shape"
        same["pool_sha256"] = old["training"]["pool_sha256"] == art["training"]["pool_sha256"]
        same["bytes_identical"] = ARTEFACT.read_text() == text
        print(json.dumps(same), "max |coef diff|", json.dumps(diffs))
        return
    ARTEFACT.parent.mkdir(parents=True, exist_ok=True)
    ARTEFACT.write_text(text)
    PARITY.write_text(json.dumps(parity, ensure_ascii=False, indent=1) + "\n")
    print(f"wrote {ARTEFACT.relative_to(ROOT)} sha256={sha256(text.encode())} ({len(text.encode())} bytes)")
    print(f"wrote {PARITY.relative_to(ROOT)} ({len(parity['cases'])} goals)")


if __name__ == "__main__":
    main()
