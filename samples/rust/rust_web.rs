// SPDX-License-Identifier: GPL-2.0

//! Minimal in-kernel async web server written in Rust.

use kernel::{
    kasync::executor::{workqueue::Executor as WqExecutor, AutoStopHandle, Executor},
    kasync::net::{TcpListener, TcpStream},
    net::{self, Ipv4Addr, SocketAddr, SocketAddrV4},
    prelude::*,
    spawn_task,
    sync::{Ref, RefBorrow},
    str::CStr,
};

module! {
    type: RustServer,
    name: "rust_web",
    author: "Andrea Righi <andrea.righi@canonical.com>",
    description: "Minimal Rust async web server",
    license: "GPL v2",
    params: {
        server_port: u16 {
            default: 8080,
            permissions: 0o644,
            description: "Server port used for client connections",
        },
    },
}

const RESPONSE: &str = r###"HTTP/1.1 200
Server: kernel
Content-Type: text/html; charset=UTF-8

<!doctype html>
<html>
<body>
<table>
<tr>
<td><img src="rust_logo.svg" /></td>
<td><h1>Hello from a minimal kernel Rust web server</h1></td>
</table>
</body>
</html>
"###;

const LOGO: &str = r###"HTTP/1.1 200
Server: kernel
Content-Type: image/svg+xml

<svg version="1.1" height="106" width="106" xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink">
<g id="logo" transform="translate(53, 53)">
  <path id="r" transform="translate(0.5, 0.5)" stroke="black" stroke-width="1" stroke-linejoin="round" d="
    M -9,-15 H 4 C 12,-15 12,-7 4,-7 H -9 Z
    M -40,22 H 0 V 11 H -9 V 3 H 1 C 12,3 6,22 15,22 H 40
    V 3 H 34 V 5 C 34,13 25,12 24,7 C 23,2 19,-2 18,-2 C 33,-10 24,-26 12,-26 H -35
    V -15 H -25 V 11 H -40 Z" />
  <g id="gear" mask="url(#holes)">
    <circle r="43" fill="none" stroke="black" stroke-width="9" />
    <g id="cogs">
      <polygon id="cog" stroke="black" stroke-width="3" stroke-linejoin="round" points="46,3 51,0 46,-3" />
      <use xlink:href="#cog" transform="rotate(11.25)" />
      <use xlink:href="#cog" transform="rotate(22.50)" />
      <use xlink:href="#cog" transform="rotate(33.75)" />
      <use xlink:href="#cog" transform="rotate(45.00)" />
      <use xlink:href="#cog" transform="rotate(56.25)" />
      <use xlink:href="#cog" transform="rotate(67.50)" />
      <use xlink:href="#cog" transform="rotate(78.75)" />
      <use xlink:href="#cog" transform="rotate(90.00)" />
      <use xlink:href="#cog" transform="rotate(101.25)" />
      <use xlink:href="#cog" transform="rotate(112.50)" />
      <use xlink:href="#cog" transform="rotate(123.75)" />
      <use xlink:href="#cog" transform="rotate(135.00)" />
      <use xlink:href="#cog" transform="rotate(146.25)" />
      <use xlink:href="#cog" transform="rotate(157.50)" />
      <use xlink:href="#cog" transform="rotate(168.75)" />
      <use xlink:href="#cog" transform="rotate(180.00)" />
      <use xlink:href="#cog" transform="rotate(191.25)" />
      <use xlink:href="#cog" transform="rotate(202.50)" />
      <use xlink:href="#cog" transform="rotate(213.75)" />
      <use xlink:href="#cog" transform="rotate(225.00)" />
      <use xlink:href="#cog" transform="rotate(236.25)" />
      <use xlink:href="#cog" transform="rotate(247.50)" />
      <use xlink:href="#cog" transform="rotate(258.75)" />
      <use xlink:href="#cog" transform="rotate(270.00)" />
      <use xlink:href="#cog" transform="rotate(281.25)" />
      <use xlink:href="#cog" transform="rotate(292.50)" />
      <use xlink:href="#cog" transform="rotate(303.75)" />
      <use xlink:href="#cog" transform="rotate(315.00)" />
      <use xlink:href="#cog" transform="rotate(326.25)" />
      <use xlink:href="#cog" transform="rotate(337.50)" />
      <use xlink:href="#cog" transform="rotate(348.75)" />
    </g>
    <g id="mounts">
      <polygon id="mount" stroke="black" stroke-width="6" stroke-linejoin="round" points="-7,-42 0,-35 7,-42" />
      <use xlink:href="#mount" transform="rotate(72)" />
      <use xlink:href="#mount" transform="rotate(144)" />
      <use xlink:href="#mount" transform="rotate(216)" />
      <use xlink:href="#mount" transform="rotate(288)" />
    </g>
  </g>
  <mask id="holes">
    <rect x="-60" y="-60" width="120" height="120" fill="white"/>
    <circle id="hole" cy="-40" r="3" />
    <use xlink:href="#hole" transform="rotate(72)" />
    <use xlink:href="#hole" transform="rotate(144)" />
    <use xlink:href="#hole" transform="rotate(216)" />
    <use xlink:href="#hole" transform="rotate(288)" />
  </mask>
</g>
</svg>
"###;

const ERROR: &str = r###"HTTP/1.1 404
Server: kernel
Content-Type: text/html; charset=UTF-8

<!doctype html>
<html>
<body>
<h1>Error 404, the requested URL was not found on this server.</h1>
</body>
</html>
"###;

async fn server_worker(stream: TcpStream) -> Result {
    let mut buf = [0u8; 1024];
    let n = stream.read(&mut buf).await?;
    if n > 0 && n < buf.len() - 1 {
        let cstr: &CStr = CStr::from_bytes_with_nul(&buf[0 .. n + 1])
                                .expect("CStr::from_bytes_with_nul failed");
        let s: &str = cstr.to_str().unwrap();
        if s.starts_with("GET / ") {
            stream.write_all(RESPONSE.as_bytes()).await?;
        } else if s.starts_with("GET /rust_logo.svg ") {
            stream.write_all(LOGO.as_bytes()).await?;
        } else {
            stream.write_all(ERROR.as_bytes()).await?;
        }
    } else {
        return Err(EINVAL);
    }
    return Ok(());
}

async fn accept_loop(listener: TcpListener, executor: Ref<impl Executor>) {
    loop {
        if let Ok(stream) = listener.accept().await {
            let _ = spawn_task!(executor.as_ref_borrow(), server_worker(stream));
        }
    }
}

fn start_listener(ex: RefBorrow<'_, impl Executor + Send + Sync + 'static>, port: u16) -> Result {
    let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::ANY, port));
    let listener = TcpListener::try_new(net::init_ns(), &addr)?;
    spawn_task!(ex, accept_loop(listener, ex.into()))?;
    Ok(())
}

struct RustServer {
    _handle: AutoStopHandle<dyn Executor>,
}

impl kernel::Module for RustServer {
    fn init(_name: &'static CStr, module: &'static ThisModule) -> Result<Self> {
        let lock = module.kernel_param_lock();
        let port = *server_port.read(&lock);
        let handle = WqExecutor::try_new(kernel::workqueue::system())?;
        start_listener(handle.executor(), port)?;
        Ok(Self {
            _handle: handle.into(),
        })
    }
}
