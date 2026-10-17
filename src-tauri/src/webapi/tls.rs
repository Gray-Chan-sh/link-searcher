use std::path::PathBuf;

pub fn ensure_cert(data_dir: &std::path::Path) -> Result<(PathBuf, PathBuf), String> {
    let tls_dir = data_dir.join("tls");
    std::fs::create_dir_all(&tls_dir).map_err(|e| format!("tls dir: {e}"))?;

    let cert_path = tls_dir.join("cert.pem");
    let key_path = tls_dir.join("key.pem");

    if cert_path.exists() && key_path.exists() {
        return Ok((cert_path, key_path));
    }

    let mut san_names = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
    ];

    if let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") {
        let _ = sock.connect("192.168.255.255:80");
        if let Ok(addr) = sock.local_addr() {
            let ip = addr.ip().to_string();
            if !san_names.contains(&ip) { san_names.push(ip); }
        }
    }
    if let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") {
        let _ = sock.connect("10.255.255.255:80");
        if let Ok(addr) = sock.local_addr() {
            let ip = addr.ip().to_string();
            if !san_names.contains(&ip) { san_names.push(ip); }
        }
    }
    if let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") {
        let _ = sock.connect("172.16.0.1:80");
        if let Ok(addr) = sock.local_addr() {
            let ip = addr.ip().to_string();
            if !san_names.contains(&ip) { san_names.push(ip); }
        }
    }
    // Tailscale / CGNAT 覆盖网地址：**动态取**，不再硬编码某台机器的 100.x 地址
    // （硬编码会随换机/换 Tailnet 失效；`tailscale_ip()` 只在确有 100.64/10 路由时给值）。
    if let Some(ts) = super::tailscale_ip()
        && !san_names.contains(&ts)
    {
        san_names.push(ts);
    }

    log::info!("[WEBAPI] generating TLS cert with SANs: {:?}", san_names);

    let key_pair = rcgen::KeyPair::generate().map_err(|e| format!("keygen: {e}"))?;
    let params = rcgen::CertificateParams::new(san_names)
        .map_err(|e| format!("cert params: {e}"))?;
    let cert = params.self_signed(&key_pair).map_err(|e| format!("self_signed: {e}"))?;

    std::fs::write(&cert_path, cert.pem())
        .map_err(|e| format!("write cert: {e}"))?;
    std::fs::write(&key_path, key_pair.serialize_pem())
        .map_err(|e| format!("write key: {e}"))?;

    log::info!("[WEBAPI] generated self-signed TLS cert at {:?}", cert_path);
    Ok((cert_path, key_path))
}
