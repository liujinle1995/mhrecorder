//! 管道传输层。
//!
//! 与分发逻辑分开，这样协议可以在任意 `Read`/`Write` 上测。
//!
//! 关键约束：**一行最多 [`ServeOptions::max_line_bytes` 字节**。老实现用
//! `read_line`，它会把缓冲区一直撑大直到出现换行——客户端一个 bug 就能让
//! 服务端吃掉几个 GB。这里改成有界读取：超过上限直接把这一行读到换行处丢弃，
//! 回一条错误，连接照常可用。

use std::io::{BufRead, BufReader, BufWriter, Write};

use mhcore::protocol::{errcode, Response};

/// Default pipe name. The C# client connects to `\\.\pipe\mhrecorder.v1`.
pub const PIPE_NAME: &str = "mhrecorder.v1";

/// Serialize one request/response as a single line. Embedded newlines inside
/// JSON strings are escaped by serde, so a trailing `\n` is a safe delimiter.
pub fn encode_line<T: serde::Serialize>(value: &T) -> std::io::Result<String> {
    let mut s = serde_json::to_string(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    s.push('\n');
    Ok(s)
}

/// 一次连接的服务参数。
#[derive(Debug, Clone, Copy)]
pub struct ServeOptions {
    /// 单行内容上限（不含换行符）。超过即拒绝。
    pub max_line_bytes: usize,
    /// 超过上限后是否断开连接。
    ///
    /// 默认断开：客户端读到 EOF 会立刻失败并重连，用户看到的是"连接断开"
    /// 而不是干等 30 秒超时。调试时可以改成 false 继续服务。
    pub close_on_oversize: bool,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self { max_line_bytes: 32 * 1024 * 1024, close_on_oversize: true }
    }
}

impl ServeOptions {
    /// 只改上限，保留默认的"超限即断连"。
    #[allow(dead_code)]
    pub fn with_limit(max_line_bytes: usize) -> Self {
        Self { max_line_bytes, close_on_oversize: true }
    }
}

/// 读取结果。
enum ReadOutcome {
    Line,
    /// 这一行超过了上限，已被丢弃（读到换行符为止）。
    Oversize,
    Eof,
}

/// Serve the line protocol over an arbitrary transport.
///
/// Reads one JSON request per line and writes one JSON response per line until
/// the peer closes the connection or `handler` returns `None`.
pub fn serve_stream<R, W, F>(
    reader: R,
    mut writer: W,
    opts: ServeOptions,
    mut handler: F,
) -> std::io::Result<()>
where
    R: std::io::Read,
    W: std::io::Write,
    F: FnMut(&str) -> Option<String>,
{
    let mut buf = BufReader::new(reader);
    let mut writer = BufWriter::new(&mut writer);
    let mut line: Vec<u8> = Vec::with_capacity(8 * 1024);

    loop {
        line.clear();
        match read_line_bounded(&mut buf, &mut line, opts.max_line_bytes)? {
            ReadOutcome::Eof => break,
            ReadOutcome::Oversize => {
                tracing::warn!(
                    "丢弃超限请求：超过 {} 字节上限",
                    opts.max_line_bytes
                );
                let resp = Response::err(
                    0,
                    errcode::BAD_REQUEST,
                    format!(
                        "请求体超过上限（{} MB），请减少图片数量或分辨率",
                        opts.max_line_bytes / (1024 * 1024)
                    ),
                );
                if let Ok(text) = encode_line(&resp) {
                    writer.write_all(text.as_bytes())?;
                    writer.flush()?;
                }
                if opts.close_on_oversize {
                    break;
                }
                continue;
            }
            ReadOutcome::Line => {}
        }

        let text = match std::str::from_utf8(&line) {
            Ok(t) => t,
            Err(e) => {
                let resp = Response::err(
                    0,
                    errcode::BAD_REQUEST,
                    format!("请求不是合法的 UTF-8：{e}"),
                );
                if let Ok(text) = encode_line(&resp) {
                    writer.write_all(text.as_bytes())?;
                    writer.flush()?;
                }
                continue;
            }
        };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }

        let Some(response) = handler(trimmed) else {
            break;
        };
        writer.write_all(response.as_bytes())?;
        writer.flush()?;
    }
    Ok(())
}

