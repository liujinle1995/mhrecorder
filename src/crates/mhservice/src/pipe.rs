//! Windows named-pipe transport.
//!
//! Deliberately kept separate from the dispatch logic so the protocol layer can
//! be exercised over any `Read`/`Write` pair in tests.

use std::io::{BufRead, BufReader};

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

/// Serve the line protocol over an arbitrary transport.
///
/// Reads one JSON request per line and writes one JSON response per line until
/// the peer closes the connection or `handler` returns `false`.
pub fn serve_stream<R, W, F>(reader: R, mut writer: W, mut handler: F) -> std::io::Result<()>
where
    R: std::io::Read,
    W: std::io::Write,
    F: FnMut(&str) -> Option<String>,
{
    let mut buf = BufReader::new(reader);
    let mut line = String::new();
    loop {
        line.clear();
        let n = buf.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        let trimmed = line.trim();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_line_is_single_line() {
        let v = serde_json::json!({"a":"x\ny"});
        let s = encode_line(&v).unwrap();
        assert_eq!(s.matches('\n').count(), 1, "only the terminator newline");
        assert!(s.ends_with('\n'));
    }

    #[test]
    fn serve_stream_handles_multiple_lines_then_stops() {
        let input = "{\"id\":1}\n{\"id\":2}\n";
        let mut out = Vec::new();
        let seen = std::cell::RefCell::new(Vec::new());
        serve_stream(input.as_bytes(), &mut out, |line| {
            seen.borrow_mut().push(line.to_string());
            Some(format!("echo:{line}\n"))
        })
        .unwrap();
        assert_eq!(seen.borrow().len(), 2);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("echo:{\"id\":1}"));
        assert!(text.contains("echo:{\"id\":2}"));
    }

    #[test]
    fn serve_stream_stops_when_handler_returns_none() {
        let input = "{\"id\":1}\n{\"id\":2}\n";
        let mut out = Vec::new();
        serve_stream(input.as_bytes(), &mut out, |_| None).unwrap();
        assert!(out.is_empty(), "handler veto must end the session immediately");
    }

    #[test]
    fn serve_stream_ignores_blank_lines() {
        let input = "\n  \n{\"id\":1}\n";
        let mut out = Vec::new();
        serve_stream(input.as_bytes(), &mut out, |line| Some(format!("{line}\n"))).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text, "{\"id\":1}\n");
    }
}
