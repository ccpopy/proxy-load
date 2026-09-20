//! Typed probe boundaries. Human-readable messages never participate in classification.
use serde::Serialize;
use std::{error::Error, fmt, future::Future, io};
use tokio::time::{timeout_at, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureScope {
    Proxy,
    TargetRoute,
    LocalRoute,
    LocalResource,
    Configuration,
    Unknown,
}

impl FailureScope {
    pub fn legacy(self) -> &'static str {
        match self {
            Self::Proxy => "proxy",
            Self::TargetRoute => "target",
            Self::LocalRoute => "network",
            Self::LocalResource => "local_resource",
            Self::Configuration => "configuration",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectPhase {
    ResolveProxy,
    TcpConnect,
    SocksNegotiation,
    ProxyAuth,
    TunnelConnect,
    TargetTls,
    HttpWrite,
    HttpResponse,
    Redirect,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum FailureCode {
    Io(#[serde(serialize_with = "serialize_io_kind")] io::ErrorKind),
    Timeout,
    ProxyAuthRejected,
    SocksReply(u8),
    HttpStatus(u16),
    InvalidProtocol,
    InvalidCertificate,
    InvalidConfiguration,
    RedirectLimit,
    UnsupportedTarget,
    UnknownTransport,
}

fn serialize_io_kind<S: serde::Serializer>(
    kind: &io::ErrorKind,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(match kind {
        io::ErrorKind::ConnectionRefused => "connection_refused",
        io::ErrorKind::ConnectionReset => "connection_reset",
        io::ErrorKind::ConnectionAborted => "connection_aborted",
        io::ErrorKind::TimedOut => "timed_out",
        io::ErrorKind::NetworkDown => "network_down",
        io::ErrorKind::NetworkUnreachable => "network_unreachable",
        io::ErrorKind::UnexpectedEof => "unexpected_eof",
        io::ErrorKind::InvalidData => "invalid_data",
        io::ErrorKind::OutOfMemory => "out_of_memory",
        _ => "io_error",
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProbeFailure {
    pub phase: ConnectPhase,
    pub scope: FailureScope,
    pub code: FailureCode,
    pub raw_os_error: Option<i32>,
    #[serde(skip)]
    pub source: Option<Box<dyn Error + Send + Sync>>,
}

impl ProbeFailure {
    pub fn new(phase: ConnectPhase, scope: FailureScope, code: FailureCode) -> Self {
        Self {
            phase,
            scope,
            code,
            raw_os_error: None,
            source: None,
        }
    }
    pub fn io(phase: ConnectPhase, scope: FailureScope, error: io::Error) -> Self {
        let scope = if matches!(
            error.kind(),
            io::ErrorKind::NetworkDown | io::ErrorKind::NetworkUnreachable
        ) || cfg!(windows) && matches!(error.raw_os_error(), Some(10050 | 10051))
            || cfg!(target_os = "linux") && matches!(error.raw_os_error(), Some(100 | 101))
            || cfg!(target_os = "macos") && matches!(error.raw_os_error(), Some(50 | 51))
        {
            FailureScope::LocalRoute
        } else if matches!(error.kind(), io::ErrorKind::OutOfMemory) {
            FailureScope::LocalResource
        } else {
            scope
        };
        Self {
            phase,
            scope,
            code: FailureCode::Io(error.kind()),
            raw_os_error: error.raw_os_error(),
            source: Some(Box::new(error)),
        }
    }
    pub fn external(phase: ConnectPhase, error: impl Error + Send + Sync + 'static) -> Self {
        Self {
            source: Some(Box::new(error)),
            ..Self::new(phase, FailureScope::Unknown, FailureCode::UnknownTransport)
        }
    }
    pub fn message(&self) -> String {
        // Intentionally omit source text, URLs and credentials from IPC/logs.
        format!(
            "测活失败（{:?} / {:?} / {:?}）",
            self.phase, self.scope, self.code
        )
    }
}
impl fmt::Display for ProbeFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}
impl Error for ProbeFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_deref().map(|e| e as &(dyn Error + 'static))
    }
}

pub(crate) async fn stage<T>(
    deadline: Instant,
    phase: ConnectPhase,
    scope: FailureScope,
    future: impl Future<Output = io::Result<T>>,
) -> Result<T, ProbeFailure> {
    if Instant::now() >= deadline {
        return Err(ProbeFailure::new(phase, scope, FailureCode::Timeout));
    }
    timeout_at(deadline, future)
        .await
        .map_err(|_| ProbeFailure::new(phase, scope, FailureCode::Timeout))?
        .map_err(|e| ProbeFailure::io(phase, scope, e))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    #[default]
    NotObserved,
    NotApplicable,
    Established,
    Accepted,
    Rejected,
    Failed,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransportEvidence {
    pub proxy_tcp: Evidence,
    pub proxy_auth: Evidence,
    pub target_tunnel: Evidence,
    pub target_tls: Evidence,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeTimings {
    pub resolve_proxy_us: Option<u64>,
    pub proxy_tcp_us: Option<u64>,
    pub proxy_auth_us: Option<u64>,
    pub connect_response_us: Option<u64>,
    pub target_tls_us: Option<u64>,
    pub http_final_headers_us: Option<u64>,
    pub total_us: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeDiagnostics {
    pub evidence: TransportEvidence,
    pub timings: ProbeTimings,
    pub phase: Option<ConnectPhase>,
    pub code: Option<FailureCode>,
    pub scope: Option<FailureScope>,
    pub raw_os_error: Option<i32>,
    pub redirects: usize,
    pub final_origin: Option<String>,
}

impl ProbeDiagnostics {
    pub fn proxy_proven(&self) -> bool {
        self.evidence.proxy_tcp == Evidence::Established
            && (self.evidence.proxy_auth == Evidence::Accepted
                || self.evidence.proxy_auth == Evidence::NotApplicable
                    && self.evidence.target_tunnel == Evidence::Established)
            && !matches!(
                self.scope,
                Some(
                    FailureScope::Proxy
                        | FailureScope::Unknown
                        | FailureScope::Configuration
                        | FailureScope::LocalRoute
                        | FailureScope::LocalResource
                )
            )
    }
}
