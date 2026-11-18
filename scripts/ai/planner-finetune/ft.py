"""LoRA fine-tune of a small instruction model on the planner SFT pairs
(`sft.py`), on Apple silicon with MLX. The token layout is fixed here, not by
the model's chat template: ChatML with the tuned system turn, no think block,
loss on the reply only. `export.sh`'s Ollama template renders the same text.

    python ft.py --model HF_DIR --data sft.json --out ADAPTER_DIR [--iters 600 --batch 8]
"""
import argparse, json, pathlib
import mlx.core as mx, mlx.optimizers as optim
from mlx_lm import load
from mlx_lm.tuner.utils import linear_to_lora_layers, print_trainable_parameters
from mlx_lm.tuner.trainer import TrainingArgs, train
from mlx_lm.tuner.datasets import CacheDataset

ap = argparse.ArgumentParser()
ap.add_argument("--model"); ap.add_argument("--data"); ap.add_argument("--out")
ap.add_argument("--iters", type=int, default=600); ap.add_argument("--rank", type=int, default=16)
ap.add_argument("--lr", type=float, default=2e-4); ap.add_argument("--batch", type=int, default=8)
ap.add_argument("--layers", type=int, default=-1); ap.add_argument("--seed", type=int, default=0)
ap.add_argument("--accum", type=int, default=1); ap.add_argument("--max-len", type=int, default=512)
ap.add_argument("--save-every", type=int, default=200); ap.add_argument("--resume", default=None)
ap.add_argument("--grad-checkpoint", action="store_true")
ap.add_argument("--mem-limit-gb", type=float, default=6.0)
a = ap.parse_args()
# Keep the run within the host budget (Metal allocations and the buffer cache).
mx.set_memory_limit(int(a.mem_limit_gb * 1024**3))
mx.set_cache_limit(512 * 1024**2)
d = json.load(open(a.data))
SYSTEM = d["system"]


def prefix(user):
    return f"<|im_start|>system\n{SYSTEM}<|im_end|>\n<|im_start|>user\n{user}<|im_end|>\n<|im_start|>assistant\n"


model, tok = load(a.model)


class DS:
    def __init__(self, rows): self.rows = rows
    def __len__(self): return len(self.rows)
    def __getitem__(self, i): return self.rows[i]
    def process(self, r):
        p = tok.encode(prefix(r["prompt"]), add_special_tokens=False)
        c = tok.encode(r["reply"] + "<|im_end|>", add_special_tokens=False)
        return (p + c, len(p))


mx.random.seed(a.seed)
model.freeze()
layers = len(model.layers) if a.layers < 0 else a.layers
lora = {"rank": a.rank, "scale": 2.0, "dropout": 0.05,
        "keys": ["self_attn.q_proj", "self_attn.k_proj", "self_attn.v_proj", "self_attn.o_proj",
                 "mlp.gate_proj", "mlp.up_proj", "mlp.down_proj"]}
linear_to_lora_layers(model, layers, lora)
if a.resume:
    model.load_weights(a.resume, strict=False)
print_trainable_parameters(model)
out = pathlib.Path(a.out); out.mkdir(parents=True, exist_ok=True)
json.dump({"fine_tune_type": "lora", "num_layers": layers, "lora_parameters": lora, "args": vars(a)},
          open(out / "adapter_config.json", "w"))
sched = optim.cosine_decay(a.lr, a.iters, a.lr * 0.05)
opt = optim.AdamW(learning_rate=sched, weight_decay=0.0)
# Bounded memory: small batches with accumulation, a capped sequence length
# (the longest pair is ~200 tokens) and checkpoints every --save-every.
targs = TrainingArgs(batch_size=a.batch, iters=a.iters, val_batches=-1, steps_per_report=50, steps_per_eval=200,
                     steps_per_save=a.save_every, adapter_file=out / "adapters.safetensors",
                     max_seq_length=a.max_len, grad_accumulation_steps=a.accum,
                     grad_checkpoint=a.grad_checkpoint)
train(model=model, args=targs, optimizer=opt, train_dataset=CacheDataset(DS(d["train"])),
      val_dataset=CacheDataset(DS(d["valid"])))
