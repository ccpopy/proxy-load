//! A probe owns one explicitly selected upstream socket; never invokes business failover.
use super::failure::{
    stage, ConnectPhase as Phase, Evidence, FailureCode as Code, FailureScope as Scope,
    ProbeDiagnostics, ProbeFailure,
};
use super::{
    address_type, build_socks4_connect_request, build_socks5_connect_request, InboundProtocol,
    TargetRequest, ADDR_DOMAIN,
};
use crate::models::ProxyRecord;
use base64::{engine::general_purpose::STANDARD, Engine};
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::TcpStream,
    time::Instant,
};
use url::{Host, Url};

pub(crate) const MAX_HEADERS: usize = 64 * 1024;
pub(crate) trait ProbeIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> ProbeIo for T {}
pub(crate) type BoxIo = Box<dyn ProbeIo>;

pub(crate) struct ProbeTransport {
    pub io: BoxIo,
    pub absolute_form: bool,
    pub proxy_authorization: Option<String>,
}

pub(crate) struct PrefixedIo<T> {
    pub inner: T,
    pub prefix: Vec<u8>,
    pub position: usize,
}
impl<T: AsyncRead + Unpin> AsyncRead for PrefixedIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.position < self.prefix.len() {
            let len = buf.remaining().min(self.prefix.len() - self.position);
            buf.put_slice(&self.prefix[self.position..self.position + len]);
            self.position += len;
            Poll::Ready(Ok(()))
        } else {
            Pin::new(&mut self.inner).poll_read(cx, buf)
        }
    }
}
impl<T: AsyncWrite + Unpin> AsyncWrite for PrefixedIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

pub(crate) fn host(target: &Url) -> Result<String, ProbeFailure> {
    target
        .host()
        .map(|h| match h {
            Host::Domain(h) => h.to_string(),
            Host::Ipv4(ip) => ip.to_string(),
            Host::Ipv6(ip) => ip.to_string(),
        })
        .ok_or_else(|| invalid(Phase::Redirect))
}
pub(crate) fn authority(target: &Url) -> Result<String, ProbeFailure> {
    let h = host(target)?;
    let h = if h.contains(':') { format!("[{h}]") } else { h };
    Ok(format!(
        "{h}:{}",
        target
            .port_or_known_default()
            .ok_or_else(|| invalid(Phase::Redirect))?
    ))
}
fn invalid(phase: Phase) -> ProbeFailure {
    ProbeFailure::new(phase, Scope::Configuration, Code::InvalidConfiguration)
}
fn protocol(phase: Phase) -> ProbeFailure {
    ProbeFailure::new(phase, Scope::Proxy, Code::InvalidProtocol)
}
fn micros(start: Instant) -> u64 {
    start.elapsed().as_micros().min(u64::MAX as u128) as u64
}

