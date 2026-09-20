use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use serde_json::{json, Map};
use url::Url;

use crate::{database::Database, version};

pub const DEFAULT_MIRROR_URL: &str = "https://ghproxy.net/";
const MIRROR_URL_KEY: &str = "update_mirror_url";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorSettings {
    pub url: String,
    pub default_url: String,
}

pub fn settings(db: &Database) -> Result<MirrorSettings> {
    let saved = db.settings_map()?;
    Ok(MirrorSettings {
        url: saved
            .get(MIRROR_URL_KEY)
            .cloned()
            .unwrap_or_else(|| DEFAULT_MIRROR_URL.into()),
        default_url: DEFAULT_MIRROR_URL.into(),
    })
}

pub fn selected_url(db: &Database, enabled: bool) -> Result<Option<String>> {
    if !enabled {
        return Ok(None);
    }
    normalize_url(&settings(db)?.url).map(Some)
}

fn normalize_url(input: &str) -> Result<String> {
    let input = input.trim();
    if input.is_empty() {
        bail!("请输入国内加速地址");
    }
    if input
        .chars()
        .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        bail!("加速地址不能包含空白或控制字符");
    }
    let url = Url::parse(input)
        .map_err(|_| anyhow!("地址格式无效，请填写完整的 http:// 或 https:// 地址"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        bail!("加速地址必须是包含主机名的 HTTP 或 HTTPS 地址");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("加速地址不能包含用户名或密码");
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("请填写加速服务的基础地址，不要包含查询参数或锚点");
    }
    Ok(format!("{}/", url.as_str().trim_end_matches('/')))
}

pub async fn save(db: &Database, input: &str) -> Result<MirrorSettings> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent(format!("proxy-load/{}", version::VERSION))
        .build()?;
    save_with_client(db, input, &client).await
}

