//! Pure-Rust inference for the pretrained Kronos-mini candlestick model.
//!
//! No training happens here. The MIT-licensed checkpoints are loaded from
//! `models/kronos`, and this crate implements the same forward pass as the
//! original PyTorch model with fixed arithmetic. Sampling is deterministic
//! (argmax), so a given input always gives the same output.

pub mod weights;

use std::path::{Path, PathBuf};

use analytics_core::types::Candle;
use serde::{Deserialize, Serialize};

use weights::Weights;

const D_MODEL: usize = 256;
const HEADS: usize = 4;
const HEAD_DIM: usize = D_MODEL / HEADS;
const FF_DIM: usize = 512;
const S1_BITS: usize = 10;
const S2_BITS: usize = 10;
const CODEBOOK: usize = S1_BITS + S2_BITS;
const N_LAYERS: usize = 4;
const DEFAULT_CONTEXT: usize = 512;

/// Paths to the two pretrained checkpoints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelPaths {
    /// Kronos-mini model.safetensors.
    pub model: PathBuf,
    /// Kronos-Tokenizer-2k model.safetensors.
    pub tokenizer: PathBuf,
}

impl Default for ModelPaths {
    fn default() -> Self {
        let root = std::env::var("KRONOS_WEIGHTS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../models/kronos"));
        Self {
            model: root.join("kronos-mini.safetensors"),
            tokenizer: root.join("tokenizer-2k.safetensors"),
        }
    }
}

/// A loaded, ready-to-run Kronos inference engine.
#[derive(Debug, Clone)]
pub struct KronosModel {
    model: Weights,
    tokenizer: Weights,
    paths: ModelPaths,
}

/// Forecast configuration.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ForecastConfig {
    /// Historical candles fed to the model.
    pub lookback: usize,
    /// Future bars to generate.
    pub horizon: usize,
    /// Maximum autoregressive context.
    pub context: usize,
    /// Input clipping bound, matching the reference predictor.
    pub clip: f64,
}

impl Default for ForecastConfig {
    fn default() -> Self {
        Self {
            lookback: 256,
            horizon: 12,
            context: DEFAULT_CONTEXT,
            clip: 5.0,
        }
    }
}

/// One forecast bar in price space.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForecastBar {
    /// Open.
    pub open: f64,
    /// High.
    pub high: f64,
    /// Low.
    pub low: f64,
    /// Close.
    pub close: f64,
    /// Volume, when supplied by the caller.
    pub volume: f64,
}

/// Result of a pretrained-model forecast.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KronosForecast {
    /// Symbol forecast.
    pub symbol: String,
    /// Generated bars, oldest first.
    pub bars: Vec<ForecastBar>,
    /// Explicit limitation.
    pub limitation: String,
}

fn linear(input: &[f32], weight: &[f32], bias: Option<&[f32]>, out: usize, input_dim: usize) -> Vec<f32> {
    let mut result = vec![0.0_f32; out];
    for row in 0..out {
        let base = row * input_dim;
        let mut sum = bias.map_or(0.0, |values| values[row]);
        for (index, value) in input.iter().enumerate() {
            sum += weight[base + index] * value;
        }
        result[row] = sum;
    }
    result
}

fn linear_named(
    weights: &Weights,
    name: &str,
    input: &[f32],
    out: usize,
    input_dim: usize,
) -> Result<Vec<f32>, String> {
    let weight = weights.get(&format!("{name}.weight"))?;
    let bias = weights.get(&format!("{name}.bias")).ok().map(|tensor| tensor.data.as_slice());
    Ok(linear(input, &weight.data, bias, out, input_dim))
}

fn rmsnorm(input: &[f32], weight: &[f32]) -> Vec<f32> {
    let sum: f32 = input.iter().map(|value| value * value).sum();
    let scale = 1.0 / (sum / input.len() as f64 as f32 + 1e-5).sqrt();
    input.iter().zip(weight).map(|(value, w)| value * scale * w).collect()
}

