// The smallest app that uses a company connection (IC-952), so the
// connection is proved through the binary a person runs rather than only in
// the host.
//
//   krate run probe.krate -- <METHOD> <URL> [<header-name> <header-value>] [dump]
//
// It sends one request -- with the app's own header if one is given, which a
// connection's credential header replaces -- and prints one line naming the
// outcome: `status=<n> body=<text>`, or `error=<kind> <words>`. With `dump`
// it then writes its whole linear memory to stdout after a marker line, so
// a test can prove the credential never entered the app's memory.
#![no_std]
extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use krate::{
    io::{args, stdio, streams::OutputStreamExt},
    net::{self, Header, HttpMethod, NetError, Request},
    Guest,
};

struct Component;

impl Guest for Component {
    fn run() -> i32 {
        let raw = args::raw();
        let words: Vec<&str> = raw.split('\n').filter(|w| !w.is_empty()).collect();
        let stdout = stdio::stdout();
        let dump = words.last() == Some(&"dump");
        let words: Vec<&str> = if dump {
            words[..words.len() - 1].to_vec()
        } else {
            words
        };
        let (method, url) = match (words.first(), words.get(1)) {
            (Some(method), Some(url)) => (*method, *url),
            _ => {
                let _ = stdout.write_line("usage: <METHOD> <URL> [<header> <value>] [dump]");
                return 2;
            }
        };
        let method = match method {
            "GET" => HttpMethod::Get,
            "POST" => HttpMethod::Post,
            "PUT" => HttpMethod::Put,
            "DELETE" => HttpMethod::Delete,
            "PATCH" => HttpMethod::Patch,
            "HEAD" => HttpMethod::Head,
            _ => HttpMethod::Options,
        };
        let mut headers = Vec::new();
        if let (Some(name), Some(value)) = (words.get(2), words.get(3)) {
            headers.push(Header {
                name: String::from(*name),
                value: String::from(*value),
            });
        }
        let line = match net::fetch(Request {
            method,
            url: String::from(url),
            headers,
            body: Vec::new(),
            timeout_millis: Some(10_000),
        }) {
            Ok(response) => {
                let body = String::from_utf8_lossy(&response.body);
                let mut line = String::from("status=");
                line.push_str(&number(response.status as u32));
                line.push_str(" body=");
                line.push_str(body.trim());
                line
            }
            Err(err) => describe(err),
        };
        let _ = stdout.write_line(&line);
        let _ = stdout.flush();
        if dump {
            let _ = stdout.write_line("--- memory ---");
            let pages = core::arch::wasm32::memory_size(0);
            let len = pages * 65536;
            // Address 0 is never handed out by the allocator or the stack;
            // starting at 1 keeps the pointer non-null.
            // SAFETY: every address below memory_size * 64 KiB is mapped
            // linear memory, readable as bytes.
            let memory = unsafe { core::slice::from_raw_parts(1 as *const u8, len - 1) };
            let mut copy = Vec::with_capacity(memory.len());
            copy.extend_from_slice(memory);
            let _ = stdout.write_all(&copy);
            let _ = stdout.flush();
        }
        0
    }
}

fn number(mut n: u32) -> String {
    let mut digits = Vec::new();
    loop {
        digits.push(b'0' + (n % 10) as u8);
        n /= 10;
        if n == 0 {
            break;
        }
    }
    digits.reverse();
    String::from_utf8(digits).unwrap_or_default()
}

fn describe(err: NetError) -> String {
    let (kind, words) = match err {
        NetError::PermissionDenied => ("permission-denied", String::new()),
        NetError::InvalidUrl => ("invalid-url", String::new()),
        NetError::BodyTooLarge => ("too-large", String::new()),
        NetError::Timeout => ("timeout", String::new()),
        NetError::Protocol(w) => ("protocol", w),
        NetError::TlsFailure(w) => ("tls", w),
        NetError::DnsFailure(w) => ("dns", w),
        NetError::ConnectFailure(w) => ("connect", w),
        NetError::Other(w) => ("other", w),
    };
    let mut line = String::from("error=");
    line.push_str(kind);
    if !words.is_empty() {
        line.push(' ');
        line.push_str(&words);
    }
    line
}

krate::export!(Component);
