//! HTTPS with a certificate RustyBox makes for itself.
//!
//! Browsers only let a page pick a folder on the user's computer (to share the Xbox drive) when
//! the page is secure. The certificate is self-signed, so the browser asks once per computer
//! whether to continue; it is made on first start and kept in the config folder.

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use crate::error::Error;

fn dir(config: &Path) -> PathBuf {
    config.join("tls")
}

/// Names the certificate should be valid for: this machine, `localhost`, and anything in
/// `RUSTYBOX_TLS_NAMES` (comma separated names or addresses, such as the server's LAN address).
fn names() -> Vec<String> {
    let mut v = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    if let Ok(h) = std::fs::read_to_string("/etc/hostname") {
        let h = h.trim();
        if !h.is_empty() {
            v.push(h.to_string());
        }
    }
    if let Ok(extra) = std::env::var("RUSTYBOX_TLS_NAMES") {
        v.extend(
            extra
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        );
    }
    v.sort();
    v.dedup();
    v
}

/// The certificate and key files, made now if they aren't there (or if the names changed).
pub fn ensure(config: &Path) -> Result<(PathBuf, PathBuf), Error> {
    let d = dir(config);
    let (cert, key, list) = (d.join("cert.pem"), d.join("key.pem"), d.join("names.txt"));
    let names = names();
    let same = std::fs::read_to_string(&list).is_ok_and(|s| s == names.join("\n"));
    if cert.is_file() && key.is_file() && same {
        return Ok((cert, key));
    }
    std::fs::create_dir_all(&d)?;
    let made = rcgen::generate_simple_self_signed(names.clone())
        .map_err(|e| Error::backend(format!("Couldn't make a certificate: {e}")))?;
    std::fs::write(&cert, made.cert.pem())?;
    std::fs::write(&key, made.key_pair.serialize_pem())?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::write(&list, names.join("\n"))?;
    Ok((cert, key))
}

/// Serve `app` over HTTPS until the process stops. A failure is reported and otherwise ignored:
/// plain HTTP keeps working.
pub async fn serve(config: PathBuf, bind: SocketAddr, app: axum::Router) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (cert, key) = match ensure(&config) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("HTTPS is off: {e}");
            return;
        }
    };
    let tls = match axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert, &key).await {
        Ok(t) => t,
        Err(e) => {
            eprintln!("HTTPS is off: can't read the certificate: {e}");
            return;
        }
    };
    eprintln!("RustyBox web UI also on https://{bind} (self-signed certificate)");
    if let Err(e) = axum_server::bind_rustls(bind, tls)
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .await
    {
        eprintln!("HTTPS stopped: {e}");
    }
}