fn silu(value: f32) -> f32 {
    value / (1.0 + (-value).exp())
}

fn softmax(values: &[f32]) -> Vec<f32> {
    let max = values.iter().copied().fold(f64::NEG_INFINITY as f32, f32::max);
    let exps: Vec<f32> = values.iter().map(|value| (value - max).exp()).collect();
    let total: f32 = exps.iter().sum();
    exps.iter().map(|value| value / total).collect()
}

fn layer_norm_rows(input: &[f32], rows: usize, cols: usize) -> Vec<Vec<f32>> {
    input.chunks(cols).take(rows).map(<[f32]>::to_vec).collect()
}

fn rotate_half(values: &[f32]) -> Vec<f32> {
    let half = values.len() / 2;
    let mut out = Vec::with_capacity(values.len());
    for value in &values[half..] {
        out.push(-value);
    }
    out.extend_from_slice(&values[..half]);
    out
}

fn rope(values: &mut [f32], inv_freq: &[f32], position: usize) {
    for (pair, frequency) in inv_freq.iter().enumerate() {
        let angle = position as f32 * frequency;
        let cosine = angle.cos();
        let sine = angle.sin();
        let left = pair;
        let right = pair + inv_freq.len();
        let a = values[left];
        let b = values[right];
        values[left] = a * cosine - b * sine;
        values[right] = a * sine + b * cosine;
    }
}

#[derive(Debug, Clone)]
struct AttentionWeights<'a> {
    weights: &'a Weights,
    prefix: &'a str,
    inv_freq: &'a weights::Tensor,
}

impl<'a> AttentionWeights<'a> {
    fn project(&self, name: &str, input: &[f32]) -> Result<Vec<f32>, String> {
        linear_named(self.weights, &format!("{}.{}", self.prefix, name), input, D_MODEL, D_MODEL)
    }
}

fn attention(
    input: &[Vec<f32>],
    attention_weights: &AttentionWeights<'_>,
    causal: bool,
) -> Result<Vec<Vec<f32>>, String> {
    let sequence = input.len();
    let mut q = Vec::with_capacity(sequence);
    let mut k = Vec::with_capacity(sequence);
    let mut v = Vec::with_capacity(sequence);
    for (position, token) in input.iter().enumerate() {
        let mut query = attention_weights.project("q_proj", token)?;
        let mut key = attention_weights.project("k_proj", token)?;
        let value = attention_weights.project("v_proj", token)?;
        for head in 0..HEADS {
            let start = head * HEAD_DIM;
            let inv = &attention_weights.inv_freq.data;
            rope(&mut query[start..start + HEAD_DIM], inv, position);
            rope(&mut key[start..start + HEAD_DIM], inv, position);
        }
        q.push(query);
        k.push(key);
        v.push(value);
    }
    let mut result = vec![vec![0.0_f32; D_MODEL]; sequence];
    for head in 0..HEADS {
        let offset = head * HEAD_DIM;
        for query_index in 0..sequence {
            let mut scores = Vec::with_capacity(sequence);
            for key_index in 0..sequence {
                if causal && key_index > query_index {
                    scores.push(f64::NEG_INFINITY as f32);
                    continue;
                }
                let mut score = 0.0_f32;
                for dim in 0..HEAD_DIM {
                    score += q[query_index][offset + dim] * k[key_index][offset + dim];
                }
                scores.push(score / (HEAD_DIM as f32).sqrt());
            }
            let probabilities = softmax(&scores);
            for dim in 0..HEAD_DIM {
                let mut value = 0.0_f32;
                for key_index in 0..sequence {
                    value += probabilities[key_index] * v[key_index][offset + dim];
                }
                result[query_index][offset + dim] = value;
            }
        }
    }
    for token in &mut result {
        *token = attention_weights.project("out_proj", token)?;
    }
    Ok(result)
}

