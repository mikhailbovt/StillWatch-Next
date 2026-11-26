use std::{pin::Pin, time::Duration};

use futures_core::Stream;
use reqwest::Method;
use sw_protocol::{Change, Cursor, Problem, ProblemCode};

use crate::{Client, Error, check};

const MAX_EVENT_BYTES: usize = 64 * 1024;
pub type ChangeStream = Pin<Box<dyn Stream<Item = Result<StreamMessage, Error>> + Send>>;

#[derive(Debug)]
pub enum StreamMessage {
    Change(Box<Change>),
    Resync(Problem),
}

impl Client {
    /// Keep the last accepted cursor outside this stream. Dropping it cancels
    /// transport. Within a stream, a yielded change becomes the resume cursor
    /// when the caller polls for the next item.
    pub fn changes(&self, mut cursor: Cursor) -> ChangeStream {
        let client = self.clone();
        Box::pin(async_stream::stream! {
            let mut attempt = 0u32;
            loop {
                let response = client.connect_changes(cursor).await;
                let mut response = match response {
                    Ok(response) => response,
                    Err(error) if reconnectable(&error) => {
                        yield Err(error);
                        backoff(attempt).await;
                        attempt = attempt.saturating_add(1);
                        continue;
                    }
                    Err(error) => {
                        yield Err(error);
                        break;
                    }
                };
                let mut decoder = Decoder::default();
                loop {
                    let chunk = match response.chunk().await {
                        Ok(Some(chunk)) => chunk,
                        Ok(None) => break,
                        Err(error) => { yield Err(Error::transport(error)); break; }
                    };
                    for byte in chunk {
                        let raw = match decoder.feed(byte) {
                            Ok(Some(event)) => event,
                            Ok(None) => continue,
                            Err(error) => { yield Err(error); return; }
                        };
                        match raw.event.as_str() {
                            "incident.opened" | "incident.updated" => {
                                let Ok(change) = serde_json::from_slice::<Change>(&raw.data) else {
                                    yield Err(Error::Protocol);
                                    return;
                                };
                                let next = change.cursor();
                                if raw.id.as_deref() != Some(next.to_string().as_str()) || next.generation != cursor.generation {
                                    yield Err(Error::Protocol);
                                    return;
                                }
                                if next.sequence > cursor.sequence {
                                    yield Ok(StreamMessage::Change(Box::new(change)));
                                    cursor = next;
                                    attempt = 0;
                                }
                            }
                            "resync" => {
                                let result = serde_json::from_slice::<Problem>(&raw.data)
                                    .map(StreamMessage::Resync).map_err(|_| Error::Protocol);
                                yield result;
                                return;
                            }
                            _ => {}
                        }
                    }
                }
                backoff(attempt).await;
                attempt = attempt.saturating_add(1);
            }
        })
    }

    async fn connect_changes(&self, cursor: Cursor) -> Result<reqwest::Response, Error> {
        let credentials = self.credentials.as_ref().ok_or(Error::Credentials)?;
        let token = credentials.token().await?;
        let request = |token: &str| {
            if !super::valid_token(token) {
                return Err(Error::Credentials);
            }
            Ok(self
                .request(Method::GET, "/api/v2/changes")?
                .bearer_auth(token)
                .header("Accept", "text/event-stream")
                .header("Last-Event-ID", cursor.to_string()))
        };
        let connect = |request: reqwest::RequestBuilder| async {
            tokio::time::timeout(Duration::from_secs(6), request.send())
                .await
                .map_err(|_| Error::Deadline)?
                .map_err(Error::transport)
        };
        let mut response = connect(request(&token)?).await?;
        if response.status().as_u16() == 401 {
            let replacement = credentials.renew(&token).await?;
            if replacement != token {
                response = connect(request(&replacement)?).await?;
            }
        }
        let response = check(response).await?;
        if !response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.split(';').next() == Some("text/event-stream"))
        {
            return Err(Error::Protocol);
        }
        Ok(response)
    }
}

fn reconnectable(error: &Error) -> bool {
    match error {
        Error::Transport | Error::Deadline => true,
        Error::Http { status, problem } => {
            matches!(status, 429 | 502 | 503 | 504)
                && !problem
                    .as_ref()
                    .is_some_and(|problem| problem.code == ProblemCode::CursorExpired)
        }
        _ => false,
    }
}

async fn backoff(attempt: u32) {
    let mut random = [0u8; 1];
    let _ = getrandom::fill(&mut random);
    let millis = (250u64 << attempt.min(6)) + u64::from(random[0]);
    tokio::time::sleep(Duration::from_millis(millis)).await;
}

#[derive(Default)]
struct Decoder {
    line: Vec<u8>,
    event: Raw,
    after_cr: bool,
}

#[derive(Default)]
struct Raw {
    event: String,
    id: Option<String>,
    data: Vec<u8>,
}

impl Decoder {
    fn feed(&mut self, byte: u8) -> Result<Option<Raw>, Error> {
        if std::mem::take(&mut self.after_cr) && byte == b'\n' {
            return Ok(None);
        }
        if byte != b'\r' && byte != b'\n' {
            if self.line.len() == MAX_EVENT_BYTES {
                return Err(Error::Protocol);
            }
            self.line.push(byte);
            return Ok(None);
        }
        self.after_cr = byte == b'\r';
        if self.line.is_empty() {
            let mut event = std::mem::take(&mut self.event);
            if event.data.pop().is_some() {
                return Ok(Some(event));
            }
            return Ok(None);
        }
        let line = std::str::from_utf8(&self.line).map_err(|_| Error::Protocol)?;
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" if value.len() <= 64 => self.event.event = value.to_owned(),
            "id" if value.len() <= 100 && !value.contains('\0') => {
                self.event.id = Some(value.to_owned())
            }
            "data" => {
                if self.event.data.len() + value.len() + 1 > MAX_EVENT_BYTES {
                    return Err(Error::Protocol);
                }
                self.event.data.extend_from_slice(value.as_bytes());
                self.event.data.push(b'\n');
            }
            _ => {}
        }
        self.line.clear();
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_sse_lines_and_preserves_multiline_utf8() {
        let fixture = b": ping\r\nid: x\r\nevent: value\ndata: \xd1\x8f\ndata: second\r\n\r\n";
        let mut decoder = Decoder::default();
        let events: Vec<_> = fixture
            .iter()
            .filter_map(|&byte| decoder.feed(byte).unwrap())
            .collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "value");
        assert_eq!(events[0].id.as_deref(), Some("x"));
        assert_eq!(events[0].data, "я\nsecond".as_bytes());
    }
}
