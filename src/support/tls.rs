//! TLS client over the system OpenSSL (libssl), via direct FFI.
//!
//! Provides a verified TLS client connection usable like a `TcpStream`
//! (implements `Read` + `Write`). Certificate chain verification against the
//! system trust store AND hostname verification are enabled — required for
//! handling money over untrusted networks. A connection only succeeds if the
//! peer presents a valid certificate for the requested host.
//!
//! Scope: client side only (HTTPS client, future DB TLS). Server-side TLS is a
//! later addition.

#![allow(non_camel_case_types)]

use std::ffi::CString;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::os::unix::io::AsRawFd;
use std::os::raw::{c_char, c_int, c_long, c_void};
use std::time::Duration;

// --- Opaque OpenSSL types ---------------------------------------------------
type SSL_METHOD = c_void;
type SSL_CTX = c_void;
type SSL = c_void;

// --- Constants (from OpenSSL headers) ---------------------------------------
const SSL_VERIFY_PEER: c_int = 0x01;
const SSL_ERROR_WANT_READ: c_int = 2;
const SSL_ERROR_WANT_WRITE: c_int = 3;
const SSL_CTRL_SET_TLSEXT_HOSTNAME: c_int = 55;
const TLSEXT_NAMETYPE_HOST_NAME: c_long = 0;
// X509_V_OK == 0 means verification succeeded.
const X509_V_OK: c_long = 0;

extern "C" {
    fn TLS_client_method() -> *const SSL_METHOD;
    fn SSL_CTX_new(method: *const SSL_METHOD) -> *mut SSL_CTX;
    fn SSL_CTX_free(ctx: *mut SSL_CTX);
    fn SSL_CTX_set_default_verify_paths(ctx: *mut SSL_CTX) -> c_int;
    fn SSL_CTX_set_verify(ctx: *mut SSL_CTX, mode: c_int, cb: *const c_void);
    fn SSL_new(ctx: *mut SSL_CTX) -> *mut SSL;
    fn SSL_free(ssl: *mut SSL);
    fn SSL_set_fd(ssl: *mut SSL, fd: c_int) -> c_int;
    fn SSL_connect(ssl: *mut SSL) -> c_int;
    fn SSL_read(ssl: *mut SSL, buf: *mut c_void, num: c_int) -> c_int;
    fn SSL_write(ssl: *mut SSL, buf: *const c_void, num: c_int) -> c_int;
    fn SSL_shutdown(ssl: *mut SSL) -> c_int;
    fn SSL_get_error(ssl: *const SSL, ret: c_int) -> c_int;
    fn SSL_get_verify_result(ssl: *const SSL) -> c_long;
    fn SSL_ctrl(ssl: *mut SSL, cmd: c_int, larg: c_long, parg: *mut c_void) -> c_long;
    // SSL_set1_host: set expected DNS hostname for built-in verification.
    fn SSL_set1_host(ssl: *mut SSL, hostname: *const c_char) -> c_int;
}

/// A verified TLS client connection. Owns the TCP stream and SSL state.
pub struct TlsStream {
    ssl: *mut SSL,
    ctx: *mut SSL_CTX,
    // Keep the TCP stream alive for the lifetime of the SSL session.
    _tcp: TcpStream,
}

// The OpenSSL session is used from a single thread at a time in our usage.
unsafe impl Send for TlsStream {}

impl TlsStream {
    /// Open a verified TLS connection to `host:port`.
    /// Fails if the certificate chain or hostname does not validate.
    pub fn connect(host: &str, port: u16, timeout: Duration) -> io::Result<TlsStream> {
        use std::net::ToSocketAddrs;
        let addr = (host, port)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "could not resolve host"))?;
        let tcp = TcpStream::connect_timeout(&addr, timeout)?;
        tcp.set_read_timeout(Some(timeout))?;
        tcp.set_write_timeout(Some(timeout))?;

        unsafe {
            let method = TLS_client_method();
            if method.is_null() {
                return Err(tls_err("TLS_client_method failed"));
            }
            let ctx = SSL_CTX_new(method);
            if ctx.is_null() {
                return Err(tls_err("SSL_CTX_new failed"));
            }
            // Load the system trust store and require peer verification.
            if SSL_CTX_set_default_verify_paths(ctx) != 1 {
                SSL_CTX_free(ctx);
                return Err(tls_err("failed to load system CA store"));
            }
            SSL_CTX_set_verify(ctx, SSL_VERIFY_PEER, std::ptr::null());

            let ssl = SSL_new(ctx);
            if ssl.is_null() {
                SSL_CTX_free(ctx);
                return Err(tls_err("SSL_new failed"));
            }

            // SNI: tell the server which host we want (required by most CDNs).
            let host_c = CString::new(host).map_err(|_| tls_err("invalid host"))?;
            SSL_ctrl(
                ssl,
                SSL_CTRL_SET_TLSEXT_HOSTNAME,
                TLSEXT_NAMETYPE_HOST_NAME,
                host_c.as_ptr() as *mut c_void,
            );
            // Enable built-in hostname verification against the cert.
            if SSL_set1_host(ssl, host_c.as_ptr()) != 1 {
                SSL_free(ssl);
                SSL_CTX_free(ctx);
                return Err(tls_err("failed to set verification hostname"));
            }

            if SSL_set_fd(ssl, tcp.as_raw_fd()) != 1 {
                SSL_free(ssl);
                SSL_CTX_free(ctx);
                return Err(tls_err("SSL_set_fd failed"));
            }

            let rc = SSL_connect(ssl);
            if rc != 1 {
                let e = SSL_get_error(ssl, rc);
                SSL_free(ssl);
                SSL_CTX_free(ctx);
                return Err(tls_err(&format!("TLS handshake failed (ssl error {})", e)));
            }

            // Final gate: certificate + hostname must have verified OK.
            let verify = SSL_get_verify_result(ssl);
            if verify != X509_V_OK {
                SSL_free(ssl);
                SSL_CTX_free(ctx);
                return Err(tls_err(&format!(
                    "certificate verification failed (code {})",
                    verify
                )));
            }

            Ok(TlsStream { ssl, ctx, _tcp: tcp })
        }
    }
}

impl Read for TlsStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let n = unsafe {
                SSL_read(self.ssl, buf.as_mut_ptr() as *mut c_void, buf.len() as c_int)
            };
            if n > 0 {
                return Ok(n as usize);
            }
            let err = unsafe { SSL_get_error(self.ssl, n) };
            match err {
                // 6 = SSL_ERROR_ZERO_RETURN (clean close), 5 = SYSCALL at EOF.
                6 => return Ok(0),
                SSL_ERROR_WANT_READ | SSL_ERROR_WANT_WRITE => continue,
                _ => return Ok(0), // treat as EOF; HTTP layer handles short reads
            }
        }
    }
}

impl Write for TlsStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = unsafe {
            SSL_write(self.ssl, buf.as_ptr() as *const c_void, buf.len() as c_int)
        };
        if n > 0 {
            Ok(n as usize)
        } else {
            Err(tls_err("SSL_write failed"))
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for TlsStream {
    fn drop(&mut self) {
        unsafe {
            if !self.ssl.is_null() {
                SSL_shutdown(self.ssl);
                SSL_free(self.ssl);
            }
            if !self.ctx.is_null() {
                SSL_CTX_free(self.ctx);
            }
        }
    }
}

fn tls_err(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Other, format!("tls: {}", msg))
}