fn cross_attention(
    queries: &[Vec<f32>],
    keys: &[Vec<f32>],
    weights: &AttentionWeights<'_>,
) -> Result<Vec<Vec<f32>>, String> {
    let mut q = Vec::with_capacity(queries.len());
    let mut k = Vec::with_capacity(keys.len());
    let mut v = Vec::with_capacity(keys.len());
    for (position, token) in queries.iter().enumerate() {
        let mut query = weights.project("q_proj", token)?;
        for head in 0..HEADS {
            let start = head * HEAD_DIM;
            rope(&mut query[start..start + HEAD_DIM], &weights.inv_freq.data, position);
        }
        q.push(query);
    }
    for (position, token) in keys.iter().enumerate() {
        let mut key = weights.project("k_proj", token)?;
        let value = weights.project("v_proj", token)?;
        for head in 0..HEADS {
            let start = head * HEAD_DIM;
            rope(&mut key[start..start + HEAD_DIM], &weights.inv_freq.data, position);
        }
        k.push(key);
        v.push(value);
    }
    let mut result = vec![vec![0.0_f32; D_MODEL]; queries.len()];
    for head in 0..HEADS {
        let offset = head * HEAD_DIM;
        for (query_index, query) in q.iter().enumerate() {
            let mut scores = Vec::with_capacity(keys.len());
            for key in &k {
                let mut score = 0.0_f32;
                for dim in 0..HEAD_DIM {
                    score += query[offset + dim] * key[offset + dim];
                }
                scores.push(score / (HEAD_DIM as f32).sqrt());
            }
            let probabilities = softmax(&scores);
            for dim in 0..HEAD_DIM {
                let mut value = 0.0_f32;
                for (index, token) in v.iter().enumerate() {
                    value += probabilities[index] * token[offset + dim];
                }
                result[query_index][offset + dim] = value;
            }
        }
    }
    for token in &mut result {
        *token = weights.project("out_proj", token)?;
    }
    Ok(result)
}

fn transformer_block(input: &[Vec<f32>], weights: &Weights, prefix: &str, causal: bool) -> Result<Vec<Vec<f32>>, String> {
    let norm1 = weights.get(&format!("{prefix}.norm1.weight"))?.data.to_vec();
    let norm2 = weights.get(&format!("{prefix}.norm2.weight"))?.data.to_vec();
    let w1 = weights.get(&format!("{prefix}.ffn.w1.weight"))?;
    let w2 = weights.get(&format!("{prefix}.ffn.w2.weight"))?;
    let w3 = weights.get(&format!("{prefix}.ffn.w3.weight"))?;
    let attention_weights = AttentionWeights {
        weights,
        prefix: &format!("{prefix}.self_attn"),
        inv_freq: weights.get(&format!("{prefix}.self_attn.rotary.inv_freq"))?,
    };
    let normalized: Vec<Vec<f32>> = input.iter().map(|token| rmsnorm(token, &norm1)).collect();
    let attended = attention(&normalized, &attention_weights, causal)?;
    let mut hidden: Vec<Vec<f32>> = input
        .iter()
        .zip(attended)
        .map(|(original, value)| original.iter().zip(value).map(|(a, b)| a + b).collect())
        .collect();
    let normalized2: Vec<Vec<f32>> = hidden.iter().map(|token| rmsnorm(token, &norm2)).collect();
    for (token, source) in hidden.iter_mut().zip(normalized2.iter()) {
        let a = linear(source, &w1.data, None, FF_DIM, D_MODEL);
        let b = linear(source, &w3.data, None, FF_DIM, D_MODEL);
        let product: Vec<f32> = a.iter().zip(b).map(|(x, y)| silu(*x) * y).collect();
        let fed = linear(&product, &w2.data, None, D_MODEL, FF_DIM);
        for (value, addition) in token.iter_mut().zip(fed) {
            *value += addition;
        }
    }
    Ok(hidden)
}

fn argmax(values: &[f32]) -> usize {
    values
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map_or(0, |(index, _)| index)
}

