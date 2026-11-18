//! multilingual-e5-small sentence encoder in plain Rust: its tokenizer
//! ([`super::tokenizer`]) and a 12-layer BERT forward pass
//! on `ndarray` (matrixmultiply kernels), fp32, CPU. Matches the training
//! script's reference (`transformers` BertModel, batch 1, mean pooling, l2
//! norm) to ~1e-6.
//!
//! Memory: the encoder layers (~85 MB fp32) are loaded; the 250k-row word
//! embedding table (~384 MB) is not. Each goal's token rows are read from
//! the sha256-verified file on demand with positional reads, so they cost
//! page cache, not process RSS.
use super::tokenizer::Tokenizer;
use ndarray::{linalg::general_mat_mul, s, Array1, Array2, ArrayView2, Axis};
use rayon::prelude::*;
use serde::Deserialize;
use std::{collections::HashMap, fs::File, os::unix::fs::FileExt};

pub(super) const HIDDEN: usize = 384;
const LAYERS: usize = 12;
const HEADS: usize = 12;
const HEAD_DIM: usize = HIDDEN / HEADS;
const INTERMEDIATE: usize = 1536;
const MAX_POSITIONS: usize = 512;
const VOCAB: usize = 250_037;
const LAYER_NORM_EPS: f32 = 1e-12;
/// A safetensors header is a few tens of KB; refuse anything absurd.
const MAX_HEADER_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Deserialize)]
struct TensorInfo {
    dtype: String,
    shape: Vec<usize>,
    data_offsets: [u64; 2],
}

struct Linear {
    /// `[out, in]`, as stored by PyTorch.
    weight: Array2<f32>,
    bias: Array1<f32>,
}

struct LayerNorm {
    gamma: Array1<f32>,
    beta: Array1<f32>,
}

struct Layer {
    query: Linear,
    key: Linear,
    value: Linear,
    attention_out: Linear,
    attention_norm: LayerNorm,
    intermediate: Linear,
    output: Linear,
    output_norm: LayerNorm,
}

pub(super) struct Encoder {
    tokenizer: Tokenizer,
    prefix: String,
    /// `model.safetensors`, kept open for the word-embedding rows.
    file: File,
    word_offset: u64,
    positions: Array2<f32>,
    token_type: Array1<f32>,
    embedding_norm: LayerNorm,
    layers: Vec<Layer>,
}

struct Tensors {
    file: File,
    data_start: u64,
    index: HashMap<String, TensorInfo>,
}

impl Tensors {
    fn open(file: File) -> Result<Self, String> {
        let mut len = [0u8; 8];
        file.read_exact_at(&mut len, 0)
            .map_err(|e| format!("read model header: {e}"))?;
        let header_len = u64::from_le_bytes(len);
        if header_len > MAX_HEADER_BYTES {
            return Err("model header too large".into());
        }
        let mut header = vec![0u8; header_len as usize];
        file.read_exact_at(&mut header, 8)
            .map_err(|e| format!("read model header: {e}"))?;
        let mut raw: HashMap<String, serde_json::Value> =
            serde_json::from_slice(&header).map_err(|e| format!("model header: {e}"))?;
        raw.remove("__metadata__");
        let index = raw
            .into_iter()
            .map(|(name, value)| {
                serde_json::from_value::<TensorInfo>(value)
                    .map(|info| (name, info))
                    .map_err(|e| format!("model header: {e}"))
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            file,
            data_start: 8 + header_len,
            index,
        })
    }

    /// Absolute offset of an f32 tensor with exactly this shape.
    fn locate(&self, name: &str, shape: &[usize]) -> Result<u64, String> {
        let info = self
            .index
            .get(name)
            .ok_or_else(|| format!("tensor {name} missing"))?;
        let bytes = shape.iter().product::<usize>() as u64 * 4;
        if info.dtype != "F32"
            || info.shape != shape
            || info.data_offsets[1].checked_sub(info.data_offsets[0]) != Some(bytes)
        {
            return Err(format!("tensor {name} has an unexpected dtype or shape"));
        }
        Ok(self.data_start + info.data_offsets[0])
    }

    fn read(&self, name: &str, shape: &[usize]) -> Result<Vec<f32>, String> {
        let offset = self.locate(name, shape)?;
        let mut bytes = vec![0u8; shape.iter().product::<usize>() * 4];
        self.file
            .read_exact_at(&mut bytes, offset)
            .map_err(|e| format!("read {name}: {e}"))?;
        Ok(f32s(&bytes))
    }

