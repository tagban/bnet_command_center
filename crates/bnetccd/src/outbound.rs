//! A tiny outbound HTTPS `POST` client, shared by the Discord updater and the stats push.
//!
//! Rather than pull in a full HTTP client, this rides the `tokio-rustls` stack already in the
//! tree: one TLS connection per post, a single `POST` with a JSON body, and a peek at the
//! status line. It is best-effort — every call has an overall timeout and returns an error
//! string the caller logs and drops. Only `https://` URLs are accepted.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;

/// Overall deadline for one post (connect + TLS + write + read).
const POST_TIMEOUT: Duration = Duration::from_secs(8);

/// POST `body` as `application/json` to an `https://` URL, optionally with a
/// `Authorization: Bearer <token>` header. Returns `Ok` on any 2xx response.
///
/// # Errors
///
/// A malformed URL, a network/TLS failure, a timeout, or a non-2xx response.
pub async fn post_json(url: &str, body: &str, bearer: Option<&str>) -> Result<(), String> {
    match timeout(POST_TIMEOUT, do_post(url, body, bearer)).await {
        Ok(result) => result,
        Err(_) => Err("timed out".to_string()),
    }
}

async fn do_post(url: &str, body: &str, bearer: Option<&str>) -> Result<(), String> {
    let (host, path) = split_url(url).ok_or("not a valid https URL")?;
    let auth = bearer.map_or_else(String::new, |t| format!("Authorization: Bearer {t}\r\n"));
    let request = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         User-Agent: bnetccd\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         {auth}\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len(),
    );

    let tcp = TcpStream::connect((host.as_str(), 443)).await.map_err(|e| format!("connect: {e}"))?;
    let connector = TlsConnector::from(Arc::new(client_config()));
    let server_name =
        rustls::pki_types::ServerName::try_from(host.clone()).map_err(|e| format!("server name: {e}"))?;
    let mut tls = connector.connect(server_name, tcp).await.map_err(|e| format!("tls: {e}"))?;

    tls.write_all(request.as_bytes()).await.map_err(|e| format!("write: {e}"))?;
    tls.flush().await.map_err(|e| format!("flush: {e}"))?;

    let mut buf = [0u8; 256];
    let n = tls.read(&mut buf).await.map_err(|e| format!("read: {e}"))?;
    let head = String::from_utf8_lossy(&buf[..n]);
    if head.starts_with("HTTP/1.1 2") || head.starts_with("HTTP/1.0 2") {
        Ok(())
    } else {
        Err(format!("unexpected response: {}", head.lines().next().unwrap_or("").trim()))
    }
}

/// Build a rustls client config trusting the Mozilla roots. Installs a default crypto
/// provider first, since `ClientConfig::builder()` requires one.
fn client_config() -> rustls::ClientConfig {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth()
}

/// Split `https://host/path…` into `(host, path)`; `None` for anything not https.
fn split_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("https://")?;
    match rest.split_once('/') {
        Some((host, path)) => Some((host.to_string(), format!("/{path}"))),
        None => Some((rest.to_string(), "/".to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_https_urls() {
        assert_eq!(
            split_url("https://example.com/ingest?k=1"),
            Some(("example.com".into(), "/ingest?k=1".into()))
        );
        assert_eq!(split_url("https://example.com"), Some(("example.com".into(), "/".into())));
        assert!(split_url("http://example.com/x").is_none());
    }
}
