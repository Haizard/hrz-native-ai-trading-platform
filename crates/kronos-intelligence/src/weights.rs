//! Minimal safetensors reader for the F32 checkpoints used by Kronos.

use std::collections::BTreeMap;
use std::path::Path;

/// One loaded tensor. Only F32 tensors are used at inference time.
#[derive(Debug, Clone)]
pub struct Tensor {
    /// Tensor dimensions in row-major order.
    pub shape: Vec<usize>,
    /// Flat F32 payload.
    pub data: Vec<f32>,
}

impl Tensor {
    /// Total number of elements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Whether the tensor has no elements.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Read a scalar value, when the tensor has exactly one element.
    #[must_use]
    pub fn scalar(&self) -> Option<f32> {
        (self.data.len() == 1).then_some(self.data[0])
    }
}

/// A checkpoint's named tensors.
#[derive(Debug, Clone)]
pub struct Weights {
    tensors: BTreeMap<String, Tensor>,
}

impl Weights {
    /// Load a safetensors file, rejecting unsupported dtypes.
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        if bytes.len() < 8 {
            return Err("safetensors file is too short".to_string());
        }
        let header_len = u64::from_le_bytes(bytes[0..8].try_into().unwrap()) as usize;
        let header_end = 8usize.saturating_add(header_len);
        if header_end > bytes.len() {
            return Err("safetensors header exceeds file length".to_string());
        }
        let header: BTreeMap<String, serde_json::Value> =
            serde_json::from_slice(&bytes[8..header_end]).map_err(|error| format!("bad safetensors header: {error}"))?;
        let mut tensors = BTreeMap::new();
        for (name, value) in header {
            if name == "__metadata__" {
                continue;
            }
            let dtype = value["dtype"].as_str().ok_or("tensor has no dtype")?;
            if dtype != "F32" && dtype != "I64" {
                return Err(format!("unsupported tensor dtype {dtype} for {name}"));
            }
            let is_i64 = dtype == "I64";
            let offsets = value["data_offsets"].as_array().ok_or("tensor has no offsets")?;
            let start = offsets[0].as_u64().ok_or("bad tensor offset")? as usize;
            let end = offsets[1].as_u64().ok_or("bad tensor offset")? as usize;
            let data_start = header_end.saturating_add(start);
            let data_end = header_end.saturating_add(end);
            if data_end > bytes.len() {
                return Err(format!("tensor {name} exceeds file length"));
            }
            let shape: Vec<usize> = value["shape"]
                .as_array()
                .ok_or("tensor has no shape")?
                .iter()
                .map(|value| value.as_u64().unwrap_or(0) as usize)
                .collect();
            let mut data = Vec::with_capacity(end.saturating_sub(start) / 4);
            if is_i64 {
                for chunk in bytes[data_start..data_end].chunks_exact(8) {
                    data.push(i64::from_le_bytes(chunk.try_into().unwrap()) as f32);
                }
            } else {
                for chunk in bytes[data_start..data_end].chunks_exact(4) {
                    data.push(f32::from_le_bytes(chunk.try_into().unwrap()));
                }
            }
            tensors.insert(name, Tensor { shape, data });
        }
        Ok(Self { tensors })
    }

    /// Look up a tensor by its checkpoint name.
    pub fn get(&self, name: &str) -> Result<&Tensor, String> {
        self.tensors
            .get(name)
            .ok_or_else(|| format!("missing tensor `{name}`"))
    }

    /// Number of loaded tensors.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tensors.len()
    }

    /// Whether the checkpoint is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tensors.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_reported() {
        assert!(Weights::load(Path::new("/no/such/checkpoint.safetensors")).is_err());
    }
}