pub(crate) async fn open_probe_transport(
    proxy: &ProxyRecord,
    target: &Url,
    deadline: Instant,
    diagnostic: &mut ProbeDiagnostics,
) -> Result<ProbeTransport, ProbeFailure> {
    if !matches!(
        proxy.proxy_type.as_str(),
        "http" | "https" | "socks4" | "socks5"
    ) || proxy.host.is_empty()
        || !(1..=65535).contains(&proxy.port)
    {
        return Err(invalid(Phase::TcpConnect));
    }
    let target_host = host(target)?;
    let request = TargetRequest {
        host: target_host.clone(),
        original_host: target_host.clone(),
        port: target
            .port_or_known_default()
            .ok_or_else(|| invalid(Phase::Redirect))?,
        address_type: address_type(&target_host).unwrap_or(ADDR_DOMAIN),
        inbound: InboundProtocol::HttpConnect,
        initial_payload: Vec::new(),
    };
    let start = Instant::now();
    let addresses: Vec<_> = stage(
        deadline,
        Phase::ResolveProxy,
        Scope::Unknown,
        tokio::net::lookup_host((proxy.host.as_str(), proxy.port as u16)),
    )
    .await?
    .collect();
    diagnostic.timings.resolve_proxy_us = Some(micros(start));
    let start = Instant::now();
    let mut connected = None;
    let mut failure = None;
    for address in addresses {
        match stage(
            deadline,
            Phase::TcpConnect,
            Scope::Proxy,
            TcpStream::connect(address),
        )
        .await
        {
            Ok(stream) => {
                connected = Some(stream);
                break;
            }
            Err(error) => {
                failure = Some(error);
            }
        }
    }
    let mut stream = connected.ok_or_else(|| {
        diagnostic.evidence.proxy_tcp = Evidence::Failed;
        failure.unwrap_or_else(|| invalid(Phase::ResolveProxy))
    })?;
    diagnostic.timings.proxy_tcp_us = Some(micros(start));
    diagnostic.evidence.proxy_tcp = Evidence::Established;
    let mut prefix = Vec::new();
    let absolute_form =
        matches!(proxy.proxy_type.as_str(), "http" | "https") && target.scheme() == "http";
    let authorization = proxy
        .username
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|user| {
            format!(
                "Basic {}",
                STANDARD.encode(format!(
                    "{user}:{}",
                    proxy.password.as_deref().unwrap_or("")
                ))
            )
        });
    match proxy.proxy_type.as_str() {
        "socks5" => socks5(&mut stream, proxy, &request, deadline, diagnostic).await?,
        "socks4" => socks4(&mut stream, proxy, &request, deadline, diagnostic).await?,
        _ if !absolute_form => {
            prefix = http_connect(
                &mut stream,
                target,
                authorization.as_deref(),
                deadline,
                diagnostic,
            )
            .await?;
        }
        _ => {}
    }
    Ok(ProbeTransport {
        io: Box::new(PrefixedIo {
            inner: stream,
            prefix,
            position: 0,
        }),
        absolute_form,
        proxy_authorization: absolute_form.then_some(authorization).flatten(),
    })
}

async fn socks5(
    stream: &mut TcpStream,
    proxy: &ProxyRecord,
    request: &TargetRequest,
    deadline: Instant,
    d: &mut ProbeDiagnostics,
) -> Result<(), ProbeFailure> {
    let user = proxy.username.as_deref().unwrap_or("");
    let password = proxy.password.as_deref().unwrap_or("");
    if user.len() > 255 || password.len() > 255 {
        return Err(invalid(Phase::ProxyAuth));
    }
    let start = Instant::now();
    let response = stage(deadline, Phase::SocksNegotiation, Scope::Proxy, async {
        stream
            .write_all(if user.is_empty() {
                &[5, 1, 0]
            } else {
                &[5, 2, 0, 2]
            })
            .await?;
        let mut r = [0; 2];
        stream.read_exact(&mut r).await?;
        Ok(r)
    })
    .await?;
    if response[0] != 5 {
        return Err(protocol(Phase::SocksNegotiation));
    }
    match response[1] {
        0 => {
            d.evidence.proxy_auth = Evidence::Accepted;
        }
        2 if !user.is_empty() => {
            let response = stage(deadline, Phase::ProxyAuth, Scope::Proxy, async {
                let mut auth = vec![1, user.len() as u8];
                auth.extend_from_slice(user.as_bytes());
                auth.push(password.len() as u8);
                auth.extend_from_slice(password.as_bytes());
                stream.write_all(&auth).await?;
                let mut r = [0; 2];
                stream.read_exact(&mut r).await?;
                Ok(r)
            })
            .await?;
            if response[0] != 1 {
                return Err(protocol(Phase::ProxyAuth));
            }
            if response[1] != 0 {
                d.evidence.proxy_auth = Evidence::Rejected;
                return Err(ProbeFailure::new(
                    Phase::ProxyAuth,
                    Scope::Proxy,
                    Code::ProxyAuthRejected,
                ));
            }
            d.evidence.proxy_auth = Evidence::Accepted;
        }
        2 => return Err(invalid(Phase::ProxyAuth)),
        255 => {
            d.evidence.proxy_auth = Evidence::Rejected;
            return Err(ProbeFailure::new(
                Phase::ProxyAuth,
                Scope::Proxy,
                Code::ProxyAuthRejected,
            ));
        }
        _ => return Err(protocol(Phase::SocksNegotiation)),
    }
    d.timings.proxy_auth_us = Some(micros(start));
    let packet =
        build_socks5_connect_request(request).map_err(|_| invalid(Phase::TunnelConnect))?;
    let start = Instant::now();
    let header = stage(deadline, Phase::TunnelConnect, Scope::TargetRoute, async {
        stream.write_all(&packet).await?;
        let mut r = [0; 4];
        stream.read_exact(&mut r).await?;
        Ok(r)
    })
    .await?;
    d.timings.connect_response_us = Some(micros(start));
    if header[0] != 5 || header[2] != 0 {
        return Err(protocol(Phase::TunnelConnect));
    }
    if header[1] != 0 {
        d.evidence.target_tunnel = Evidence::Failed;
        let scope = if matches!(header[1], 1 | 7 | 9..=255) {
            Scope::Proxy
        } else {
            Scope::TargetRoute
        };
        return Err(ProbeFailure::new(
            Phase::TunnelConnect,
            scope,
            Code::SocksReply(header[1]),
        ));
    }
    let len = match header[3] {
        1 => 6,
        4 => 18,
        3 => {
            stage(
                deadline,
                Phase::TunnelConnect,
                Scope::Proxy,
                stream.read_u8(),
            )
            .await? as usize
                + 2
        }
        _ => return Err(protocol(Phase::TunnelConnect)),
    };
    stage(
        deadline,
        Phase::TunnelConnect,
        Scope::Proxy,
        stream.read_exact(&mut vec![0; len]),
    )
    .await?;
    d.evidence.target_tunnel = Evidence::Established;
    Ok(())
}

