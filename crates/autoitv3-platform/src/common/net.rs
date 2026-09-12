//! Network service — AutoIt's `Inet*`, `TCP*`, `UDP*` and proxy builtins.
//!
//! Implemented with `std::net`, so the same code runs on every host. Everything
//! that touches the network is gated by the execution profile's effect policy:
//! under [`EffectPolicy::ReadOnly`](autoitv3_runtime::profile::EffectPolicy)
//! (the deterministic deobfuscation profile) it fails with `@error = 1` instead
//! of opening a socket.
//!
//! # Deliberate approximations
//!
//! * `InetGet`/`InetRead`/`InetGetSize` speak **plain HTTP** only. `https://`
//!   needs TLS, which this deliberately dependency-free layer does not provide,
//!   so those URLs fail with `@error = 1`. `Transfer-Encoding: chunked` is
//!   decoded.
//! * `InetGet`'s `background` flag is accepted but the transfer completes
//!   synchronously; the returned handle still answers `InetGetInfo`/`InetClose`.
//! * `TCPAccept` is non-blocking (returns `-1` when nobody is waiting) so an
//!   analysis run cannot hang on it; AutoIt blocks there.
//! * `UDPOpen`'s address is treated as the **peer**, `UDPBind` rebinds the local
//!   socket, and `UDPRecv` remembers the sender so a reply can be sent.
//! * `$STDERR_MERGED`-style proxy settings are stored; only the user-agent and
//!   proxy values influence `Inet*` requests (as a `Proxy` header is not
//!   portable, the stored proxy is currently informational).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::rc::Rc;
use std::time::Duration;

use autoitv3_runtime::host::HostContext;
use autoitv3_runtime::profile::EffectPolicy;
use autoitv3_runtime::profile::EffectKind;
use autoitv3_runtime::value::Value;

/// Every function this service implements.
pub const FUNCTIONS: &[&str] = &[
    "InetGet",
    "InetGetInfo",
    "InetGetSize",
    "InetRead",
    "InetClose",
    "FtpSetProxy",
    "HttpSetProxy",
    "HttpSetUserAgent",
    "Ping",
    "TCPStartup",
    "TCPShutdown",
    "TCPConnect",
    "TCPListen",
    "TCPAccept",
    "TCPRecv",
    "TCPSend",
    "TCPCloseSocket",
    "TCPNameToIP",
    "UDPStartup",
    "UDPShutdown",
    "UDPOpen",
    "UDPBind",
    "UDPRecv",
    "UDPSend",
    "UDPCloseSocket",
];

enum TcpEntry {
    Listener(TcpListener),
    Stream(TcpStream),
}

struct UdpEntry {
    socket: UdpSocket,
    peer: Option<SocketAddr>,
}

struct Download {
    bytes: i64,
    complete: bool,
}

/// The network service.
pub struct NetworkService {
    user_agent: String,
    http_proxy: Option<String>,
    ftp_proxy: Option<String>,
    tcp: Vec<Option<TcpEntry>>,
    udp: Vec<Option<UdpEntry>>,
    downloads: Vec<Option<Download>>,
}

impl Default for NetworkService {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkService {
    /// Create the service with AutoIt's default user-agent.
    pub fn new() -> Self {
        Self {
            user_agent: "AutoIt".to_string(),
            http_proxy: None,
            ftp_proxy: None,
            tcp: Vec::new(),
            udp: Vec::new(),
            downloads: Vec::new(),
        }
    }

    /// Whether this service provides `name`.
    pub fn provides(name: &str) -> bool {
        FUNCTIONS.iter().any(|f| f.eq_ignore_ascii_case(name))
    }

