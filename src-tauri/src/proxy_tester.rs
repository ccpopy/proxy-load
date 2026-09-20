//! Single-chain, fixed-node probes. Cancellation drops the HTTP driver and its socket inline.
use crate::{
    models::{ProxyRecord, TestResult},
    proxy::{
        failure::{
            ConnectPhase as Phase, Evidence, FailureCode as Code, FailureScope as Scope,
            ProbeDiagnostics, ProbeFailure,
        },
        probe_tls,
        probe_transport::{self, BoxIo, MAX_HEADERS},
    },
};
use http_body_util::Empty;
use hyper::{body::Bytes, client::conn::http1, header, Request};
use hyper_util::rt::TokioIo;
use std::{sync::Arc, time::Duration};
use tokio::time::{timeout_at, Instant};
use tokio_rustls::{rustls, TlsConnector};
use url::Url;

pub async fn test_proxy(proxy: &ProxyRecord, test_url: &str, timeout_ms: u64) -> TestResult {
    test_with_tls(
        proxy,
        test_url,
        timeout_ms,
        probe_tls::config(proxy.skip_cert_verify == 1),
    )
    .await
}

async fn test_with_tls(
    proxy: &ProxyRecord,
    test_url: &str,
    timeout_ms: u64,
    tls: Arc<rustls::ClientConfig>,
) -> TestResult {
    let start = Instant::now();
    let deadline = start + Duration::from_millis(timeout_ms.min(300_000));
    let mut diagnostic = ProbeDiagnostics::default();
    let mut status = None;
    let result = execute(proxy, test_url, deadline, tls, &mut diagnostic, &mut status).await;
    diagnostic.timings.total_us = start.elapsed().as_micros().min(u64::MAX as u128) as u64;
    if let Err(error) = &result {
        diagnostic.phase = Some(error.phase);
        diagnostic.scope = Some(error.scope);
        diagnostic.code = Some(error.code.clone());
        diagnostic.raw_os_error = error.raw_os_error;
        match error.phase {
            Phase::TargetTls => diagnostic.evidence.target_tls = Evidence::Failed,
            Phase::TunnelConnect => diagnostic.evidence.target_tunnel = Evidence::Failed,
            _ => {}
        }
    }
    TestResult {
        success: result.is_ok(),
        response_time: start.elapsed().as_millis().min(i64::MAX as u128) as i64,
        status_code: status,
        error: result.as_ref().err().map(ProbeFailure::message),
        failure_scope: result.err().map(|e| e.scope.legacy().to_string()),
        diagnostics: diagnostic,
    }
}

fn validate_url(mut target: Url) -> Result<Url, ProbeFailure> {
    if !matches!(target.scheme(), "http" | "https")
        || target.host().is_none()
        || !target.username().is_empty()
        || target.password().is_some()
        || target.as_str().len() > 8192
    {
        return Err(ProbeFailure::new(
            Phase::Redirect,
            Scope::Configuration,
            Code::UnsupportedTarget,
        ));
    }
    target.set_fragment(None);
    Ok(target)
}

async fn execute(
    proxy: &ProxyRecord,
    url: &str,
    deadline: Instant,
    tls: Arc<rustls::ClientConfig>,
    d: &mut ProbeDiagnostics,
    status: &mut Option<u16>,
) -> Result<(), ProbeFailure> {
    let mut target = validate_url(Url::parse(url).map_err(|_| {
        ProbeFailure::new(
            Phase::Redirect,
            Scope::Configuration,
            Code::InvalidConfiguration,
        )
    })?)?;
    for redirects in 0..=5 {
        if Instant::now() >= deadline {
            return Err(ProbeFailure::new(
                Phase::Redirect,
                Scope::Unknown,
                Code::Timeout,
            ));
        }
        d.redirects = redirects;
        d.final_origin = Some(target.origin().ascii_serialization());
        d.evidence = Default::default();
        d.timings = Default::default();
        *status = None;
        let transport = probe_transport::open_probe_transport(proxy, &target, deadline, d).await?;
        let io = if target.scheme() == "https" {
            target_tls(transport.io, &target, deadline, tls.clone(), d).await?
        } else {
            d.evidence.target_tls = Evidence::NotApplicable;
            transport.io
        };
        let response = request_headers(
            io,
            &target,
            transport.absolute_form,
            transport.proxy_authorization.as_deref(),
            deadline,
            d,
        )
        .await?;
        let code = response.status().as_u16();
        *status = Some(code);
        if transport.absolute_form {
            if code == 407 {
                d.evidence.proxy_auth = Evidence::Rejected;
                return Err(ProbeFailure::new(
                    Phase::ProxyAuth,
                    Scope::Proxy,
                    Code::ProxyAuthRejected,
                ));
            }
            d.evidence.proxy_auth = Evidence::Accepted;
        }
        if matches!(code, 301 | 302 | 303 | 307 | 308) {
            if let Some(location) = response.headers().get(header::LOCATION) {
                if redirects == 5 {
                    return Err(ProbeFailure::new(
                        Phase::Redirect,
                        Scope::TargetRoute,
                        Code::RedirectLimit,
                    ));
                }
                target = validate_url(
                    location
                        .to_str()
                        .ok()
                        .and_then(|value| target.join(value).ok())
                        .ok_or_else(|| {
                            ProbeFailure::new(
                                Phase::Redirect,
                                Scope::TargetRoute,
                                Code::InvalidProtocol,
                            )
                        })?,
                )?;
                continue;
            }
        }
        if !(200..500).contains(&code) || code == 407 {
            return Err(ProbeFailure::new(
                Phase::HttpResponse,
                Scope::TargetRoute,
                Code::HttpStatus(code),
            ));
        }
        return Ok(());
    }
    unreachable!("redirect loop has an explicit limit")
}