async fn save_with_client(
    db: &Database,
    input: &str,
    client: &reqwest::Client,
) -> Result<MirrorSettings> {
    let url = normalize_url(input)?;
    // 只请求基础地址并检查响应头，不读取正文、不查询版本或下载更新包，也不附加 GitHub Token。
    let response = client.get(&url).send().await.map_err(|error| {
        if error.is_timeout() {
            anyhow!("加速地址连接超时，请检查地址或网络后重试")
        } else {
            anyhow!("无法连接加速地址：{error}")
        }
    })?;
    if !response.status().is_success() {
        bail!("加速地址不可用：HTTP {}，未保存此次修改", response.status());
    }
    db.save_settings(&Map::from_iter([(MIRROR_URL_KEY.into(), json!(url))]))?;
    Ok(MirrorSettings {
        url,
        default_url: DEFAULT_MIRROR_URL.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    fn client(timeout_ms: u64) -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(timeout_ms))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .unwrap()
    }

    async fn server(response: &'static str) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mirror/", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(stream.read_u8().await.unwrap());
            }
            stream.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8(request).unwrap()
        });
        (url, task)
    }

    #[test]
    fn default_and_disabled_selection_preserve_existing_settings() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(settings(&db).unwrap().url, DEFAULT_MIRROR_URL);
        assert_eq!(settings(&db).unwrap().default_url, DEFAULT_MIRROR_URL);
        assert_eq!(
            selected_url(&db, true).unwrap().as_deref(),
            Some(DEFAULT_MIRROR_URL)
        );
        db.save_settings(&Map::from_iter([(MIRROR_URL_KEY.into(), json!("invalid"))]))
            .unwrap();
        assert!(selected_url(&db, true).is_err());
        assert_eq!(selected_url(&db, false).unwrap(), None);
        assert_eq!(
            settings(&db).unwrap().url,
            "invalid",
            "损坏配置仍可在界面中修正"
        );
    }

    #[test]
    fn normalizes_custom_domains_ports_and_path_prefixes() {
        assert_eq!(
            normalize_url("  https://GHProxy.NET  ").unwrap(),
            DEFAULT_MIRROR_URL
        );
        assert_eq!(
            normalize_url("http://localhost:8080/proxy///").unwrap(),
            "http://localhost:8080/proxy/"
        );
        assert_eq!(
            normalize_url("https://mirror.example/gh/").unwrap(),
            "https://mirror.example/gh/"
        );
    }

    #[test]
    fn rejects_unsafe_or_non_base_urls() {
        for input in [
            "",
            "ghproxy.net",
            "ftp://example.com",
            "file:///tmp/a",
            "https://user:secret@example.com",
            "https://example.com/?url=",
            "https://example.com/#fragment",
            "https://exam\nple.com",
            "https://example.com/a b",
        ] {
            assert!(normalize_url(input).is_err(), "accepted {input:?}");
        }
    }

    #[tokio::test]
    async fn successful_save_only_gets_the_base_url_and_persists_it() {
        let db = Database::open_in_memory().unwrap();
        let (url, task) = server("HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n").await;
        let result = save_with_client(&db, url.trim_end_matches('/'), &client(2000))
            .await
            .unwrap();
        assert_eq!(result.url, url);
        assert_eq!(settings(&db).unwrap().url, url);
        assert_eq!(selected_url(&db, true).unwrap(), Some(url));
        let request = task.await.unwrap().to_lowercase();
        assert!(request.starts_with("get /mirror/ http/1.1\r\n"));
        assert!(!request.contains("authorization:"));
        assert!(!request.contains("github.com"));
    }

    #[tokio::test]
    async fn save_does_not_wait_for_or_download_the_response_body() {
        let db = Database::open_in_memory().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(stream.read_u8().await.unwrap());
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10000000\r\n\r\n")
                .await
                .unwrap();
            // 不发送正文。读取正文的实现会等待到请求超时，正确实现只需响应头即可保存。
            let mut byte = [0; 1];
            let _ = stream.read(&mut byte).await;
        });
        assert_eq!(
            save_with_client(&db, &url, &client(1000))
                .await
                .unwrap()
                .url,
            url
        );
        task.abort();
        let _ = task.await;
    }

    #[tokio::test]
    async fn redirect_loops_are_rejected_without_overwriting_settings() {
        let db = Database::open_in_memory().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            for _ in 0..6 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(stream.read_u8().await.unwrap());
                }
                stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: /\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
            }
        });
        assert!(save_with_client(&db, &url, &client(2000)).await.is_err());
        assert!(!db.settings_map().unwrap().contains_key(MIRROR_URL_KEY));
        task.abort();
        let _ = task.await;
    }

    #[tokio::test]
    async fn failed_http_status_does_not_replace_the_saved_url() {
        let db = Database::open_in_memory().unwrap();
        let saved = "https://old.example/";
        db.save_settings(&Map::from_iter([(MIRROR_URL_KEY.into(), json!(saved))]))
            .unwrap();
        for response in [
            "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n",
        ] {
            let (url, task) = server(response).await;
            assert!(save_with_client(&db, &url, &client(2000))
                .await
                .unwrap_err()
                .to_string()
                .contains("HTTP"));
            assert_eq!(settings(&db).unwrap().url, saved);
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn invalid_input_and_timeouts_do_not_write_settings() {
        let db = Database::open_in_memory().unwrap();
        assert!(save_with_client(&db, "not-a-url", &client(100))
            .await
            .is_err());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let error = save_with_client(&db, &url, &client(100))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("超时"), "{error}");
        assert!(!db.settings_map().unwrap().contains_key(MIRROR_URL_KEY));
    }

    #[tokio::test]
    async fn saves_the_original_base_after_a_successful_redirect() {
        let db = Database::open_in_memory().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            for (path, response) in [("/", "HTTP/1.1 302 Found\r\nLocation: /home\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
                ("/home", "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") { request.push(stream.read_u8().await.unwrap()); }
                assert!(String::from_utf8(request).unwrap().starts_with(&format!("GET {path} HTTP/1.1")));
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        assert_eq!(
            save_with_client(&db, &url, &client(2000))
                .await
                .unwrap()
                .url,
            url
        );
        task.await.unwrap();
    }
}
