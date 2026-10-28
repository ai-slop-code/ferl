//! Test-only PostgreSQL wire fault: forward startup/queries, but withhold the
//! backend's successful COMMIT CommandComplete. No production TLS setting changes.
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    task::JoinHandle,
    time::timeout,
};

pub struct CommitProxy {
    pub url: String,
    observed: Option<oneshot::Receiver<()>>,
    release: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}
impl CommitProxy {
    pub async fn start(schema_url: &str) -> Self {
        Self::start_target(schema_url, 1).await
    }

    /// Withhold the selected successful COMMIT acknowledgement on this
    /// loopback-only connection. `1` is the allowance transaction; `2` is the
    /// subsequent creation transaction for a real createLink request.
    pub async fn start_target(schema_url: &str, target_commit: usize) -> Self {
        assert!(target_commit > 0);
        let mut url = url::Url::parse(schema_url).unwrap();
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert!(url.query_pairs().any(
            |(key, value)| key == "options[search_path]" && value.starts_with("ferl_stage01_")
        ));
        let upstream = format!("127.0.0.1:{}", url.port().unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        url.set_port(Some(listener.local_addr().unwrap().port()))
            .unwrap();
        // Only this loopback, fixture-owned fault connection uses cleartext so
        // the test can withhold a protocol acknowledgement after durable commit.
        url.query_pairs_mut().append_pair("sslmode", "disable");
        let (observed_tx, observed) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            timeout(Duration::from_secs(20), async move {
                let (client, _) = listener.accept().await.unwrap();
                let upstream = TcpStream::connect(upstream).await.unwrap();
                let (mut client_read, mut client_write) = client.into_split();
                let (mut server_read, mut server_write) = upstream.into_split();
                let frontend = async { let _ = tokio::io::copy(&mut client_read, &mut server_write).await; };
                let backend = async {
                    let mut successful_commits = 0;
                    loop {
                        let mut header = [0u8; 5];
                        server_read.read_exact(&mut header).await.unwrap();
                        let length = u32::from_be_bytes(header[1..5].try_into().unwrap()) as usize;
                        assert!((4..=1_048_576).contains(&length), "bounded backend protocol frame");
                        let mut payload = vec![0; length - 4];
                        server_read.read_exact(&mut payload).await.unwrap();
                        if header[0] == b'C' && payload == b"COMMIT\0" {
                            successful_commits += 1;
                            if successful_commits == target_commit {
                                observed_tx.send(()).unwrap();
                                // Parent independently reads the durable row before
                                // allowing this connection to close without delivery.
                                let _ = release_rx.await;
                                client_write.shutdown().await.unwrap();
                                break;
                            }
                        }
                        client_write.write_all(&header).await.unwrap();
                        client_write.write_all(&payload).await.unwrap();
                    }
                };
                tokio::select! { _ = frontend => panic!("client closed before COMMIT observation"), _ = backend => {} }
            }).await.expect("commit fault proxy has a hard lifetime bound");
        });
        Self {
            url: url.to_string(),
            observed: Some(observed),
            release: Some(release),
            task,
        }
    }
    pub async fn committed(&mut self) {
        timeout(Duration::from_secs(8), self.observed.take().unwrap())
            .await
            .unwrap()
            .unwrap();
    }
    pub async fn disconnect(&mut self) {
        self.release.take().unwrap().send(()).unwrap();
        timeout(Duration::from_secs(3), &mut self.task)
            .await
            .unwrap()
            .unwrap();
    }
}
impl Drop for CommitProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}
