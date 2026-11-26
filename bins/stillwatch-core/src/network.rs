use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::{Extension, Router};
use hyper::{
    rt::{Read, Write},
    server::conn::http1,
};
use hyper_util::{
    rt::{TokioIo, TokioTimer},
    service::TowerToHyperService,
};
use tokio::{
    net::TcpListener,
    sync::{Semaphore, watch},
    task::JoinSet,
};
use tokio_rustls::TlsAcceptor;

pub async fn serve(
    listener: TcpListener,
    router: Router,
    tls: Option<TlsAcceptor>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut children = JoinSet::new();
    let slots = Arc::new(Semaphore::new(96));
    while !*shutdown.borrow() {
        tokio::select! {
            _ = async { let _ = shutdown.wait_for(|stop| *stop).await; } => break,
            _ = children.join_next(), if !children.is_empty() => {},
            result = listener.accept() => {
                let Ok((stream, peer)) = result else {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                };
                let Ok(permit) = slots.clone().try_acquire_owned() else { continue; };
                let acceptor = tls.clone();
                let router = router.clone();
                let mut stopping = shutdown.clone();
                children.spawn(async move {
                    let _permit = permit;
                    match acceptor {
                        Some(acceptor) => {
                            let tls = tokio::select! {
                                _ = stopping.wait_for(|stop| *stop) => return,
                                result = tokio::time::timeout(Duration::from_secs(4), acceptor.accept(stream)) => result,
                            };
                            if let Ok(Ok(stream)) = tls {
                                serve_http(TokioIo::new(stream), router, peer, stopping).await;
                            }
                        }
                        None => serve_http(TokioIo::new(stream), router, peer, stopping).await,
                    }
                });
            }
        }
    }
    if tokio::time::timeout(Duration::from_secs(3), async {
        while children.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        children.abort_all();
    }
}

async fn serve_http(
    stream: impl Read + Write + Unpin,
    router: Router,
    peer: SocketAddr,
    mut stopping: watch::Receiver<bool>,
) {
    let service = router.layer(Extension(peer));
    let mut builder = http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(Duration::from_secs(5))
        .max_headers(48);
    let connection = builder.serve_connection(stream, TowerToHyperService::new(service));
    tokio::pin!(connection);
    tokio::select! {
        _ = &mut connection => {},
        _ = async { let _ = stopping.wait_for(|stop| *stop).await; } => {
            connection.as_mut().graceful_shutdown();
            let _ = tokio::time::timeout(Duration::from_secs(2), &mut connection).await;
        }
    }
}
