//! Finds an HTTP/1 Host header without copying the request.
use std::ops::Range;

pub fn find_host(payload: &[u8]) -> Option<Range<usize>> {
    let end = payload.windows(4).position(|w| w == b"\r\n\r\n")?;
    let first = payload.windows(2).position(|w| w == b"\r\n")?;
    if !payload[..first].ends_with(b" HTTP/1.1") && !payload[..first].ends_with(b" HTTP/1.0") {
        return None;
    }
    let mut at = first + 2;
    while at < end {
        let line_end = at + payload[at..].windows(2).position(|w| w == b"\r\n")?;
        let line = &payload[at..line_end];
        if line.len() >= 5 && line[..5].eq_ignore_ascii_case(b"host:") {
            let mut start = at + 5;
            while start < line_end && payload[start].is_ascii_whitespace() {
                start += 1;
            }
            let mut stop = line_end;
            while stop > start && payload[stop - 1].is_ascii_whitespace() {
                stop -= 1;
            }
            if let Some(colon) = payload[start..stop].iter().position(|b| *b == b':') {
                stop = start + colon;
            }
            if start == stop
                || !payload[start..stop]
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || *b == b'.' || *b == b'-')
            {
                return None;
            }
            return Some(start..stop);
        }
        at = line_end + 2;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finds_case_insensitive_host_and_excludes_port() {
        let request = b"GET / HTTP/1.1\r\nUser-Agent: test\r\nhOsT: Example.org:8080 \r\n\r\n";
        assert_eq!(&request[find_host(request).unwrap()], b"Example.org");
    }
    #[test]
    fn ignores_body_and_incomplete_headers() {
        assert!(
            find_host(b"POST / HTTP/1.1\r\nContent-Length: 20\r\n\r\nHost: example.org\r\n")
                .is_none()
        );
        assert!(find_host(b"GET / HTTP/1.1\r\nHost: example.org").is_none());
        assert!(find_host(b"random HTTP/2\r\nHost: example.org\r\n\r\n").is_none());
    }
}
