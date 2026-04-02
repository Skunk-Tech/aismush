//! Local embedding engine using MiniLM-L6-v2 via ONNX Runtime.
//!
//! Embeds text into 384-dimensional vectors for semantic search.
//! Runs entirely locally — no cloud, no API cost, ~10ms per embedding.

use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;
use std::sync::Mutex as StdMutex;
use tracing::info;

const EMBEDDING_DIM: usize = 384;

pub struct EmbeddingEngine {
    session: StdMutex<Session>,
    tokenizer: tokenizers::Tokenizer,
}

// Safe because Session is only accessed through Mutex
unsafe impl Send for EmbeddingEngine {}
unsafe impl Sync for EmbeddingEngine {}

impl EmbeddingEngine {
    /// Load model + tokenizer. Downloads from HuggingFace on first use.
    pub async fn new(data_dir: &Path) -> Result<Self, String> {
        let models_dir = data_dir.join("models");
        std::fs::create_dir_all(&models_dir).ok();
        ensure_model_files(&models_dir).await?;

        let mp = models_dir.join("model.onnx").to_string_lossy().to_string();
        let tp = models_dir.join("tokenizer.json");

        let result = tokio::task::spawn_blocking(move || {
            let session = Session::builder()
                .map_err(|e| format!("Session: {}", e))?
                .commit_from_file(&mp)
                .map_err(|e| format!("Model: {}", e))?;

            let tokenizer = tokenizers::Tokenizer::from_file(&tp)
                .map_err(|e| format!("Tokenizer: {}", e))?;

            Ok::<_, String>((session, tokenizer))
        }).await.map_err(|e| format!("Spawn: {}", e))??;

        info!("Embedding engine ready (MiniLM-L6-v2, {}D)", EMBEDDING_DIM);
        Ok(Self { session: StdMutex::new(result.0), tokenizer: result.1 })
    }

    /// Embed text → 384-dim L2-normalized vector.
    pub fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        let truncated = safe_truncate(text, 1000);

        let encoding = self.tokenizer.encode(truncated, true)
            .map_err(|e| format!("Tokenize: {}", e))?;

        let input_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
        let attention_mask: Vec<i64> = encoding.get_attention_mask().iter().map(|&m| m as i64).collect();
        let token_type_ids: Vec<i64> = encoding.get_type_ids().iter().map(|&t| t as i64).collect();
        let seq_len = input_ids.len();

        if seq_len == 0 { return Ok(vec![0.0; EMBEDDING_DIM]); }

        // Create tensors: (shape_as_vec, data_as_boxed_slice)
        let ids_tensor = Tensor::from_array(
            (vec![1i64, seq_len as i64], input_ids.into_boxed_slice())
        ).map_err(|e| format!("Tensor ids: {}", e))?;

        let mask_tensor = Tensor::from_array(
            (vec![1i64, seq_len as i64], attention_mask.clone().into_boxed_slice())
        ).map_err(|e| format!("Tensor mask: {}", e))?;

        let type_tensor = Tensor::from_array(
            (vec![1i64, seq_len as i64], token_type_ids.into_boxed_slice())
        ).map_err(|e| format!("Tensor types: {}", e))?;

        // Run inference through mutex
        let mut session = self.session.lock().map_err(|e| format!("Lock: {}", e))?;
        let outputs = session.run(ort::inputs![
            "input_ids" => ids_tensor,
            "attention_mask" => mask_tensor,
            "token_type_ids" => type_tensor,
        ]).map_err(|e| format!("Inference: {}", e))?;

        // Extract output: flat slice with shape [1, seq_len, hidden_dim]
        let (shape, data) = outputs[0].try_extract_tensor::<f32>()
            .map_err(|e| format!("Extract: {}", e))?;
        let dims: Vec<usize> = shape.iter().map(|&d| d as usize).collect();
        let hidden_dim = if dims.len() == 3 { dims[2] } else { EMBEDDING_DIM };

        // Mean pooling over non-padding tokens
        let mut embedding = vec![0.0f32; hidden_dim];
        let mut count = 0.0f32;
        for i in 0..seq_len {
            if attention_mask[i] == 1 {
                let offset = i * hidden_dim;
                for j in 0..hidden_dim {
                    if offset + j < data.len() {
                        embedding[j] += data[offset + j];
                    }
                }
                count += 1.0;
            }
        }
        if count > 0.0 { for val in &mut embedding { *val /= count; } }

        // L2 normalize
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 { for val in &mut embedding { *val /= norm; } }

        Ok(embedding)
    }

    pub fn similarity(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
    }

    pub fn search_similar(query_vec: &[f32], candidates: &[(i64, Vec<u8>)], top_k: usize) -> Vec<(i64, f32)> {
        let mut scored: Vec<(i64, f32)> = candidates.iter()
            .map(|(id, blob)| (*id, Self::similarity(query_vec, &blob_to_vec(blob))))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);
        scored
    }
}

pub fn vec_to_blob(vec: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vec.len() * 4);
    for &val in vec { bytes.extend_from_slice(&val.to_le_bytes()); }
    bytes
}

pub fn blob_to_vec(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn safe_truncate(s: &str, max: usize) -> &str {
    if s.len() <= max { return s; }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) { end -= 1; }
    &s[..end]
}

async fn ensure_model_files(models_dir: &Path) -> Result<(), String> {
    let model_path = models_dir.join("model.onnx");
    let tokenizer_path = models_dir.join("tokenizer.json");
    if model_path.exists() && tokenizer_path.exists() { return Ok(()); }

    eprintln!("  Downloading embedding model (first time only)...");
    if !model_path.exists() {
        eprint!("  ├─ model.onnx (23MB)...");
        download_file("https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/onnx/model.onnx", &model_path).await?;
        eprintln!(" done");
    }
    if !tokenizer_path.exists() {
        eprint!("  └─ tokenizer.json...");
        download_file("https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/tokenizer.json", &tokenizer_path).await?;
        eprintln!(" done");
    }
    eprintln!("  Model ready.");
    Ok(())
}

async fn download_file(url: &str, dest: &Path) -> Result<(), String> {
    let output = tokio::process::Command::new("curl")
        .args(["-sfL", "--max-time", "120", "-o"])
        .arg(dest.to_str().unwrap_or(""))
        .arg(url)
        .output().await
        .map_err(|e| format!("Download: {}", e))?;
    if !output.status.success() {
        let _ = std::fs::remove_file(dest);
        return Err(format!("Download failed: {}", String::from_utf8_lossy(&output.stderr)));
    }
    Ok(())
}
