use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use reqwest::{Client, Proxy};
use url::Url;

use crate::models::{ProxyRecord, TestResult};

pub async fn test_proxy(proxy: &ProxyRecord, test_url: &str, timeout_ms: u64) -> TestResult {
    let start = Instant::now();
    match execute_proxy_test(proxy, test_url, timeout_ms).await {
        Ok(status) => TestResult {
            success: true,
            response_time: elapsed_ms(start),
            status_code: Some(status),
            error: None,
        },
        Err(error) => TestResult {
            success: false,
            response_time: elapsed_ms(start),
            status_code: None,
            error: Some(error.to_string()),
        },
    }
}

async fn execute_proxy_test(proxy: &ProxyRecord, test_url: &str, timeout_ms: u64) -> Result<u16> {
    let target = Url::parse(test_url).with_context(|| format!("测试地址无效: {test_url}"))?;
    if target.scheme() != "http" && target.scheme() != "https" {
        return Err(anyhow!("不支持的测试地址协议: {}", target.scheme()));
    }

    let proxy_url = build_proxy_url(proxy)?;
    let reqwest_proxy = Proxy::all(proxy_url.as_str())
        .with_context(|| format!("代理 URL 无效: {}", proxy_url.as_str()))?;
    let client = Client::builder()
        .proxy(reqwest_proxy)
        .timeout(Duration::from_millis(timeout_ms.max(1)))
        .danger_accept_invalid_certs(proxy.skip_cert_verify == 1)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?;

    let response = client.get(target).send().await?;
    let status = response.status();
    if status.as_u16() < 200 || status.as_u16() >= 500 {
        return Err(anyhow!("HTTP状态码不符合连通性策略: {}", status.as_u16()));
    }
    Ok(status.as_u16())
}

fn build_proxy_url(proxy: &ProxyRecord) -> Result<Url> {
    let scheme = match proxy.proxy_type.as_str() {
        "http" => "http",
        "https" => "https",
        "socks4" => "socks4",
        "socks5" => "socks5h",
        other => return Err(anyhow!("不支持的代理类型: {other}")),
    };
    let mut url = Url::parse(&format!("{scheme}://{}:{}", proxy.host, proxy.port))?;
    if let Some(username) = proxy.username.as_deref().filter(|value| !value.is_empty()) {
        url.set_username(username)
            .map_err(|_| anyhow!("代理用户名包含非法字符"))?;
        if let Some(password) = proxy.password.as_deref() {
            url.set_password(Some(password))
                .map_err(|_| anyhow!("代理密码包含非法字符"))?;
        }
    }
    Ok(url)
}

fn elapsed_ms(start: Instant) -> i64 {
    start.elapsed().as_millis() as i64
}
