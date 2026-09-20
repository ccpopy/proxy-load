//! Test seams call the production selection, dial and passive-success paths.
use super::*;

pub(crate) struct TestBusinessConnection {
    stream: TcpStream,
    _lease: ConnectionLease,
}
impl TestBusinessConnection {
    pub(crate) async fn echo(&mut self) {
        self.stream.write_all(b"alive").await.unwrap();
        let mut bytes = [0; 5];
        self.stream.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"alive");
    }
}
fn request(host: &str) -> TargetRequest {
    TargetRequest {
        host: host.into(),
        original_host: host.into(),
        port: 443,
        address_type: ADDR_DOMAIN,
        inbound: InboundProtocol::HttpConnect,
        initial_payload: Vec::new(),
    }
}
impl ProxyRuntime {
    pub(crate) async fn readiness_candidates(
        &self,
        host: &str,
        algorithm: &str,
    ) -> Result<Vec<i64>> {
        self.update_load_settings(json!({"algorithm": algorithm}).as_object().unwrap())
            .await?;
        self.select_proxies(&request(host), &HashSet::new())
            .map(|p| p.iter().map(|p| p.id).collect())
    }
    pub(crate) async fn readiness_business_success(&self, id: i64) {
        let _guard = self.lock_proxy_status(id).await;
        self.record_breaker_success(id).await;
        self.record_connection_success_locked(id, 1000).await;
    }
    pub(crate) async fn readiness_dial(
        self: &Arc<Self>,
        host: &str,
    ) -> Result<TestBusinessConnection> {
        let (lease, connected) =
            connect_with_fail_fast(self.clone(), &request(host), Instant::now()).await?;
        Ok(TestBusinessConnection {
            stream: connected.stream,
            _lease: lease,
        })
    }
}