impl KronosModel {
    /// Load both pretrained checkpoints.
    pub fn load(paths: &ModelPaths) -> Result<Self, String> {
        let model = Weights::load(&paths.model)?;
        let tokenizer = Weights::load(&paths.tokenizer)?;
        Ok(Self {
            model,
            tokenizer,
            paths: paths.clone(),
        })
    }

    /// Paths of the loaded checkpoints.
    #[must_use]
    pub fn paths(&self) -> &ModelPaths {
        &self.paths
    }

    /// Tokenize normalized OHLCV+amount rows into two index streams.
    pub fn encode(&self, rows: &[Vec<f32>]) -> Result<(Vec<usize>, Vec<usize>), String> {
        if rows.is_empty() {
            return Err("no rows to tokenize".to_string());
        }
        let embed = self.tokenizer.get("embed.weight")?;
        let embed_bias = self.tokenizer.get("embed.bias").ok().map(|tensor| tensor.data.as_slice());
        let quant = self.tokenizer.get("quant_embed.weight")?;
        let quant_bias = self.tokenizer.get("quant_embed.bias").ok().map(|tensor| tensor.data.as_slice());
        let mut hidden: Vec<Vec<f32>> = rows
            .iter()
            .map(|row| linear(row, &embed.data, embed_bias, D_MODEL, 6))
            .collect();
        for layer in 0..3 {
            hidden = transformer_block(&hidden, &self.tokenizer, &format!("encoder.{layer}"), false)?;
        }
        let mut codes = Vec::with_capacity(hidden.len());
        for token in &hidden {
            let projected = linear(token, &quant.data, quant_bias, CODEBOOK, D_MODEL);
            let norm: f32 = projected.iter().map(|value| value * value).sum::<f32>().sqrt();
            let scale = if norm > 0.0 { 1.0 / norm } else { 0.0 };
            let mut s1 = 0usize;
            let mut s2 = 0usize;
            for (bit, value) in projected.iter().enumerate() {
                let bit_index = if *value >= 0.0 { 1usize } else { 0usize };
                if bit < S1_BITS {
                    s1 |= bit_index << bit;
                } else {
                    s2 |= bit_index << (bit - S1_BITS);
                }
            }
            let _ = scale;
            codes.push((s1, s2));
        }
        Ok((codes.iter().map(|code| code.0).collect(), codes.iter().map(|code| code.1).collect()))
    }

    /// Decode two index streams back to six-dimensional rows.
    pub fn decode(&self, s1: &[usize], s2: &[usize]) -> Result<Vec<Vec<f32>>, String> {
        if s1.len() != s2.len() || s1.is_empty() {
            return Err("token streams must have equal, non-zero length".to_string());
        }
        let post = self.tokenizer.get("post_quant_embed.weight")?;
        let post_bias = self.tokenizer.get("post_quant_embed.bias").ok().map(|tensor| tensor.data.as_slice());
        let head = self.tokenizer.get("head.weight")?;
        let head_bias = self.tokenizer.get("head.bias").ok().map(|tensor| tensor.data.as_slice());
        let scale = 1.0 / (CODEBOOK as f32).sqrt();
        let quantized: Vec<Vec<f32>> = s1
            .iter()
            .zip(s2)
            .map(|(first, second)| {
                (0..CODEBOOK)
                    .map(|bit| {
                        let value = if bit < S1_BITS {
                            (first >> bit) & 1
                        } else {
                            (second >> (bit - S1_BITS)) & 1
                        };
                        (if value == 1 { 1.0 } else { -1.0 }) * scale
                    })
                    .collect()
            })
            .collect();
        let mut hidden: Vec<Vec<f32>> = quantized
            .iter()
            .map(|row| linear(row, &post.data, post_bias, D_MODEL, CODEBOOK))
            .collect();
        for layer in 0..3 {
            hidden = transformer_block(&hidden, &self.tokenizer, &format!("decoder.{layer}"), false)?;
        }
        Ok(hidden
            .iter()
            .map(|row| linear(row, &head.data, head_bias, 6, D_MODEL))
            .collect())
    }