async fn socks4(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    proxy: &ProxyRecord,
    request: &TargetRequest,
    deadline: Instant,
    d: &mut ProbeDiagnostics,
) -> Result<(), ProbeFailure> {
    let user = proxy.username.as_deref().unwrap_or("");
    if user.len() > 255 || user.contains('\0') {
        return Err(invalid(Phase::ProxyAuth));
    }
    let packet =
        build_socks4_connect_request(request, user).map_err(|_| invalid(Phase::TunnelConnect))?;
    d.evidence.proxy_auth = Evidence::NotApplicable;
    let start = Instant::now();
    let response = stage(deadline, Phase::TunnelConnect, Scope::TargetRoute, async {
        stream.write_all(&packet).await?;
        let mut r = [0; 8];
        stream.read_exact(&mut r).await?;
        Ok(r)
    })
    .await?;
    d.timings.connect_response_us = Some(micros(start));
    if response[0] != 0 {
        return Err(protocol(Phase::TunnelConnect));
    }
    if response[1] != 0x5a {
        d.evidence.target_tunnel = Evidence::Failed;
        return Err(ProbeFailure::new(
            Phase::TunnelConnect,
            if response[1] == 0x5b {
                Scope::TargetRoute
            } else {
                Scope::Proxy
            },
            Code::SocksReply(response[1]),
        ));
    }
    d.evidence.target_tunnel = Evidence::Established;
    Ok(())
}

