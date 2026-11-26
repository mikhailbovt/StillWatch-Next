use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use stillwatch_sdk::{Client, CredentialFuture, Credentials, Error, protocol::ProblemCode};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
};

#[derive(Default)]
struct RotatingTokens(AtomicUsize);

impl Credentials for RotatingTokens {
    fn token(&self) -> CredentialFuture<'_> {
        let generation = self.0.fetch_add(1, Ordering::Relaxed);
        Box::pin(std::future::ready(Ok(format!("{generation:064x}"))))
    }
}

async fn respond(listener: &TcpListener, status: &str, body: &str) -> std::io::Result<bool> {
    let (mut socket, _) = listener.accept().await?;
    let mut headers = BufReader::new(&mut socket).lines();
    let mut authorized = false;
    while let Some(line) = headers.next_line().await? {
        if line.is_empty() {
            break;
        }
        if let Some((name, _)) = line.split_once(':') {
            authorized |= name.eq_ignore_ascii_case("authorization");
        }
    }
    let challenge = if status.starts_with("401 ") {
        "WWW-Authenticate: Bearer\r\nContent-Type: application/problem+json\r\n"
    } else {
        ""
    };
    let response = format!(
        "HTTP/1.1 {status}\r\n{challenge}Connection: close\r\nRetry-After: 0\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(response.as_bytes()).await?;
    Ok(authorized)
}

#[tokio::test]
async fn final_unauthorized_response_preserves_the_problem_and_credential() -> anyhow::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let tokens = Arc::new(RotatingTokens::default());
    let client = Client::loopback(&endpoint, tokens.clone())?;
    let serve = async {
        let problem =
            r#"{"type":"about:blank","title":"Unauthorized","status":401,"code":"unauthorized"}"#;
        assert!(respond(&listener, "503 Unavailable", "").await?);
        assert!(respond(&listener, "503 Unavailable", "").await?);
        assert!(respond(&listener, "401 Unauthorized", problem).await?);
        Ok::<_, std::io::Error>(())
    };
    let (served, result) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(serve, client.status())
    })
    .await?;
    served?;
    let error = result.expect_err("the server rejected the third request");
    assert!(matches!(error, Error::Http { status: 401, .. }), "{error}");
    assert_eq!(error.problem().unwrap().code, ProblemCode::Unauthorized);
    assert_eq!(tokens.0.load(Ordering::Relaxed), 1);
    Ok(())
}

#[cfg(feature = "blocking")]
#[tokio::test]
async fn blocking_bootstrap_is_anonymous_and_checks_api_version() -> anyhow::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let read = tokio::task::spawn_blocking(move || {
        stillwatch_sdk::blocking::Client::loopback(&endpoint, "ab".repeat(32))?.capabilities()
    });
    let body = r#"{"instance_id":"d65dc84e-e691-485e-aad3-00383fe24d4c","version":{"major":999,"minor":0}}"#;
    let (served, result) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(respond(&listener, "200 OK", body), read)
    })
    .await?;
    let carried_bearer = served?;
    let capabilities = result?;
    assert!(
        matches!(capabilities, Err(Error::Identity)) && !carried_bearer,
        "accepted_incompatible={} carried_bearer={carried_bearer}",
        capabilities.is_ok()
    );
    Ok(())
}