    /// Dispatch a call; `None` means "not mine".
    pub fn call(&mut self, name: &str, args: &[Value], ctx: &mut dyn HostContext) -> Option<Value> {
        Some(match name {
            "tcpstartup" | "udpstartup" => Value::Int(1),
            "tcpshutdown" => {
                self.tcp.clear();
                Value::Int(1)
            }
            "udpshutdown" => {
                self.udp.clear();
                Value::Int(1)
            }
            "httpsetuseragent" => {
                self.user_agent = arg_str(args, 0);
                Value::Int(1)
            }
            "httpsetproxy" => {
                self.http_proxy = proxy_arg(args);
                Value::Int(1)
            }
            "ftpsetproxy" => {
                self.ftp_proxy = proxy_arg(args);
                Value::Int(1)
            }
            "tcpconnect" => self.tcp_connect(args, ctx),
            "tcplisten" => self.tcp_listen(args, ctx),
            "tcpaccept" => self.tcp_accept(args, ctx),
            "tcprecv" => self.tcp_recv(args, ctx),
            "tcpsend" => self.tcp_send(args, ctx),
            "tcpclosesocket" => self.tcp_close(args, ctx),
            "tcpnametoip" => self.tcp_name_to_ip(args, ctx),
            "udpopen" => self.udp_open(args, ctx),
            "udpbind" => self.udp_bind(args, ctx),
            "udprecv" => self.udp_recv(args, ctx),
            "udpsend" => self.udp_send(args, ctx),
            "udpclosesocket" => self.udp_close(args, ctx),
            "inetget" => self.inet_get(args, ctx),
            "inetread" => self.inet_read(args, ctx),
            "inetgetsize" => self.inet_get_size(args, ctx),
            "inetgetinfo" => self.inet_get_info(args, ctx),
            "inetclose" => self.inet_close(args, ctx),
            "ping" => self.ping(args, ctx),
            _ => return None,
        })
    }

    // ----- TCP -----

    fn tcp_connect(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        if !io_allowed(ctx) {
            ctx.set_error(1, 0);
            return Value::Int(-1);
        }
        let ip = arg_str(args, 0);
        let port = arg_int(args, 1);
        let timeout = args
            .get(2)
            .map(|v| v.to_int())
            .filter(|t| *t > 0)
            .unwrap_or(5000);
        match connect_addr(&ip, port, timeout) {
            Some(s) => {
                let _ = s.set_nonblocking(true);
                self.tcp.push(Some(TcpEntry::Stream(s)));
                ctx.set_error(0, 0);
                Value::Int(self.tcp.len() as i64)
            }
            None => {
                ctx.set_error(1, 0);
                Value::Int(-1)
            }
        }
    }

    fn tcp_listen(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        if !io_allowed(ctx) {
            ctx.set_error(1, 0);
            return Value::Int(-1);
        }
        let ip = if arg_str(args, 0).is_empty() {
            "0.0.0.0".to_string()
        } else {
            arg_str(args, 0)
        };
        let port = arg_int(args, 1);
        let Ok(addrs) = (ip.as_str(), port as u16).to_socket_addrs() else {
            ctx.set_error(1, 0);
            return Value::Int(-1);
        };
        for addr in addrs {
            if let Ok(l) = TcpListener::bind(addr) {
                let _ = l.set_nonblocking(true);
                self.tcp.push(Some(TcpEntry::Listener(l)));
                ctx.set_error(0, 0);
                return Value::Int(self.tcp.len() as i64);
            }
        }
        ctx.set_error(1, 0);
        Value::Int(-1)
    }

    fn tcp_accept(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        if !io_allowed(ctx) {
            ctx.set_error(1, 0);
            return Value::Int(-1);
        }
        let handle = arg_int(args, 0);
        if handle < 1 {
            ctx.set_error(1, 0);
            return Value::Int(-1);
        }
        let accepted = match self.tcp.get_mut(handle as usize - 1).and_then(|o| o.as_mut()) {
            Some(TcpEntry::Listener(l)) => Some(l.accept()),
            _ => None,
        };
        match accepted {
            Some(Ok((s, _))) => {
                let _ = s.set_nonblocking(true);
                self.tcp.push(Some(TcpEntry::Stream(s)));
                ctx.set_error(0, 0);
                Value::Int(self.tcp.len() as i64)
            }
            _ => {
                ctx.set_error(1, 0);
                Value::Int(-1)
            }
        }
    }