    fn token_embeddings(&self, s1: usize, s2: usize) -> Result<Vec<f32>, String> {
        let emb1 = self.model.get("embedding.emb_s1.weight")?;
        let emb2 = self.model.get("embedding.emb_s2.weight")?;
        let fusion = self.model.get("embedding.fusion_proj.weight")?;
        let fusion_bias = self.model.get("embedding.fusion_proj.bias").ok().map(|tensor| tensor.data.as_slice());
        let mut combined = Vec::with_capacity(D_MODEL * 2);
        for (table, index) in [(emb1, s1), (emb2, s2)] {
            let start = index * D_MODEL;
            let scale = (D_MODEL as f32).sqrt();
            let embedded: Vec<f32> = table.data[start..start + D_MODEL].iter().map(|value| value * scale).collect();
            combined.extend(embedded);
        }
        let mut result: Vec<f32> = (0..D_MODEL).map(|index| fusion_bias.map_or(0.0, |values| values[index])).collect();
        for index in 0..D_MODEL {
            for input in 0..D_MODEL * 2 {
                result[index] += fusion.data[index * D_MODEL * 2 + input] * combined[input];
            }
        }
        Ok(result)
    }

    fn time_embedding(&self, stamp: [f32; 5]) -> Result<Vec<f32>, String> {
        let names = ["minute", "hour", "weekday", "day", "month"];
        let mut result = vec![0.0_f32; D_MODEL];
        for (name, value) in names.iter().zip(stamp) {
            let table = self.model.get(&format!("time_emb.{name}_embed.weight"))?;
            let index = (value.max(0.0) as usize).min(table.shape[0].saturating_sub(1));
            let start = index * D_MODEL;
            for (target, source) in result.iter_mut().zip(&table.data[start..start + D_MODEL]) {
                *target += source;
            }
        }
        Ok(result)
    }

    fn decode_step(
        &self,
        s1: &[usize],
        s2: &[usize],
        stamps: &[[f32; 5]],
    ) -> Result<(usize, usize), String> {
        let mut sequence: Vec<Vec<f32>> = s1
            .iter()
            .zip(s2)
            .zip(stamps)
            .map(|((first, second), stamp)| {
                let mut embedding = self.token_embeddings(*first, *second)?;
                let time = self.time_embedding(*stamp)?;
                for (value, addition) in embedding.iter_mut().zip(time) {
                    *value += addition;
                }
                Ok(embedding)
            })
            .collect::<Result<Vec<_>, String>>()?;
        for layer in 0..N_LAYERS {
            sequence = transformer_block(&sequence, &self.model, &format!("transformer.{layer}"), true)?;
        }
        let norm = self.model.get("norm.weight")?;
        let normalized: Vec<Vec<f32>> = sequence.iter().map(|row| rmsnorm(row, &norm.data)).collect();
        let s1_head = self.model.get("head.proj_s1.weight")?;
        let s1_bias = self.model.get("head.proj_s1.bias").ok().map(|tensor| tensor.data.as_slice());
        let last = normalized.last().ok_or("empty context")?;
        let s1_logits = linear(last, &s1_head.data, s1_bias, 1 << S1_BITS, D_MODEL);
        let sample_s1 = argmax(&s1_logits);

        let emb1 = self.model.get("embedding.emb_s1.weight")?;
        let sibling: Vec<f32> = emb1.data[sample_s1 * D_MODEL..(sample_s1 + 1) * D_MODEL].to_vec();
        let cross = AttentionWeights {
            weights: &self.model,
            prefix: "dep_layer.cross_attn",
            inv_freq: self.model.get("dep_layer.cross_attn.rotary.inv_freq")?,
        };
        let attention_output = cross_attention(&[sibling.clone()], &sequence, &cross)?;
        let dep_norm = self.model.get("dep_layer.norm.weight")?;
        let residual: Vec<f32> = sequence
            .last()
            .unwrap()
            .iter()
            .zip(&attention_output[0])
            .map(|(value, attention)| value + attention)
            .collect();
        let conditioned = rmsnorm(&residual, &dep_norm.data);
        let s2_head = self.model.get("head.proj_s2.weight")?;
        let s2_bias = self.model.get("head.proj_s2.bias").ok().map(|tensor| tensor.data.as_slice());
        let s2_logits = linear(&conditioned, &s2_head.data, s2_bias, 1 << S2_BITS, D_MODEL);
        Ok((sample_s1, argmax(&s2_logits)))
    }