/// 有界读一行：最多读 `max` 字节，超出部分一直丢到换行符。
///
/// 不用 `read_line` 的原因就是它没有上限——"先把整行读完再判断长短"本身
/// 就是我们要避免的那次分配。
fn read_line_bounded<R: BufRead>(
    r: &mut R,
    out: &mut Vec<u8>,
    max: usize,
) -> std::io::Result<ReadOutcome> {
    let mut discarding = false;
    loop {
        // 先算出"这次消费多少字节"，出了借用作用域再 consume。
        let (consume, done, oversize) = {
            let buf = r.fill_buf()?;
            if buf.is_empty() {
                // EOF：手里还有半行就是最后一行（没有换行结尾也算）。
                return Ok(if discarding {
                    ReadOutcome::Oversize
                } else if out.is_empty() {
                    ReadOutcome::Eof
                } else {
                    ReadOutcome::Line
                });
            }
            match buf.iter().position(|&b| b == b'\n') {
                Some(pos) => {
                    let mut over = false;
                    if !discarding {
                        if out.len() + pos > max {
                            over = true;
                            out.clear();
                            discarding = true;
                        } else {
                            out.extend_from_slice(&buf[..pos]);
                        }
                    }
                    (pos + 1, true, over)
                }
                None => {
                    let mut over = false;
                    if !discarding {
                        if out.len() + buf.len() > max {
                            over = true;
                            out.clear();
                            discarding = true;
                        } else {
                            out.extend_from_slice(buf);
                        }
                    }
                    (buf.len(), false, over)
                }
            }
        };
        r.consume(consume);
        if done {
            return Ok(if oversize { ReadOutcome::Oversize } else { ReadOutcome::Line });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serve(input: &[u8], max: usize) -> String {
        let mut out = Vec::new();
        serve_stream(
            input,
            &mut out,
            ServeOptions::with_limit(max),
            |line| Some(format!("echo:{line}\n")),
        )
        .unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn encode_line_is_single_line() {
        let v = serde_json::json!({"a":"x\ny"});
        let s = encode_line(&v).unwrap();
        assert_eq!(s.matches('\n').count(), 1, "only the terminator newline");
        assert!(s.ends_with('\n'));
    }

    #[test]
    fn serve_stream_handles_multiple_lines_then_stops() {
        let text = serve(&b"{\"id\":1}\n{\"id\":2}\n"[..], 1024);
        assert!(text.contains("echo:{\"id\":1}"));
        assert!(text.contains("echo:{\"id\":2}"));
    }

    #[test]
    fn serve_stream_stops_when_handler_returns_none() {
        let mut out = Vec::new();
        serve_stream(&b"{\"id\":1}\n{\"id\":2}\n"[..], &mut out, ServeOptions::default(), |_| None)
            .unwrap();
        assert!(out.is_empty(), "handler veto must end the session immediately");
    }

    #[test]
    fn serve_stream_ignores_blank_lines() {
        assert_eq!(serve(&b"\n  \n{\"id\":1}\n"[..], 1024), "echo:{\"id\":1}\n");
    }

    /// 超限的行被整行丢弃并回一条错误，**下一行照常处理**（连接不断）。
    #[test]
    fn oversized_line_is_rejected_without_killing_the_session() {
        let mut out = Vec::new();
        let opts = ServeOptions { max_line_bytes: 32, close_on_oversize: false };
        let long = format!("{{\"pad\":\"{}\"}}", "x".repeat(4096));
        let input = format!("{long}\n{{\"id\":2}}\n");
        serve_stream(input.as_bytes(), &mut out, opts, |line| Some(format!("echo:{line}\n")))
            .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("请求体超过上限"), "{text}");
        assert!(text.contains("echo:{\"id\":2}"), "下一行必须继续被处理：{text}");
    }

    #[test]
    fn oversized_line_closes_the_connection_by_default() {
        let long = format!("{{\"pad\":\"{}\"}}", "x".repeat(4096));
        let input = format!("{long}\n{{\"id\":2}}\n");
        let out = serve(input.as_bytes(), 32);
        assert!(out.contains("请求体超过上限"));
        assert!(!out.contains("echo:"), "默认断连，后续请求不再处理：{out}");
    }

    /// 边界：内容长度正好等于上限要通过，多一个字节就拒绝（换行符不计）。
    #[test]
    fn limit_is_inclusive_at_the_boundary() {
        let exact = "a".repeat(32);
        assert_eq!(serve(format!("{exact}\n").as_bytes(), 32), format!("echo:{exact}\n"));

        let over = "a".repeat(33);
        assert!(serve(format!("{over}\n").as_bytes(), 32).contains("请求体超过上限"));
    }

    #[test]
    fn invalid_utf8_is_reported_and_skipped() {
        let mut input = vec![0xFF, 0xFE, b'\n'];
        input.extend_from_slice(b"{\"id\":3}\n");
        let mut out = Vec::new();
        let opts = ServeOptions { max_line_bytes: 1024, close_on_oversize: false };
        serve_stream(input.as_slice(), &mut out, opts, |line| Some(format!("echo:{line}\n")))
            .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("UTF-8"), "{text}");
        assert!(text.contains("echo:{\"id\":3}"));
    }

    #[test]
    fn line_without_trailing_newline_is_still_served() {
        assert_eq!(serve(&b"{\"id\":9}"[..], 1024), "echo:{\"id\":9}\n");
    }
}