    fn tcp_recv(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let handle = arg_int(args, 0);
        let maxlen = arg_int(args, 1).max(1).min(1 << 20) as usize;
        let binary = arg_int(args, 2) & 1 != 0;
        let mut buf = vec![0u8; maxlen];
        let read = match self.tcp.get_mut(handle as usize - 1).and_then(|o| o.as_mut()) {
            Some(TcpEntry::Stream(s)) => s.read(&mut buf),
            _ => {
                ctx.set_error(1, 0);
                return Value::str("");
            }
        };
        match read {
            Ok(n) => {
                buf.truncate(n);
                ctx.set_error(0, n as i64);
                if binary {
                    Value::Binary(Rc::new(buf))
                } else {
                    Value::Str(String::from_utf8_lossy(&buf).into_owned())
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                ctx.set_error(0, 0);
                Value::str("")
            }
            Err(_) => {
                ctx.set_error(1, 0);
                Value::str("")
            }
        }
    }

    fn tcp_send(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        if !io_allowed(ctx) {
            ctx.set_error(1, 0);
            return Value::Int(0);
        }
        let handle = arg_int(args, 0);
        let data = arg_str(args, 1);
        let sent = match self.tcp.get_mut(handle as usize - 1).and_then(|o| o.as_mut()) {
            Some(TcpEntry::Stream(s)) => s.write(data.as_bytes()),
            _ => {
                ctx.set_error(1, 0);
                return Value::Int(0);
            }
        };
        match sent {
            Ok(n) => {
                ctx.set_error(0, n as i64);
                Value::Int(n as i64)
            }
            Err(_) => {
                ctx.set_error(1, 0);
                Value::Int(0)
            }
        }
    }

    fn tcp_close(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let handle = arg_int(args, 0);
        if handle >= 1 && (handle as usize) <= self.tcp.len() {
            self.tcp[handle as usize - 1] = None;
            ctx.set_error(0, 0);
            Value::Int(1)
        } else {
            ctx.set_error(1, 0);
            Value::Int(0)
        }
    }

    fn tcp_name_to_ip(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        if !io_allowed(ctx) {
            ctx.set_error(1, 0);
            return Value::str("");
        }
        let name = arg_str(args, 0);
        match (name.as_str(), 0u16).to_socket_addrs() {
            Ok(addrs) => {
                let all: Vec<_> = addrs.collect();
                // AutoIt hands back an IPv4 dotted quad when one exists;
                // `localhost` otherwise resolves to `::1` first on many hosts.
                let pick = all
                    .iter()
                    .find(|a| a.is_ipv4())
                    .or_else(|| all.first())
                    .copied();
                match pick {
                    Some(a) => {
                        ctx.set_error(0, 0);
                        Value::Str(a.ip().to_string())
                    }
                    None => {
                        ctx.set_error(1, 0);
                        Value::str("")
                    }
                }
            }
            Err(_) => {
                ctx.set_error(1, 0);
                Value::str("")
            }
        }
    }

    // ----- UDP -----

    fn udp_open(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        if !io_allowed(ctx) {
            ctx.set_error(1, 0);
            return Value::Int(-1);
        }
        let ip = arg_str(args, 0);
        let port = arg_int(args, 1);
        let local: SocketAddr = "0.0.0.0:0".parse().expect("literal addr");
        let Ok(socket) = UdpSocket::bind(local) else {
            ctx.set_error(1, 0);
            return Value::Int(-1);
        };
        let _ = socket.set_nonblocking(true);
        let peer = if !ip.is_empty() && port > 0 {
            resolve(&ip, port).and_then(|a| a.into_iter().next())
        } else {
            None
        };
        self.udp.push(Some(UdpEntry { socket, peer }));
        ctx.set_error(0, 0);
        Value::Int(self.udp.len() as i64)
    }

    fn udp_bind(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        if !io_allowed(ctx) {
            ctx.set_error(1, 0);
            return Value::Int(-1);
        }
        let handle = arg_int(args, 0);
        let ip = if arg_str(args, 1).is_empty() {
            "0.0.0.0".to_string()
        } else {
            arg_str(args, 1)
        };
        let port = arg_int(args, 2);
        let Ok(addrs) = (ip.as_str(), port as u16).to_socket_addrs() else {
            ctx.set_error(1, 0);
            return Value::Int(-1);
        };
        let Some(entry) = self.udp.get_mut(handle as usize - 1).and_then(|o| o.as_mut()) else {
            ctx.set_error(1, 0);
            return Value::Int(-1);
        };
        for addr in addrs {
            if let Ok(s) = UdpSocket::bind(addr) {
                let _ = s.set_nonblocking(true);
                entry.socket = s;
                ctx.set_error(0, 0);
                return Value::Int(handle);
            }
        }
        ctx.set_error(1, 0);
        Value::Int(-1)
    }

    fn udp_recv(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let handle = arg_int(args, 0);
        let maxlen = arg_int(args, 1).max(1).min(1 << 20) as usize;
        let binary = arg_int(args, 2) & 1 != 0;
        let mut buf = vec![0u8; maxlen];
        let Some(entry) = self.udp.get_mut(handle as usize - 1).and_then(|o| o.as_mut()) else {
            ctx.set_error(1, 0);
            return Value::str("");
        };
        match entry.socket.recv_from(&mut buf) {
            Ok((n, from)) => {
                // Remember the sender so the script can reply with UDPSend.
                entry.peer = Some(from);
                buf.truncate(n);
                ctx.set_error(0, n as i64);
                if binary {
                    Value::Binary(Rc::new(buf))
                } else {
                    Value::Str(String::from_utf8_lossy(&buf).into_owned())
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                ctx.set_error(0, 0);
                Value::str("")
            }
            Err(_) => {
                ctx.set_error(1, 0);
                Value::str("")
            }
        }
    }

    fn udp_send(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        if !io_allowed(ctx) {
            ctx.set_error(1, 0);
            return Value::Int(0);
        }
        let handle = arg_int(args, 0);
        let data = arg_str(args, 1);
        let Some(peer) = self
            .udp
            .get(handle as usize - 1)
            .and_then(|o| o.as_ref())
            .and_then(|e| e.peer)
        else {
            ctx.set_error(1, 0);
            return Value::Int(0);
        };
        let sent = self
            .udp
            .get(handle as usize - 1)
            .and_then(|o| o.as_ref())
            .map(|e| e.socket.send_to(data.as_bytes(), peer));
        match sent {
            Some(Ok(n)) => {
                ctx.set_error(0, n as i64);
                Value::Int(n as i64)
            }
            _ => {
                ctx.set_error(1, 0);
                Value::Int(0)
            }
        }
    }

    fn udp_close(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let handle = arg_int(args, 0);
        if handle >= 1 && (handle as usize) <= self.udp.len() {
            self.udp[handle as usize - 1] = None;
            ctx.set_error(0, 0);
            Value::Int(1)
        } else {
            ctx.set_error(1, 0);
            Value::Int(0)
        }
    }

    // ----- Inet -----

    fn inet_get(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        if !io_allowed(ctx) {
            ctx.set_error(1, 0);
            return Value::Int(0);
        }
        let url = arg_str(args, 0);
        let file = arg_str(args, 1);
        let background = arg_int(args, 3) != 0;
        let Some(resp) = http_get(&url, &self.user_agent) else {
            ctx.set_error(1, 0);
            return Value::Int(0);
        };
        if !file.is_empty() && !ctx.effect_allowed(EffectKind::NetAccess) {
            ctx.set_error(1, 0);
            return Value::Int(0);
        }
        if !file.is_empty() && std::fs::write(&file, &resp.body).is_err() {
            ctx.set_error(1, 0);
            return Value::Int(0);
        }
        let n = resp.body.len() as i64;
        ctx.set_error(0, 0);
        if background {
            self.downloads.push(Some(Download {
                bytes: n,
                complete: true,
            }));
            Value::Int(self.downloads.len() as i64)
        } else {
            Value::Int(n)
        }
    }

    fn inet_read(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        if !io_allowed(ctx) {
            ctx.set_error(1, 0);
            return Value::Binary(Rc::new(Vec::new()));
        }
        let url = arg_str(args, 0);
        match http_get(&url, &self.user_agent) {
            Some(resp) => {
                ctx.set_error(0, resp.body.len() as i64);
                Value::Binary(Rc::new(resp.body))
            }
            None => {
                ctx.set_error(1, 0);
                Value::Binary(Rc::new(Vec::new()))
            }
        }
    }

    fn inet_get_size(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        if !io_allowed(ctx) {
            ctx.set_error(1, 0);
            return Value::Int(0);
        }
        let url = arg_str(args, 0);
        match http_get(&url, &self.user_agent) {
            Some(resp) => {
                let size = resp
                    .content_length
                    .unwrap_or_else(|| resp.body.len() as i64);
                ctx.set_error(0, 0);
                Value::Int(size)
            }
            None => {
                ctx.set_error(1, 0);
                Value::Int(0)
            }
        }
    }

    fn inet_get_info(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let handle = arg_int(args, 0);
        let Some(Some(d)) = self.downloads.get(handle as usize - 1) else {
            ctx.set_error(1, 0);
            return Value::array(vec![Value::Int(0)]);
        };
        ctx.set_error(0, 0);
        Value::array(vec![
            Value::Int(d.bytes),
            Value::Int(i64::from(d.complete)),
            Value::Int(0),
            Value::Int(0),
        ])
    }

    fn inet_close(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let handle = arg_int(args, 0);
        if handle >= 1 && (handle as usize) <= self.downloads.len() {
            self.downloads[handle as usize - 1] = None;
            ctx.set_error(0, 0);
            Value::Int(1)
        } else {
            ctx.set_error(1, 0);
            Value::Int(0)
        }
    }

    // ----- Ping -----

    fn ping(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        if !io_allowed(ctx) {
            ctx.set_error(1, 0);
            return Value::Int(0);
        }
        let host = arg_str(args, 0);
        let timeout_ms = args
            .get(1)
            .map(|v| v.to_int())
            .filter(|t| *t > 0)
            .unwrap_or(4000);
        // Shell out to the host's `ping`; parsing its "time=" field avoids a
        // raw-socket dependency (and the privileges raw ICMP would need).
        let output = std::process::Command::new("ping")
            .arg("-c")
            .arg("1")
            .arg("-W")
            .arg((timeout_ms / 1000).max(1).to_string())
            .arg(&host)
            .output();
        let Ok(out) = output else {
            ctx.set_error(1, 0);
            return Value::Int(0);
        };
        if !out.status.success() {
            ctx.set_error(1, 0);
            return Value::Int(0);
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let rtt = text
            .split("time=")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|v| v.parse::<f64>().ok());
        match rtt {
            Some(ms) => {
                ctx.set_error(0, 0);
                Value::Int(ms.round() as i64)
            }
            None => {
                ctx.set_error(1, 0);
                Value::Int(0)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn io_allowed(ctx: &dyn HostContext) -> bool {
    matches!(ctx.profile().effects, EffectPolicy::Allow)
}

fn arg_str(args: &[Value], i: usize) -> String {
    args.get(i).map(|v| v.to_autoit_string()).unwrap_or_default()
}

fn arg_int(args: &[Value], i: usize) -> i64 {
    args.get(i).map(|v| v.to_int()).unwrap_or(0)
}

fn proxy_arg(args: &[Value]) -> Option<String> {
    args.get(1).map(|v| v.to_autoit_string()).filter(|s| !s.is_empty())
}

fn resolve(ip: &str, port: i64) -> Option<Vec<SocketAddr>> {
    if !(0..=65535).contains(&port) {
        return None;
    }
    (ip, port as u16)
        .to_socket_addrs()
        .ok()
        .map(|it| it.collect())
}

fn connect_addr(ip: &str, port: i64, timeout_ms: i64) -> Option<TcpStream> {
    let addrs = resolve(ip, port)?;
    let timeout = Duration::from_millis(timeout_ms.max(1) as u64);
    for addr in addrs {
        if let Ok(s) = TcpStream::connect_timeout(&addr, timeout) {
            return Some(s);
        }
    }
    None
}

struct HttpResponse {
    content_length: Option<i64>,
    body: Vec<u8>,
}

/// A minimal HTTP/1.1 GET. Returns `None` for `https://` (no TLS) or any error.
fn http_get(url: &str, user_agent: &str) -> Option<HttpResponse> {
    let (host, port, path) = parse_http_url(url)?;
    let mut stream = connect_addr(&host, i64::from(port), 15_000)?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: {user_agent}\r\nAccept: */*\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).ok()?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).ok()?;

    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let body_raw = &raw[split + 4..];
    let headers_lower = head.to_ascii_lowercase();
    let content_length = headers_lower
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<i64>().ok());
    let body = if headers_lower.contains("transfer-encoding: chunked") {
        decode_chunked(body_raw)
    } else {
        body_raw.to_vec()
    };
    Some(HttpResponse {
        content_length,
        body,
    })
}

/// `(host, port, path)` for an `http://` URL. `https://` is rejected.
fn parse_http_url(url: &str) -> Option<(String, u16, String)> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !h.is_empty() => {
            (h.to_string(), p.parse::<u16>().ok()?)
        }
        _ => (authority.to_string(), 80),
    };
    if host.is_empty() {
        return None;
    }
    Some((host, port, path.to_string()))
}

fn decode_chunked(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let Some(nl) = raw[i..].iter().position(|b| *b == b'\n') else {
            break;
        };
        let line = String::from_utf8_lossy(&raw[i..i + nl]);
        let size = usize::from_str_radix(
            line.trim().split(';').next().unwrap_or("").trim(),
            16,
        )
        .unwrap_or(0);
        i += nl + 1;
        if size == 0 || i + size > raw.len() {
            break;
        }
        out.extend_from_slice(&raw[i..i + size]);
        i += size;
        if i < raw.len() && raw[i] == b'\r' {
            i += 1;
        }
        if i < raw.len() && raw[i] == b'\n' {
            i += 1;
        }
    }
    out
}
