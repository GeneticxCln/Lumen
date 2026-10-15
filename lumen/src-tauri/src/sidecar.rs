use crate::error::{PlayerError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, OnceCell};
use tokio::time::{timeout, Duration};

#[derive(Debug, Serialize, Deserialize)]
pub struct SidecarReady {
    pub ready: bool,
    pub port: u16,
    pub token: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ExtractRequest {
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractResponse {
    pub title: String,
    pub url: String,
    pub headers: HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HarvestRequest {
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarvestResponse {
    pub urls: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProbeRequest {
    pub urls: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeResponse {
    pub results: Vec<ProbeResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeResult {
    pub url: String,
    pub live: bool,
    pub status: Option<u16>,
    pub content_type: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    pub library: Vec<serde_json::Value>,
    #[serde(default)]
    pub providers: Vec<serde_json::Value>,
    pub check_availability: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub query: String,
    pub results: Vec<serde_json::Value>,
    pub providers: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
}

static SIDECAR: OnceCell<Arc<Mutex<SidecarManager>>> = OnceCell::const_new();

pub struct SidecarManager {
    binaries_dir: PathBuf,
    process: Option<Child>,
    base_url: String,
    token: String,
    client: reqwest::Client,
}

fn locate_sidecar_executable(preferred_dir: &Path) -> Option<PathBuf> {
    let mut candidates = vec![
        preferred_dir.join("extractor"),
        preferred_dir.join("extractor-x86_64-unknown-linux-gnu"),
        preferred_dir
            .join("extractor-x86_64-unknown-linux-gnu")
            .join("extractor-x86_64-unknown-linux-gnu"),
    ];

    let manifest_bins = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("binaries");
    candidates.push(manifest_bins.join("extractor"));
    candidates.push(manifest_bins.join("extractor-x86_64-unknown-linux-gnu"));
    candidates.push(
        manifest_bins
            .join("extractor-x86_64-unknown-linux-gnu")
            .join("extractor-x86_64-unknown-linux-gnu"),
    );

    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("extractor"));
            candidates.push(parent.join("binaries").join("extractor"));
        }
    }

    candidates.into_iter().find(|p| p.exists() && p.is_file())
}

impl SidecarManager {
    pub async fn new(binaries_dir: PathBuf) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| PlayerError::Sidecar(format!("Failed to create HTTP client: {}", e)))?;

        let mut manager = Self {
            binaries_dir,
            process: None,
            base_url: String::new(),
            token: String::new(),
            client,
        };

        manager.spawn_process().await?;
        Ok(manager)
    }

    async fn spawn_process(&mut self) -> Result<()> {
        self.shutdown().await;

        let sidecar_path = locate_sidecar_executable(&self.binaries_dir).ok_or_else(|| {
            PlayerError::Sidecar(format!(
                "Sidecar executable not found in {:?}",
                self.binaries_dir
            ))
        })?;

        let mut cmd = Command::new(&sidecar_path);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        // Put sidecar in its own process group on Unix so we can terminate the group cleanly
        #[cfg(unix)]
        unsafe {
            cmd.pre_exec(|| {
                let _ = nix::unistd::setpgid(
                    nix::unistd::Pid::from_raw(0),
                    nix::unistd::Pid::from_raw(0),
                );
                Ok(())
            });
        }

        let mut process = cmd
            .spawn()
            .map_err(|e| PlayerError::Sidecar(format!("Failed to spawn sidecar: {}", e)))?;

        let stdout = process
            .stdout
            .take()
            .ok_or_else(|| PlayerError::Sidecar("No stdout from sidecar".into()))?;
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();

        let ready_line = timeout(Duration::from_secs(10), async {
            loop {
                line.clear();
                let n = reader
                    .read_line(&mut line)
                    .await
                    .map_err(|e| PlayerError::Sidecar(format!("Failed to read ready line: {}", e)))?;
                if n == 0 {
                    return Err(PlayerError::Sidecar("Sidecar exited before ready".into()));
                }
                let trimmed = line.trim();
                if trimmed.starts_with('{') {
                    return Ok(trimmed.to_string());
                }
            }
        })
        .await
        .map_err(|_| PlayerError::Sidecar("Timeout waiting for sidecar ready".into()))??;

        let ready: SidecarReady = serde_json::from_str(&ready_line)
            .map_err(|e| PlayerError::Sidecar(format!("Invalid ready line: {}", e)))?;

        if !ready.ready {
            return Err(PlayerError::Sidecar("Sidecar reported not ready".into()));
        }

        // Keep draining both pipes for as long as the child lives. A pipe nobody reads holds
        // ~64 KiB: once the sidecar fills it, its next write blocks, and because that write
        // happens on its event loop the whole sidecar stops responding. The ready line is the
        // only thing we must parse from stdout, but the read end still has to stay open —
        // dropping it would turn the sidecar's later writes into a broken pipe.
        tokio::spawn(async move {
            let mut rest = String::new();
            loop {
                rest.clear();
                match reader.read_line(&mut rest).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let trimmed = rest.trim_end();
                        if !trimmed.is_empty() {
                            eprintln!("lumen sidecar: {}", trimmed);
                        }
                    }
                }
            }
        });

        if let Some(stderr) = process.stderr.take() {
            tokio::spawn(async move {
                let mut drain = BufReader::new(stderr);
                let mut line = String::new();
                loop {
                    line.clear();
                    match drain.read_line(&mut line).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            let trimmed = line.trim_end();
                            if !trimmed.is_empty() {
                                eprintln!("lumen sidecar: {}", trimmed);
                            }
                        }
                    }
                }
            });
        }

        self.base_url = format!("http://127.0.0.1:{}", ready.port);
        self.token = ready.token;
        self.process = Some(process);

        self.health_check().await?;
        Ok(())
    }

    async fn ensure_running(&mut self) -> Result<()> {
        let needs_restart = match self.process.as_mut() {
            Some(child) => !matches!(child.try_wait(), Ok(None)),
            None => true,
        };

        if needs_restart || self.health_check().await.is_err() {
            self.spawn_process().await?;
        }
        Ok(())
    }

    pub async fn health_check(&self) -> Result<()> {
        let url = format!("{}/health", self.base_url);
        let response = self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| PlayerError::Sidecar(format!("Health check failed: {}", e)))?;

        if !response.status().is_success() {
            return Err(PlayerError::Sidecar("Health check returned error".into()));
        }

        let health: HealthResponse = response
            .json()
            .await
            .map_err(|e| PlayerError::Sidecar(format!("Invalid health response: {}", e)))?;

        if health.status != "ok" {
            return Err(PlayerError::Sidecar("Sidecar unhealthy".into()));
        }

        Ok(())
    }

    pub async fn extract(&mut self, target_url: &str) -> Result<ExtractResponse> {
        self.ensure_running().await?;
        let endpoint = format!("{}/extract", self.base_url);
        let request = ExtractRequest {
            url: target_url.to_string(),
        };

        let response = self
            .client
            .post(&endpoint)
            .bearer_auth(&self.token)
            .json(&request)
            .send()
            .await
            .map_err(|e| PlayerError::Sidecar(format!("Extract request failed: {}", e)))?;

        if !response.status().is_success() {
            let err = response.text().await.unwrap_or_default();
            return Err(PlayerError::Sidecar(format!("Extract error: {}", err)));
        }

        response
            .json()
            .await
            .map_err(|e| PlayerError::Sidecar(format!("Invalid extract response: {}", e)))
    }

    pub async fn harvest(&mut self, target_url: &str) -> Result<HarvestResponse> {
        self.ensure_running().await?;
        let endpoint = format!("{}/harvest", self.base_url);
        let request = HarvestRequest {
            url: target_url.to_string(),
        };

        let response = self
            .client
            .post(&endpoint)
            .bearer_auth(&self.token)
            .json(&request)
            .send()
            .await
            .map_err(|e| PlayerError::Sidecar(format!("Harvest request failed: {}", e)))?;

        if !response.status().is_success() {
            let err = response.text().await.unwrap_or_default();
            return Err(PlayerError::Sidecar(format!("Harvest error: {}", err)));
        }

        response
            .json()
            .await
            .map_err(|e| PlayerError::Sidecar(format!("Invalid harvest response: {}", e)))
    }

    pub async fn probe(&mut self, urls: Vec<String>) -> Result<ProbeResponse> {
        self.ensure_running().await?;
        let endpoint = format!("{}/probe", self.base_url);
        let request = ProbeRequest { urls };

        let response = self
            .client
            .post(&endpoint)
            .bearer_auth(&self.token)
            .json(&request)
            .send()
            .await
            .map_err(|e| PlayerError::Sidecar(format!("Probe request failed: {}", e)))?;

        if !response.status().is_success() {
            let err = response.text().await.unwrap_or_default();
            return Err(PlayerError::Sidecar(format!("Probe error: {}", err)));
        }

        response
            .json()
            .await
            .map_err(|e| PlayerError::Sidecar(format!("Invalid probe response: {}", e)))
    }

    pub async fn search(&mut self, request: &SearchRequest) -> Result<SearchResponse> {
        self.ensure_running().await?;
        let endpoint = format!("{}/search", self.base_url);
        let response = self
            .client
            .post(&endpoint)
            .bearer_auth(&self.token)
            .json(request)
            .send()
            .await
            .map_err(|error| PlayerError::Sidecar(format!("Search request failed: {error}")))?;

        if !response.status().is_success() {
            let err = response.text().await.unwrap_or_default();
            return Err(PlayerError::Sidecar(format!("Search error: {err}")));
        }

        response
            .json()
            .await
            .map_err(|error| PlayerError::Sidecar(format!("Invalid search response: {error}")))
    }

    pub async fn shutdown(&mut self) {
        if let Some(mut process) = self.process.take() {
            let pid = process.id();
            if let Some(mut stdin) = process.stdin.take() {
                let _ = stdin.write_all(b"SHUTDOWN\n").await;
                let _ = stdin.flush().await;
            }

            match timeout(Duration::from_secs(2), process.wait()).await {
                Ok(Ok(_)) => {}
                _ => {
                    #[cfg(unix)]
                    if let Some(raw_pid) = pid {
                        let _ = nix::sys::signal::killpg(
                            nix::unistd::Pid::from_raw(raw_pid as i32),
                            nix::sys::signal::Signal::SIGKILL,
                        );
                    }
                    let _ = process.kill().await;
                    let _ = process.wait().await;
                }
            }
        }
    }
}

pub async fn init_sidecar(binaries_dir: PathBuf) -> Result<()> {
    if SIDECAR.initialized() {
        return Ok(());
    }
    let manager = SidecarManager::new(binaries_dir).await?;
    let _ = SIDECAR.set(Arc::new(Mutex::new(manager)));
    Ok(())
}

pub fn get_sidecar() -> Option<Arc<Mutex<SidecarManager>>> {
    SIDECAR.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_sidecar_lifecycle_and_probe() {
        let binaries_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("binaries");
        let mut manager = SidecarManager::new(binaries_dir)
            .await
            .expect("SidecarManager should start and pass health_check");

        manager
            .health_check()
            .await
            .expect("health_check should succeed");

        let probe_res = manager
            .probe(vec!["invalid-scheme://not-http".to_string()])
            .await
            .expect("probe should return results");
        assert_eq!(probe_res.results.len(), 1);
        assert!(!probe_res.results[0].live);

        manager.shutdown().await;
    }
}
