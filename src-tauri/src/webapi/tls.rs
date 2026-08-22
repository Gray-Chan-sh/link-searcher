use std::path::PathBuf;

pub fn ensure_cert(data_dir: &std::path::Path) -> Result<(PathBuf, PathBuf), String> {
    let tls_dir = data_dir.join("tls");
    std::fs::create_dir_all(&tls_dir).map_err(|e| format!("tls dir: {e}"))?;

    let cert_path = tls_dir.join("cert.pem");
    let key_path = tls_dir.join("key.pem");

    if cert_path.exists() && key_path.exists() {
        return Ok((cert_path, key_path));
    }

    let key_pair = rcgen::KeyPair::generate().map_err(|e| format!("keygen: {e}"))?;
    let params = rcgen::CertificateParams::new(vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
    ]).map_err(|e| format!("cert params: {e}"))?;
    let cert = params.self_signed(&key_pair).map_err(|e| format!("self_signed: {e}"))?;

    std::fs::write(&cert_path, cert.pem())
        .map_err(|e| format!("write cert: {e}"))?;
    std::fs::write(&key_path, key_pair.serialize_pem())
        .map_err(|e| format!("write key: {e}"))?;

    log::info!("[WEBAPI] generated self-signed TLS cert at {:?}", cert_path);
    Ok((cert_path, key_path))
}
