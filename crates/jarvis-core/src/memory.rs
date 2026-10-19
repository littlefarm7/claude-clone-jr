//! Persistent, user-controlled memory with hybrid (FTS5 + vector) search.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::store::{MemoryRecord, Store};

#[async_trait]
pub trait Embedder: Send + Sync {
    fn id(&self) -> String;
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}

/// Offline fallback: feature-hashed bag of words + character trigrams.
/// Captures lexical overlap and morphology, not true semantics.
pub struct HashEmbedder {
    pub dim: usize,
}

impl Default for HashEmbedder {
    fn default() -> Self {
        Self { dim: 384 }
    }
}

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

impl HashEmbedder {
    pub fn embed_one(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0f32; self.dim];
        let lower = text.to_lowercase();
        for word in lower.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() > 1) {
            let h = fnv(word);
            v[(h % self.dim as u64) as usize] += if h & 1 == 0 { 1.0 } else { -1.0 };
            let chars: Vec<char> = format!(" {word} ").chars().collect();
            for w in chars.windows(3) {
                let g: String = w.iter().collect();
                let h = fnv(&g);
                v[(h % self.dim as u64) as usize] += if h & 1 == 0 { 0.5 } else { -0.5 };
            }
        }
        normalize(&mut v);
        v
    }
}

#[async_trait]
impl Embedder for HashEmbedder {
    fn id(&self) -> String {
        format!("hash-{}", self.dim)
    }
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| self.embed_one(t)).collect())
    }
}

fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

pub struct Memory {
    pub store: Arc<Store>,
    pub embedder: Arc<dyn Embedder>,
}

#[derive(Debug, Clone)]
pub struct MemoryHit {
    pub record: MemoryRecord,
    pub score: f32,
}

impl Memory {
    pub fn new(store: Arc<Store>, embedder: Arc<dyn Embedder>) -> Self {
        Self { store, embedder }
    }

    pub async fn save(&self, kind: &str, content: &str, tags: &str, source: &str) -> Result<i64> {
        let emb = self.embedder.embed(&[format!("{content} {tags}")]).await.ok().and_then(|mut v| v.pop());
        self.store.add_memory(kind, content, tags, source, emb.as_deref())
    }

    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<MemoryHit>> {
        let all = self.store.list_memories()?;
        if all.is_empty() {
            return Ok(vec![]);
        }
        let q = self.embedder.embed(&[query.to_string()]).await.ok().and_then(|mut v| v.pop());
        let fts = self.store.search_memories_fts(query, 50).unwrap_or_default();
        let mut hits: Vec<MemoryHit> = all
            .into_iter()
            .map(|r| {
                let vec_score = match (&q, &r.embedding) {
                    (Some(q), Some(e)) => cosine(q, e).max(0.0),
                    _ => 0.0,
                };
                let lex = fts.iter().position(|(m, _)| m.id == r.id).map(|rank| 1.0 / (1.0 + rank as f32)).unwrap_or(0.0);
                MemoryHit { score: 0.6 * vec_score + 0.4 * lex, record: r }
            })
            .filter(|h| h.score > 0.12)
            .collect();
        hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
        hits.truncate(limit);
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hybrid_search_ranks_relevant_first() {
        let store = Arc::new(Store::in_memory().unwrap());
        let m = Memory::new(store, Arc::new(HashEmbedder::default()));
        m.save("preference", "Prefiro relatórios em PDF com fonte grande", "documentos", "user").await.unwrap();
        m.save("fact", "Meu computador tem uma RX 5500 XT", "hardware", "user").await.unwrap();
        m.save("fact", "A reunião semanal é às terças", "agenda", "user").await.unwrap();
        let hits = m.search("qual placa de vídeo do computador?", 3).await.unwrap();
        assert!(!hits.is_empty());
        assert!(hits[0].record.content.contains("RX 5500"));
        let hits = m.search("relatório", 3).await.unwrap();
        assert!(hits[0].record.content.contains("PDF"));
    }
}