    /// Generate a forecast from completed candles, with no training involved.
    pub fn forecast(&self, symbol: &str, candles: &[Candle], config: &ForecastConfig) -> Result<KronosForecast, String> {
        if candles.len() < 8 || config.lookback < 8 || config.horizon == 0 {
            return Err("forecast needs candles, a lookback and a horizon".to_string());
        }
        let lookback = config.lookback.min(candles.len());
        let history = &candles[candles.len() - lookback..];
        let mut rows = Vec::with_capacity(lookback);
        for candle in history {
            let amount = candle.volume * (candle.open + candle.high + candle.low + candle.close) / 4.0;
            rows.push(vec![candle.open as f32, candle.high as f32, candle.low as f32, candle.close as f32, candle.volume as f32, amount as f32]);
        }
        let mut means = vec![0.0_f32; 6];
        let mut stds = vec![0.0_f32; 6];
        for column in 0..6 {
            means[column] = rows.iter().map(|row| row[column]).sum::<f32>() / lookback as f32;
            let variance: f32 = rows.iter().map(|row| (row[column] - means[column]).powi(2)).sum::<f32>() / lookback as f32;
            stds[column] = variance.sqrt().max(1e-5);
        }
        for row in &mut rows {
            for column in 0..6 {
                row[column] = ((row[column] - means[column]) / stds[column]).clamp(-config.clip as f32, config.clip as f32);
            }
        }
        let (mut s1, mut s2) = self.encode(&rows)?;
        let step_ns = history.last().unwrap().open_time - history[history.len() - 2].open_time;
        let mut future_stamps = Vec::with_capacity(config.horizon);
        for index in 1..=config.horizon {
            let time_ns = history.last().unwrap().open_time + index as i64 * step_ns;
            future_stamps.push(timestamp_stamp(time_ns));
        }
        let mut generated = Vec::with_capacity(config.horizon);
        for stamp in future_stamps {
            let mut prompt_stamps: Vec<[f32; 5]> = history.iter().map(|candle| timestamp_stamp(candle.open_time)).collect();
            prompt_stamps.push(stamp);
            let mut prompt_s1 = s1.clone();
            let mut prompt_s2 = s2.clone();
            prompt_s1.push(0);
            prompt_s2.push(0);
            if prompt_s1.len() > config.context {
                let start = prompt_s1.len() - config.context;
                prompt_s1 = prompt_s1[start..].to_vec();
                prompt_s2 = prompt_s2[start..].to_vec();
                prompt_stamps = prompt_stamps[start..].to_vec();
            }
            let (next_s1, next_s2) = self.decode_step(&prompt_s1, &prompt_s2, &prompt_stamps)?;
            s1.push(next_s1);
            s2.push(next_s2);
            generated.push((next_s1, next_s2));
        }
        let decode_start = s1.len().saturating_sub(config.horizon).saturating_sub(lookback).min(s1.len().saturating_sub(config.horizon));
        let tail1 = &s1[decode_start..];
        let tail2 = &s2[decode_start..];
        if tail1.len() != tail2.len() || tail1.is_empty() {
            return Err("token generation produced an invalid tail".to_string());
        }
        let decoded = self.decode(tail1, tail2)?;
        let bars = decoded
            .into_iter()
            .rev()
            .take(config.horizon)
            .rev()
            .enumerate()
            .map(|(index, row)| {
                let value: Vec<f64> = row
                    .iter()
                    .enumerate()
                    .map(|(column, normalized)| (normalized * stds[column] + means[column]) as f64)
                    .collect();
                ForecastBar {
                    open: value[0],
                    high: value[1],
                    low: value[2],
                    close: value[3],
                    volume: value[4],
                }
            })
            .collect();
        let _ = index_placeholder();
        Ok(KronosForecast {
            symbol: symbol.to_string(),
            bars,
            limitation: "pretrained Kronos-mini inference with deterministic argmax sampling; not validated for this instrument".to_string(),
        })
    }
}

