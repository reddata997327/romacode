//! OPSEC layer. Every Net tool passes through here. Nothing bypasses.

use std::sync::Mutex;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum OpsecError { #[error("all proxies burned")] PoolEmpty }

pub struct Opsec {
    proxies: Mutex<Vec<String>>,
    burns: Mutex<Vec<String>>,
    idx: Mutex<usize>,
    rotate_every: usize,
    jitter: (u64, u64),
    seen: Mutex<usize>,
}

impl Opsec {
    pub fn new(proxies: Vec<String>, rotate_every: usize, jitter: (u64, u64)) -> Self {
        Self {
            proxies: Mutex::new(proxies), burns: Mutex::new(Vec::new()),
            idx: Mutex::new(0), rotate_every, jitter, seen: Mutex::new(0),
        }
    }

    /// Sleep a jittered interval. Call before every request.
    pub async fn jitter(&self) {
        let (lo, hi) = self.jitter;
        if hi > lo {
            let ms = lo + (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap()
                .subsec_nanos() as u64 % (hi - lo));
            tokio::time::sleep(Duration::from_millis(ms)).await;
        }
    }

    /// Next proxy, honouring rotation and burn list.
    pub fn next_proxy(&self) -> Result<Option<String>, OpsecError> {
        let mut seen = self.seen.lock().unwrap();
        *seen += 1;
        let pool = self.proxies.lock().unwrap();
        if pool.is_empty() { return Ok(None); }
        let burns = self.burns.lock().unwrap();
        let live: Vec<&String> = pool.iter().filter(|p| !burns.contains(p)).collect();
        if live.is_empty() { return Err(OpsecError::PoolEmpty); }
        if *seen % self.rotate_every == 0 {
            let mut i = self.idx.lock().unwrap();
            *i = (*i + 1) % live.len();
        }
        Ok(Some(live[*self.idx.lock().unwrap() % live.len()].clone()))
    }

    pub fn burn(&self, proxy: &str) {
        let mut b = self.burns.lock().unwrap();
        if !b.iter().any(|x| x == proxy) { b.push(proxy.to_string()); }
    }

    pub fn burn_host(&self, host: &str) { self.burn(&format!("host:{host}")); }
    pub fn is_burned(&self, host: &str) -> bool {
        self.burns.lock().unwrap().iter().any(|x| x == &format!("host:{host}"))
    }
}
