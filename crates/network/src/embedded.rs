use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
};

use axum::Router;
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder,
    service::TowerToHyperService,
};
use rcgen::{CertificateParams, KeyPair, SanType};
use rustls::{
    ServerConfig,
    pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer},
};
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_rustls::TlsAcceptor;

/// A signaling listener owned by the Branch process. It binds on all network
/// interfaces so guests can reach it through the host's router port mapping.
pub struct EmbeddedSignalServer {
    address: SocketAddr,
    certificate: Vec<u8>,
    task: JoinHandle<()>,
}

impl EmbeddedSignalServer {
    pub async fn bind(port: u16, public_host: &str, app: Router) -> Result<Self, String> {
        let host = public_host.trim().trim_matches(['[', ']']);
        if host.is_empty() || host.contains('/') || host.contains('@') || host.contains(':') {
            return Err("Введите имя хоста или IP-адрес без схемы и порта".into());
        }
        let listener = TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], port)))
            .await
            .map_err(|_| "Не удалось открыть порт для комнаты".to_owned())?;
        let address = listener
            .local_addr()
            .map_err(|_| "Не удалось определить порт".to_owned())?;

        let key = KeyPair::generate()
            .map_err(|_| "Не удалось создать ключ защиты соединения".to_owned())?;
        let mut names = vec![host.to_owned(), "localhost".to_owned()];
        if host == "localhost" {
            names.truncate(1);
        }
        let mut params = CertificateParams::new(names)
            .map_err(|_| "Некорректный адрес подключения".to_owned())?;
        params
            .subject_alt_names
            .push(SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        let cert = params
            .self_signed(&key)
            .map_err(|_| "Не удалось создать сертификат комнаты".to_owned())?;
        let certificate = cert.der().to_vec();
        let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let tls = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert.der().clone()], private_key)
            .map_err(|_| "Не удалось настроить защищённое соединение".to_owned())?;
        let acceptor = TlsAcceptor::from(Arc::new(tls));
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let acceptor = acceptor.clone();
                let app = app.clone();
                tokio::spawn(async move {
                    let Ok(stream) = acceptor.accept(stream).await else {
                        return;
                    };
                    let io = TokioIo::new(stream);
                    let service = TowerToHyperService::new(app);
                    let _ = Builder::new(TokioExecutor::new())
                        .serve_connection_with_upgrades(io, service)
                        .await;
                });
            }
        });
        Ok(Self {
            address,
            certificate,
            task,
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }
    pub fn certificate(&self) -> &[u8] {
        &self.certificate
    }
}

impl Drop for EmbeddedSignalServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
