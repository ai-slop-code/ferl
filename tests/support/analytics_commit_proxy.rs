//! Loopback-only PostgreSQL fault proxy that commits the first transaction but
//! withholds its successful acknowledgement, then transparently forwards the
//! worker's idempotent retry. It never changes production TLS configuration.

use std::time::Duration;

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    task::JoinHandle,
    time::timeout,
};

pub struct AnalyticsCommitProxy {
    pub url: String,
    observed: Option<oneshot::Receiver<()>>,
    release: Option<oneshot::Sender<()>>,
    retry_connected: Option<oneshot::Receiver<()>>,
    task: Option<JoinHandle<()>>,
}

impl AnalyticsCommitProxy {
    pub async fn start(schema_url: &str) -> Self {
        let mut url = url::Url::parse(schema_url).unwrap();
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert!(url.query_pairs().any(|(key, value)| {
            key == "options[search_path]" && value.starts_with("ferl_stage01_")
        }));
        let upstream_addr = format!("127.0.0.1:{}", url.port().unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        url.set_port(Some(listener.local_addr().unwrap().port()))
            .unwrap();
        // Only this fixture-owned loopback protocol proxy uses cleartext so it
        // can withhold the PostgreSQL COMMIT acknowledgement. Never deploy this URL.
        url.query_pairs_mut().append_pair("sslmode", "disable");
        let (observed_tx, observed) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let (retry_tx, retry_connected) = oneshot::channel();
        let task = tokio::spawn(async move {
            timeout(Duration::from_secs(30), async move {
                let (client, _) = listener.accept().await.unwrap();
                let upstream = TcpStream::connect(&upstream_addr).await.unwrap();
                let (mut client_read, mut client_write) = client.into_split();
                let (mut server_read, mut server_write) = upstream.into_split();
                let frontend = async {
                    let _ = tokio::io::copy(&mut client_read, &mut server_write).await;
                };
                let backend = async {
                    loop {
                        let mut header = [0_u8; 5];
                        server_read.read_exact(&mut header).await.unwrap();
                        let length = u32::from_be_bytes(header[1..5].try_into().unwrap()) as usize;
                        assert!((4..=1_048_576).contains(&length));
                        let mut payload = vec![0; length - 4];
                        server_read.read_exact(&mut payload).await.unwrap();
                        if header[0] == b'C' && payload == b"COMMIT\0" {
                            observed_tx.send(()).unwrap();
                            let _ = release_rx.await;
                            client_write.shutdown().await.unwrap();
                            return;
                        }
                        client_write.write_all(&header).await.unwrap();
                        client_write.write_all(&payload).await.unwrap();
                    }
                };
                tokio::select! {
                    biased;
                    _ = backend => {},
                    _ = frontend => panic!("client closed before visit COMMIT was intercepted"),
                }

                // SQLx closes the uncertain connection. Accept exactly the
                // retry connection and transparently relay it until pool close.
                let (mut retry_client, _) = listener.accept().await.unwrap();
                retry_tx.send(()).unwrap();
                let mut retry_upstream = TcpStream::connect(&upstream_addr).await.unwrap();
                let _ = tokio::io::copy_bidirectional(&mut retry_client, &mut retry_upstream).await;
            })
            .await
            .expect("visit commit proxy has a hard lifetime bound");
        });
        Self {
            url: url.to_string(),
            observed: Some(observed),
            release: Some(release),
            retry_connected: Some(retry_connected),
            task: Some(task),
        }
    }

    pub async fn committed(&mut self) {
        timeout(Duration::from_secs(10), self.observed.take().unwrap())
            .await
            .unwrap()
            .unwrap();
    }

    pub fn release_first_commit(&mut self) {
        self.release.take().unwrap().send(()).unwrap();
    }

    pub async fn retry_connected(&mut self) {
        timeout(
            Duration::from_secs(10),
            self.retry_connected.take().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    }

    pub async fn stop(&mut self) {
        if let Some(mut task) = self.task.take() {
            task.abort();
            let _ = timeout(Duration::from_secs(2), &mut task).await;
        }
    }
}

impl Drop for AnalyticsCommitProxy {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