    fn vector(&self, name: &str, len: usize) -> Result<Array1<f32>, String> {
        Ok(Array1::from(self.read(name, &[len])?))
    }

    fn matrix(&self, name: &str, rows: usize, cols: usize) -> Result<Array2<f32>, String> {
        Array2::from_shape_vec((rows, cols), self.read(name, &[rows, cols])?)
            .map_err(|e| format!("{name}: {e}"))
    }

    fn linear(&self, prefix: &str, out: usize, input: usize) -> Result<Linear, String> {
        Ok(Linear {
            weight: self.matrix(&format!("{prefix}.weight"), out, input)?,
            bias: self.vector(&format!("{prefix}.bias"), out)?,
        })
    }

    fn norm(&self, prefix: &str) -> Result<LayerNorm, String> {
        Ok(LayerNorm {
            gamma: self.vector(&format!("{prefix}.weight"), HIDDEN)?,
            beta: self.vector(&format!("{prefix}.bias"), HIDDEN)?,
        })
    }
}

fn f32s(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

impl LayerNorm {
    /// Row-wise, in place; statistics accumulated in f64.
    fn apply(&self, x: &mut Array2<f32>) {
        for mut row in x.rows_mut() {
            let n = row.len() as f64;
            let mean = row.iter().map(|v| f64::from(*v)).sum::<f64>() / n;
            let var = row
                .iter()
                .map(|v| (f64::from(*v) - mean).powi(2))
                .sum::<f64>()
                / n;
            let inv = 1.0 / (var + f64::from(LAYER_NORM_EPS)).sqrt();
            for ((v, g), b) in row.iter_mut().zip(&self.gamma).zip(&self.beta) {
                *v = ((f64::from(*v) - mean) * inv) as f32 * g + b;
            }
        }
    }
}

/// `x · Wᵀ + b`. With more than one thread the output features are split
/// across the current rayon pool.
fn linear(x: &Array2<f32>, layer: &Linear, threads: usize) -> Array2<f32> {
    let out = layer.weight.nrows();
    let mut y = Array2::<f32>::zeros((x.nrows(), out));
    if threads > 1 {
        let chunk = out.div_ceil(threads);
        y.axis_chunks_iter_mut(Axis(1), chunk)
            .into_par_iter()
            .zip(layer.weight.axis_chunks_iter(Axis(0), chunk))
            .for_each(|(mut y, w)| general_mat_mul(1.0, x, &w.t(), 0.0, &mut y));
    } else {
        general_mat_mul(1.0, x, &layer.weight.t(), 0.0, &mut y);
    }
    y += &layer.bias;
    y
}

/// Unmasked multi-head self-attention (batch 1, no padding).
fn attention(q: &Array2<f32>, k: &Array2<f32>, v: &Array2<f32>) -> Array2<f32> {
    let n = q.nrows();
    let scale = 1.0 / (HEAD_DIM as f32).sqrt();
    let mut context = Array2::<f32>::zeros((n, HIDDEN));
    context
        .axis_chunks_iter_mut(Axis(1), HEAD_DIM)
        .into_par_iter()
        .enumerate()
        .for_each(|(h, mut out)| {
            let cols = s![.., h * HEAD_DIM..(h + 1) * HEAD_DIM];
            let (qh, kh, vh): (ArrayView2<f32>, ArrayView2<f32>, ArrayView2<f32>) =
                (q.slice(cols), k.slice(cols), v.slice(cols));
            let mut scores = Array2::<f32>::zeros((n, n));
            general_mat_mul(scale, &qh, &kh.t(), 0.0, &mut scores);
            for mut row in scores.rows_mut() {
                let max = row.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                row.mapv_inplace(|s| (s - max).exp());
                let sum = row.sum();
                row.mapv_inplace(|s| s / sum);
            }
            general_mat_mul(1.0, &scores, &vh, 0.0, &mut out);
        });
    context
}

fn gelu(x: f32) -> f32 {
    0.5 * x * (1.0 + libm::erff(x * std::f32::consts::FRAC_1_SQRT_2))
}

impl Encoder {
    /// `model`: the open, already verified `model.safetensors`.
    pub(super) fn load(model: File, tokenizer: &[u8], prefix: &str) -> Result<Self, String> {
        let tokenizer = Tokenizer::from_json(tokenizer)?;
        let tensors = Tensors::open(model)?;
        let word_offset = tensors.locate("embeddings.word_embeddings.weight", &[VOCAB, HIDDEN])?;
        let layers = (0..LAYERS)
            .map(|i| {
                let p = format!("encoder.layer.{i}");
                Ok(Layer {
                    query: tensors.linear(&format!("{p}.attention.self.query"), HIDDEN, HIDDEN)?,
                    key: tensors.linear(&format!("{p}.attention.self.key"), HIDDEN, HIDDEN)?,
                    value: tensors.linear(&format!("{p}.attention.self.value"), HIDDEN, HIDDEN)?,
                    attention_out: tensors.linear(
                        &format!("{p}.attention.output.dense"),
                        HIDDEN,
                        HIDDEN,
                    )?,
                    attention_norm: tensors.norm(&format!("{p}.attention.output.LayerNorm"))?,
                    intermediate: tensors.linear(
                        &format!("{p}.intermediate.dense"),
                        INTERMEDIATE,
                        HIDDEN,
                    )?,
                    output: tensors.linear(&format!("{p}.output.dense"), HIDDEN, INTERMEDIATE)?,
                    output_norm: tensors.norm(&format!("{p}.output.LayerNorm"))?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let token_types = tensors.matrix("embeddings.token_type_embeddings.weight", 2, HIDDEN)?;
        Ok(Self {
            tokenizer,
            prefix: prefix.to_owned(),
            positions: tensors.matrix(
                "embeddings.position_embeddings.weight",
                MAX_POSITIONS,
                HIDDEN,
            )?,
            token_type: token_types.row(0).to_owned(),
            embedding_norm: tensors.norm("embeddings.LayerNorm")?,
            layers,
            word_offset,
            file: tensors.file,
        })
    }

    /// Token ids of `prefix + text`, with `<s>`/`</s>`, truncated to 512.
    pub(super) fn ids(&self, text: &str) -> Result<Vec<u32>, String> {
        Ok(self.tokenizer.encode(&format!("{}{text}", self.prefix)))
    }

    /// The l2-normalized mean-pooled embedding. Call inside the classifier's
    /// thread pool; `threads` is that pool's size.
    pub(super) fn embed(&self, text: &str, threads: usize) -> Result<Vec<f32>, String> {
        self.embed_ids(self.ids(text)?, threads)
    }

    /// [`Self::embed`] with another e5 prefix (`passage: ` for programme
    /// text in the relevance reranker).
    pub(super) fn embed_prefixed(
        &self,
        prefix: &str,
        text: &str,
        threads: usize,
    ) -> Result<Vec<f32>, String> {
        self.embed_ids(self.tokenizer.encode(&format!("{prefix}{text}")), threads)
    }

    fn embed_ids(&self, ids: Vec<u32>, threads: usize) -> Result<Vec<f32>, String> {
        let n = ids.len();
        if n == 0 || n > MAX_POSITIONS {
            return Err("token count out of range".into());
        }
        let mut row = vec![0u8; HIDDEN * 4];
        let mut x = Array2::<f32>::zeros((n, HIDDEN));
        for (i, id) in ids.iter().enumerate() {
            let id = *id as usize;
            if id >= VOCAB {
                return Err("token id out of range".into());
            }
            self.file
                .read_exact_at(&mut row, self.word_offset + (id * HIDDEN * 4) as u64)
                .map_err(|e| format!("read embedding row: {e}"))?;
            let mut out = x.row_mut(i);
            for (j, v) in f32s(&row).into_iter().enumerate() {
                out[j] = v + self.positions[[i, j]] + self.token_type[j];
            }
        }
        self.embedding_norm.apply(&mut x);
        for layer in &self.layers {
            let q = linear(&x, &layer.query, threads);
            let k = linear(&x, &layer.key, threads);
            let v = linear(&x, &layer.value, threads);
            let context = attention(&q, &k, &v);
            let mut h = linear(&context, &layer.attention_out, threads);
            h += &x;
            layer.attention_norm.apply(&mut h);
            let mut inter = linear(&h, &layer.intermediate, threads);
            inter.mapv_inplace(gelu);
            let mut out = linear(&inter, &layer.output, threads);
            out += &h;
            layer.output_norm.apply(&mut out);
            x = out;
        }
        let mean = x
            .mean_axis(Axis(0))
            .ok_or_else(|| "empty encoder output".to_owned())?;
        let norm = mean.iter().map(|v| v * v).sum::<f32>().sqrt();
        if !norm.is_finite() || norm == 0.0 {
            return Err("degenerate embedding".into());
        }
        Ok(mean.iter().map(|v| v / norm).collect())
    }
}
