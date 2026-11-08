//! Browser lifecycle and CDP transport. No provider-specific DOM logic lives here.
use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    process::{Child, Command},
    time::Duration,
};
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub chrome_binary: String,
    pub profile_dir: PathBuf,
    pub cdp_port: u16,
    pub headless: bool,
    #[serde(default = "default_timeout")]
    pub response_timeout_secs: u64,
    #[serde(default)]
    pub ai_mode: crate::ai_mode::PageConfig,
}
fn default_timeout() -> u64 {
    180
}

pub struct Browser {
    // Leave Chrome open on exit so login and diagnostics remain accessible.
    _process: Option<Child>,
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    request_id: u64,
    rate_limited: bool,
    retry_after_secs: u64,
}

impl Browser {
    pub async fn start(cfg: &Config) -> Result<Self> {
        let profile = std::env::current_dir()?.join(&cfg.profile_dir);
        std::fs::create_dir_all(&profile)?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()?;
        let endpoint = format!("http://127.0.0.1:{}/json", cfg.cdp_port);
        let running = tokio::net::TcpStream::connect(("127.0.0.1", cfg.cdp_port))
            .await
            .is_ok();
        let mut cmd = Command::new(&cfg.chrome_binary);
        cmd.arg(format!("--remote-debugging-port={}", cfg.cdp_port))
            .arg("--remote-debugging-address=127.0.0.1")
            .arg(format!("--user-data-dir={}", profile.display()))
            .args([
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-background-timer-throttling",
                "--disable-renderer-backgrounding",
            ])
            .arg(crate::ai_mode::START_URL);
        if cfg.headless {
            cmd.arg("--headless=new");
        }
        let child = if running {
            None
        } else {
            Some(
                cmd.spawn()
                    .context("Не удалось запустить Chrome; проверьте chrome_binary")?,
            )
        };
        for _ in 0..60 {
            if let Ok(response) = client.get(&endpoint).send().await {
                if let Ok(targets) = response.json::<Vec<Value>>().await {
                    for target in targets {
                        let url = target["url"].as_str().unwrap_or_default();
                        if target["type"] == "page" && crate::ai_mode::is_search_url(url) {
                            if let Some(ws_url) = target["webSocketDebuggerUrl"].as_str() {
                                let mut browser = Self::connect_url(ws_url).await?;
                                browser._process = child;
                                browser.command("Runtime.enable", json!({})).await?;
                                browser.command("Page.enable", json!({})).await?;
                                browser.command("Network.enable", json!({})).await?;
                                // CDP may advertise the navigation URL before the document exists.
                                for _ in 0..30 {
                                    if browser.evaluate("document.readyState !== 'loading' && location.href !== 'about:blank'").await?.as_bool() == Some(true) { break; }
                                    tokio::time::sleep(Duration::from_millis(500)).await;
                                }
                                return Ok(browser);
                            }
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        bail!("Не найдена вкладка Google AI Mode. Проверьте открытый Chrome и доступ к Google.")
    }

    pub async fn command(&mut self, method: &str, params: Value) -> Result<Value> {
        self.request_id += 1;
        let id = self.request_id;
        timeout(Duration::from_secs(15), async {
            self.ws
                .send(Message::Text(
                    json!({"id":id,"method":method,"params":params}).to_string(),
                ))
                .await?;
            while let Some(message) = self.ws.next().await {
                match message? {
                    Message::Text(text) => {
                        let value: Value = serde_json::from_str(&text)?;
                        if value["method"] == "Network.responseReceived"
                            && value["params"]["response"]["status"].as_f64() == Some(429.0)
                        {
                            self.rate_limited = true;
                            self.retry_after_secs = self
                                .retry_after_secs
                                .max(retry_after(&value["params"]["response"]["headers"]));
                        }
                        if value["id"].as_u64() == Some(id) {
                            if let Some(error) = value.get("error") {
                                bail!("CDP {method}: {error}");
                            }
                            return Ok(value["result"].clone());
                        }
                    }
                    Message::Ping(data) => self.ws.send(Message::Pong(data)).await?,
                    Message::Close(_) => bail!("Chrome закрыл соединение"),
                    _ => {}
                }
            }
            Err(anyhow!("Соединение с Chrome закрыто"))
        })
        .await
        .context("Chrome не ответил за 15 секунд")?
    }

    pub async fn evaluate(&mut self, script: &str) -> Result<Value> {
        let result = self
            .command(
                "Runtime.evaluate",
                json!({"expression":script,"returnByValue":true,"awaitPromise":true}),
            )
            .await?;
        if let Some(exception) = result.get("exceptionDetails") {
            bail!(
                "Ошибка страницы: {}",
                exception["exception"]["description"]
                    .as_str()
                    .unwrap_or("JavaScript exception")
            );
        }
        Ok(result["result"]["value"].clone())
    }
    pub async fn connect_url(url: &str) -> Result<Self> {
        let parsed = reqwest::Url::parse(url)?;
        if parsed.scheme() != "ws"
            || !matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
        {
            bail!("Only local CDP endpoints are accepted");
        }
        let (ws, _) = timeout(Duration::from_secs(10), connect_async(url)).await??;
        let browser = Self {
            _process: None,
            ws,
            request_id: 0,
            rate_limited: false,
            retry_after_secs: 0,
        };
        Ok(browser)
    }
    pub async fn browser_endpoint(cfg: &Config) -> Result<String> {
        let value: Value = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()?
            .get(format!("http://127.0.0.1:{}/json/version", cfg.cdp_port))
            .send()
            .await?
            .json()
            .await?;
        value["webSocketDebuggerUrl"]
            .as_str()
            .map(str::to_owned)
            .context("Chrome browser CDP endpoint missing")
    }
    pub fn take_rate_limit(&mut self) -> Option<u64> {
        if std::mem::take(&mut self.rate_limited) {
            Some(std::mem::take(&mut self.retry_after_secs))
        } else {
            None
        }
    }
}

fn retry_after(headers: &Value) -> u64 {
    let value = headers
        .as_object()
        .and_then(|headers| {
            headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("retry-after"))
        })
        .and_then(|(_, v)| v.as_str())
        .unwrap_or("");
    value
        .parse::<u64>()
        .ok()
        .or_else(|| {
            httpdate::parse_http_date(value)
                .ok()
                .and_then(|date| date.duration_since(std::time::SystemTime::now()).ok())
                .map(|d| d.as_secs().saturating_add(1))
        })
        .unwrap_or(0)
}
#[cfg(test)]
mod retry_tests {
    use super::*;
    #[test]
    fn respects_retry_after_headers() {
        assert_eq!(retry_after(&json!({"Retry-After":"300"})), 300);
        let date = std::time::SystemTime::now() + Duration::from_secs(120);
        assert!(retry_after(&json!({"retry-after":httpdate::fmt_http_date(date)})) >= 119);
    }
    #[tokio::test]
    async fn remote_and_lookalike_cdp_hosts_are_rejected_without_connecting() {
        for url in [
            "ws://example.com/devtools/page/test",
            "ws://localhost.evil.test/devtools/page/test",
            "wss://127.0.0.1/devtools/page/test",
        ] {
            assert!(Browser::connect_url(url)
                .await
                .err()
                .unwrap()
                .to_string()
                .contains("Only local"));
        }
    }
}