async fn http_connect(
    stream: &mut TcpStream,
    target: &Url,
    authorization: Option<&str>,
    deadline: Instant,
    d: &mut ProbeDiagnostics,
) -> Result<Vec<u8>, ProbeFailure> {
    let address = authority(target)?;
    let mut request = format!("CONNECT {address} HTTP/1.1\r\nHost: {address}\r\n");
    if let Some(auth) = authorization {
        request.push_str(&format!("Proxy-Authorization: {auth}\r\n"));
    }
    request.push_str("\r\n");
    let start = Instant::now();
    stage(
        deadline,
        Phase::TunnelConnect,
        Scope::Unknown,
        stream.write_all(request.as_bytes()),
    )
    .await?;
    let (status, prefix) = stage(
        deadline,
        Phase::TunnelConnect,
        Scope::Unknown,
        connect_headers(stream),
    )
    .await
    .map_err(|failure| {
        if failure.code == Code::Io(io::ErrorKind::InvalidData) {
            ProbeFailure {
                source: failure.source,
                ..protocol(Phase::TunnelConnect)
            }
        } else {
            failure
        }
    })?;
    d.timings.connect_response_us = Some(micros(start));
    if status == 407 {
        d.evidence.proxy_auth = Evidence::Rejected;
        return Err(ProbeFailure::new(
            Phase::ProxyAuth,
            Scope::Proxy,
            Code::ProxyAuthRejected,
        ));
    }
    if !(200..300).contains(&status) {
        d.evidence.target_tunnel = Evidence::Failed;
        return Err(ProbeFailure::new(
            Phase::TunnelConnect,
            Scope::TargetRoute,
            Code::HttpStatus(status),
        ));
    }
    d.evidence.proxy_auth = Evidence::Accepted;
    d.evidence.target_tunnel = Evidence::Established;
    Ok(prefix)
}

async fn connect_headers(stream: &mut TcpStream) -> io::Result<(u16, Vec<u8>)> {
    let mut buffer = Vec::new();
    let mut consumed = 0;
    loop {
        let mut headers = [httparse::EMPTY_HEADER; 128];
        let mut response = httparse::Response::new(&mut headers);
        match response
            .parse(&buffer)
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?
        {
            httparse::Status::Complete(end) => {
                let status = response.code.ok_or(io::ErrorKind::InvalidData)?;
                if !(100..=599).contains(&status) {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                consumed += end;
                if consumed > MAX_HEADERS {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                let suffix = buffer.split_off(end);
                if (100..200).contains(&status) && status != 101 {
                    buffer = suffix;
                    continue;
                }
                return Ok((status, suffix));
            }
            httparse::Status::Partial => {}
        }
        if consumed + buffer.len() >= MAX_HEADERS {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut bytes = [0; 2048];
        let count = stream.read(&mut bytes).await?;
        if count == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buffer.extend_from_slice(&bytes[..count]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn production_socks4a_preserves_domain_tail_and_typed_replies() {
        let db = crate::database::Database::open_in_memory().unwrap();
        let proxy = db
            .create_proxy(crate::models::ProxyInput {
                name: "socks4a".into(),
                proxy_type: "socks4".into(),
                host: "127.0.0.1".into(),
                port: 1080,
                username: Some("probe-user".into()),
                password: None,
                enabled: Some(1),
                test_url: None,
                test_timeout: None,
                skip_cert_verify: None,
            })
            .unwrap();
        let request = TargetRequest {
            host: "probe.test".into(),
            original_host: "probe.test".into(),
            port: 80,
            address_type: ADDR_DOMAIN,
            inbound: InboundProtocol::HttpForward,
            initial_payload: Vec::new(),
        };
        for reply in [0x5a, 0x5b, 0x5c, 0x5d] {
            let (mut client, mut peer) = tokio::io::duplex(64);
            let server = tokio::spawn(async move {
                let mut packet = [0; 30];
                peer.read_exact(&mut packet).await.unwrap();
                assert_eq!(
                    &packet,
                    b"\x04\x01\x00\x50\x00\x00\x00\x01probe-user\x00probe.test\x00"
                );
                peer.write_all(&[0, reply, 0, 0, 0, 0, 0, 0]).await.unwrap();
            });
            let mut d = ProbeDiagnostics::default();
            let result = socks4(
                &mut client,
                &proxy,
                &request,
                Instant::now() + std::time::Duration::from_secs(1),
                &mut d,
            )
            .await;
            if reply == 0x5a {
                assert!(result.is_ok());
                assert_eq!(d.evidence.target_tunnel, Evidence::Established);
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.code, Code::SocksReply(reply));
                assert_eq!(
                    error.scope,
                    if reply == 0x5b {
                        Scope::TargetRoute
                    } else {
                        Scope::Proxy
                    }
                );
            }
            server.await.unwrap();
        }
    }
}