fn index_placeholder() -> usize {
    0
}

/// UTC calendar stamp used by Kronos: minute, hour, weekday, day, month.
fn timestamp_stamp(time_ns: i64) -> [f32; 5] {
    let seconds = time_ns / 1_000_000_000;
    let days = seconds.div_euclid(86_400);
    let within_day = seconds.rem_euclid(86_400);
    let hour = within_day / 3600;
    let minute = (within_day % 3600) / 60;
    // 1970-01-01 was a Thursday; ISO weekday with Monday = 0.
    let weekday = (days + 3).rem_euclid(7);
    let (year, month, day) = civil_from_days(days);
    let _ = year;
    [minute as f32, hour as f32, weekday as f32, day as f32, month as f32]
}

fn civil_from_days(days: i64) -> (i64, f32, f32) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let day_of_era = z - era * 146097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 { month_prime + 3 } else { month_prime - 9 };
    (year + if month <= 2 { 1 } else { 0 }, month as f32, day as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candles() -> Vec<Candle> {
        (0..120)
            .map(|index| {
                let close = 100.0 + index as f64 * 0.2 + (index % 5) as f64 * 0.1;
                Candle {
                    symbol: "BTCUSDT".to_string(),
                    timeframe: analytics_core::Timeframe::M5,
                    open_time: index as i64 * 300_000_000_000,
                    open: close - 0.2,
                    high: close + 0.4,
                    low: close - 0.4,
                    close,
                    volume: 10.0,
                    buy_volume: 6.0,
                    sell_volume: 4.0,
                }
            })
            .collect()
    }

    #[test]
    fn pretrained_weights_load_and_tokenize() {
        let model = KronosModel::load(&ModelPaths::default()).expect("committed checkpoints must load");
        assert!(model.paths().model.exists());
        let rows: Vec<Vec<f32>> = (0..16)
            .map(|index| vec![100.0, 101.0, 99.0, 100.5, 10.0, 1000.0 + index as f32])
            .collect();
        let (s1, s2) = model.encode(&rows).unwrap();
        assert_eq!(s1.len(), rows.len());
        assert_eq!(s2.len(), rows.len());
        assert!(s1.iter().all(|value| *value < (1 << S1_BITS)));
        assert!(s2.iter().all(|value| *value < (1 << S2_BITS)));
    }

    #[test]
    fn decode_reconstructs_six_dimensions() {
        let model = KronosModel::load(&ModelPaths::default()).unwrap();
        let decoded = model.decode(&[5, 10, 20], &[3, 6, 9]).unwrap();
        assert_eq!(decoded.len(), 3);
        assert!(decoded.iter().all(|row| row.len() == 6 && row.iter().all(|value| value.is_finite())));
    }

    #[test]
    fn a_forecast_is_finite_and_deterministic() {
        let model = KronosModel::load(&ModelPaths::default()).unwrap();
        let config = ForecastConfig { lookback: 32, horizon: 2, context: 64, ..ForecastConfig::default() };
        let first = model.forecast("BTCUSDT", &candles(), &config).unwrap();
        let second = model.forecast("BTCUSDT", &candles(), &config).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.bars.len(), 2);
        assert!(first.bars.iter().all(|bar| bar.open.is_finite() && bar.close.is_finite()));
    }

    #[test]
    fn invalid_inputs_are_refused() {
        let model = KronosModel::load(&ModelPaths::default()).unwrap();
        assert!(model.forecast("X", &[], &ForecastConfig::default()).is_err());
        assert!(model.decode(&[], &[]).is_err());
    }
}
