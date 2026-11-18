
import sys, json, numpy as np, importlib.util, pathlib
W, M, CAL, OUT = sys.argv[1:5]
spec = importlib.util.spec_from_file_location("train", W + "/scripts/ai/intent-classifier/train.py")
train = importlib.util.module_from_spec(spec); spec.loader.exec_module(train)
enc = train.Encoder(pathlib.Path(M))
cal = json.load(open(CAL))
def passage(c): return (c["title"] + ". " + c["summary"][:200]).strip()
qs = sorted({c["prod_query"] for c in cal}); ps = sorted({passage(c) for c in cal})
train.ENCODER["prefix"] = "query: "; Q = dict(zip(qs, enc.encode(qs)))
train.ENCODER["prefix"] = "passage: "; P = dict(zip(ps, enc.encode(ps)))
json.dump([float(Q[c["prod_query"]] @ P[passage(c)]) for c in cal], open(OUT, "w"))