async fn target_tls(
    io: BoxIo,
    target: &Url,
    deadline: Instant,
    config: Arc<rustls::ClientConfig>,
    d: &mut ProbeDiagnostics,
) -> Result<BoxIo, ProbeFailure> {
    if Instant::now() >= deadline {
        return Err(ProbeFailure::new(
            Phase::TargetTls,
            Scope::TargetRoute,
            Code::Timeout,
        ));
    }
    let name =
        rustls::pki_types::ServerName::try_from(probe_transport::host(target)?).map_err(|_| {
            ProbeFailure::new(
                Phase::TargetTls,
                Scope::Configuration,
                Code::UnsupportedTarget,
            )
        })?;
    let start = Instant::now();
    let result = timeout_at(deadline, TlsConnector::from(config).connect(name, io))
        .await
        .map_err(|_| ProbeFailure::new(Phase::TargetTls, Scope::TargetRoute, Code::Timeout))?;
    let stream = result.map_err(|error| {
        #[cfg(test)]
        if std::env::var_os("PROXY_LOAD_PROBE_BENCH_OUTPUT").is_some() {
            eprintln!("probe fixture TLS failure: {error:?}");
        }
        let certificate = error
            .get_ref()
            .and_then(|e| e.downcast_ref::<rustls::Error>())
            .is_some_and(|e| matches!(e, rustls::Error::InvalidCertificate(_)));
        let mut failure = ProbeFailure::io(Phase::TargetTls, Scope::TargetRoute, error);
        if certificate {
            failure.code = Code::InvalidCertificate;
        }
        failure
    })?;
    d.timings.target_tls_us = Some(start.elapsed().as_micros() as u64);
    d.evidence.target_tls = Evidence::Established;
    Ok(Box::new(stream))
}

async fn request_headers(
    io: BoxIo,
    target: &Url,
    absolute: bool,
    authorization: Option<&str>,
    deadline: Instant,
    d: &mut ProbeDiagnostics,
) -> Result<hyper::Response<hyper::body::Incoming>, ProbeFailure> {
    if Instant::now() >= deadline {
        return Err(ProbeFailure::new(
            Phase::HttpWrite,
            Scope::Unknown,
            Code::Timeout,
        ));
    }
    let uri = if absolute {
        target.as_str().to_string()
    } else {
        let mut uri = target.path().to_string();
        if let Some(query) = target.query() {
            uri.push('?');
            uri.push_str(query);
        }
        uri
    };
    let mut request = Request::get(uri)
        .header(header::HOST, probe_transport::authority(target)?)
        .header(header::CONNECTION, "close");
    if let Some(auth) = authorization {
        request = request.header(header::PROXY_AUTHORIZATION, auth);
    }
    let request = request.body(Empty::<Bytes>::new()).map_err(|_| {
        ProbeFailure::new(
            Phase::HttpWrite,
            Scope::Configuration,
            Code::InvalidConfiguration,
        )
    })?;
    let start = Instant::now();
    let scope = if absolute {
        Scope::Unknown
    } else {
        Scope::TargetRoute
    };
    let response = timeout_at(deadline, async {
        let (mut sender, connection) = http1::Builder::new().max_headers(128).max_buf_size(MAX_HEADERS)
            .handshake(TokioIo::new(io)).await.map_err(|e| ProbeFailure::external(Phase::HttpWrite, e))?;
        // Poll both inline: cancellation drops both; no detached driver tasks or sockets.
        tokio::pin!(connection);
        let send = sender.send_request(request);
        tokio::pin!(send);
        tokio::select! {
            biased;
            response = &mut send => response.map_err(|e| ProbeFailure::external(Phase::HttpResponse, e)),
            _completed = &mut connection => {
                // The driver may deliver a zero-length response and finish in the same poll.
                // Drain that response channel before treating a finished connection as failure.
                send.await.map_err(|e| ProbeFailure::external(Phase::HttpResponse, e))
            }
        }
    }).await.map_err(|_| ProbeFailure::new(Phase::HttpResponse, scope, Code::Timeout))??;
    d.timings.http_final_headers_us = Some(start.elapsed().as_micros() as u64);
    Ok(response)
}

#[cfg(test)]
mod tests;
