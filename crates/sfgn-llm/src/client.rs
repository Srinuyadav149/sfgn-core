use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Instant;

use crate::json::{escape_json_str, parse_json};
use crate::stream::StreamDemuxer;
use crate::{
    ChatMessage, LlmConfig, LlmError, Result, TokenUsage, TurnResponse, TurnStreamEvent,
};

/// Streaming `/v1/chat/completions` client over HTTP/1.1 SSE.
#[derive(Debug, Clone)]
pub struct LlmClient {
    config: LlmConfig,
}

impl LlmClient {
    pub fn new(config: LlmConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &LlmConfig {
        &self.config
    }

    /// Streams a chat completion turn, invoking `on_event` for each incremental thinking or
    /// content token and returning the demuxed [`TurnResponse`] with token usage stats.
    pub fn stream_chat<F>(
        &self,
        messages: &[ChatMessage],
        grammar: Option<&str>,
        mut on_event: F,
    ) -> Result<TurnResponse>
    where
        F: FnMut(TurnStreamEvent<'_>),
    {
        let parsed_url = ParsedHttpUrl::parse(&self.config.endpoint_url)?;
        let body_json = self.build_request_json(messages, grammar);

        let t0 = Instant::now();
        let socket_addr = format!("{}:{}", parsed_url.host, parsed_url.port)
            .to_socket_addrs()
            .map_err(LlmError::Io)?
            .next()
            .ok_or_else(|| {
                LlmError::InvalidEndpoint(format!(
                    "could not resolve host '{}:{}'",
                    parsed_url.host, parsed_url.port
                ))
            })?;

        let mut stream = TcpStream::connect_timeout(&socket_addr, self.config.connect_timeout)?;
        stream.set_read_timeout(Some(self.config.read_timeout))?;
        stream.set_write_timeout(Some(self.config.connect_timeout))?;
        let _ = stream.set_nodelay(true);

        let mut request_head = String::with_capacity(512);
        let _ = write!(
            request_head,
            "POST {} HTTP/1.1\r\n\
             Host: {}\r\n\
             Content-Type: application/json\r\n\
             Accept: text/event-stream\r\n\
             Connection: close\r\n\
             Content-Length: {}\r\n\r\n",
            parsed_url.path,
            parsed_url.host_header(),
            body_json.len()
        );

        stream.write_all(request_head.as_bytes())?;
        stream.write_all(body_json.as_bytes())?;
        stream.flush()?;

        let mut reader = BufReader::new(stream);
        let (status_code, is_chunked) = read_http_response_head(&mut reader)?;

        if !(200..300).contains(&status_code) {
            let mut err_body = String::new();
            let mut body_reader = HttpBodyReader::new(reader, is_chunked);
            let _ = body_reader.read_to_string(&mut err_body);
            return Err(LlmError::HttpStatus {
                status: status_code,
                body: err_body,
            });
        }

        let mut sse_reader = BufReader::new(HttpBodyReader::new(reader, is_chunked));
        let mut demux = StreamDemuxer::new();
        let mut first_token_at: Option<Instant> = None;
        let mut line_buf = String::new();

        loop {
            line_buf.clear();
            let bytes_read = sse_reader.read_line(&mut line_buf)?;
            if bytes_read == 0 {
                break;
            }

            let trimmed = line_buf.trim();
            if trimmed.is_empty() || trimmed.starts_with(':') {
                continue;
            }
            let Some(data_payload) = trimmed.strip_prefix("data:") else {
                continue;
            };
            let data_str = data_payload.trim();
            if data_str == "[DONE]" {
                break;
            }

            let chunk = parse_json(data_str)?;

            if let Some(usage_obj) = chunk.get("usage")
                && !usage_obj.is_null()
            {
                let prompt_tokens = usage_obj
                    .get("prompt_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let completion_tokens = usage_obj
                    .get("completion_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let total_tokens = usage_obj
                    .get("total_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(prompt_tokens + completion_tokens);
                let cached_tokens = usage_obj
                    .get("prompt_tokens_details")
                    .and_then(|d| d.get("cached_tokens"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);

                demux.set_usage(TokenUsage {
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                    cached_tokens,
                });
            }

            let Some(choice0) = chunk.get("choices").and_then(|c| c.index(0)) else {
                continue;
            };

            if let Some(reason) = choice0.get("finish_reason").and_then(|v| v.as_str()) {
                demux.set_finish_reason(reason);
            }

            let Some(delta) = choice0.get("delta") else {
                continue;
            };

            if let Some(r_tok) = delta.get("reasoning_content").and_then(|v| v.as_str())
                && !r_tok.is_empty()
            {
                if first_token_at.is_none() {
                    first_token_at = Some(Instant::now());
                }
                demux.push_reasoning_delta(r_tok, &mut on_event);
            }

            if let Some(c_tok) = delta.get("content").and_then(|v| v.as_str())
                && !c_tok.is_empty()
            {
                if first_token_at.is_none() {
                    first_token_at = Some(Instant::now());
                }
                demux.push_content_delta(c_tok, &mut on_event);
            }
        }

        let elapsed = t0.elapsed();
        let ttft = first_token_at.map(|t| t.duration_since(t0)).unwrap_or(elapsed);
        Ok(demux.finish(ttft, elapsed, &mut on_event))
    }

    fn build_request_json(&self, messages: &[ChatMessage], grammar: Option<&str>) -> String {
        let mut out = String::with_capacity(2048);
        out.push('{');

        if let Some(model) = &self.config.model {
            let _ = write!(out, "\"model\":{},", escape_json_str(model));
        }

        out.push_str("\"messages\":[");
        for (idx, msg) in messages.iter().enumerate() {
            if idx > 0 {
                out.push(',');
            }
            let _ = write!(
                out,
                "{{\"role\":{},\"content\":{}}}",
                escape_json_str(msg.role.as_str()),
                escape_json_str(&msg.content)
            );
        }
        out.push_str("],");

        let _ = write!(
            out,
            "\"temperature\":{:.3},\
             \"repeat_penalty\":{:.3},\
             \"max_tokens\":{},\
             \"cache_prompt\":{},\
             \"stream\":true,\
             \"stream_options\":{{\"include_usage\":true}}",
            self.config.temperature,
            self.config.repeat_penalty,
            self.config.max_tokens,
            self.config.cache_prompt
        );

        if self.config.use_grammar
            && let Some(gbnf) = grammar
            && !gbnf.trim().is_empty()
        {
            let _ = write!(out, ",\"grammar\":{}", escape_json_str(gbnf));
        }

        out.push('}');
        out
    }
}

struct ParsedHttpUrl {
    host: String,
    port: u16,
    path: String,
}

impl ParsedHttpUrl {
    fn parse(raw: &str) -> Result<Self> {
        let trimmed = raw.trim();
        if trimmed.starts_with("https://") {
            return Err(LlmError::InvalidEndpoint(
                "https:// is not supported by built-in TCP client; point endpoint_url to local http://127.0.0.1:9931/v1/chat/completions".to_owned(),
            ));
        }
        let rest = trimmed.strip_prefix("http://").ok_or_else(|| {
            LlmError::InvalidEndpoint(format!("endpoint must start with http:// (got '{}')", raw))
        })?;

        let (authority, path) = match rest.find('/') {
            Some(slash_idx) => (&rest[..slash_idx], &rest[slash_idx..]),
            None => (rest, "/v1/chat/completions"),
        };

        if authority.is_empty() {
            return Err(LlmError::InvalidEndpoint("missing host in URL".to_owned()));
        }

        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if !h.is_empty() => {
                let parsed_port = p.parse::<u16>().map_err(|_| {
                    LlmError::InvalidEndpoint(format!("invalid port '{}' in URL", p))
                })?;
                (h.to_owned(), parsed_port)
            }
            _ => (authority.to_owned(), 80),
        };

        Ok(Self {
            host,
            port,
            path: path.to_owned(),
        })
    }

    fn host_header(&self) -> String {
        if self.port == 80 {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

fn read_http_response_head<R: BufRead>(reader: &mut R) -> Result<(u16, bool)> {
    let mut status_line = String::new();
    let n = reader.read_line(&mut status_line)?;
    if n == 0 {
        return Err(LlmError::Protocol(
            "LLM server closed connection before sending HTTP status line".to_owned(),
        ));
    }

    let mut parts = status_line.split_whitespace();
    let proto = parts.next().unwrap_or("");
    if !proto.starts_with("HTTP/1.") {
        return Err(LlmError::Protocol(format!(
            "unexpected HTTP response line: '{}'",
            status_line.trim()
        )));
    }
    let status_code = parts
        .next()
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| {
            LlmError::Protocol(format!(
                "missing numeric HTTP status in '{}'",
                status_line.trim()
            ))
        })?;

    let mut is_chunked = false;
    let mut header_line = String::new();
    loop {
        header_line.clear();
        let read = reader.read_line(&mut header_line)?;
        if read == 0 {
            break;
        }
        let trimmed = header_line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, val)) = trimmed.split_once(':')
            && name.trim().eq_ignore_ascii_case("transfer-encoding")
            && val.to_ascii_lowercase().contains("chunked")
        {
            is_chunked = true;
        }
    }

    Ok((status_code, is_chunked))
}

/// Decodes `Transfer-Encoding: chunked` seamlessly if enabled, or passes bytes through verbatim.
struct HttpBodyReader<R: BufRead> {
    inner: R,
    chunked: bool,
    remaining_in_chunk: usize,
    eof: bool,
}

impl<R: BufRead> HttpBodyReader<R> {
    fn new(inner: R, chunked: bool) -> Self {
        Self {
            inner,
            chunked,
            remaining_in_chunk: 0,
            eof: false,
        }
    }
}

impl<R: BufRead> Read for HttpBodyReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.eof || buf.is_empty() {
            return Ok(0);
        }
        if !self.chunked {
            return self.inner.read(buf);
        }

        if self.remaining_in_chunk == 0 {
            let mut size_line = String::new();
            loop {
                size_line.clear();
                let n = self.inner.read_line(&mut size_line)?;
                if n == 0 {
                    self.eof = true;
                    return Ok(0);
                }
                let trimmed = size_line.trim();
                if !trimmed.is_empty() {
                    break;
                }
            }
            let hex_part = size_line.trim().split(';').next().unwrap_or("").trim();
            let chunk_size = usize::from_str_radix(hex_part, 16).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid HTTP chunk size '{}'", hex_part),
                )
            })?;
            if chunk_size == 0 {
                self.eof = true;
                return Ok(0);
            }
            self.remaining_in_chunk = chunk_size;
        }

        let to_read = buf.len().min(self.remaining_in_chunk);
        let n = self.inner.read(&mut buf[..to_read])?;
        if n == 0 {
            self.eof = true;
            return Ok(0);
        }
        self.remaining_in_chunk -= n;
        if self.remaining_in_chunk == 0 {
            // Consume trailing CRLF after each chunk
            let mut crlf = [0u8; 2];
            let _ = self.inner.read_exact(&mut crlf);
        }
        Ok(n)
    }
}

