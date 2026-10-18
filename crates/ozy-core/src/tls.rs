//! Certificate inspection for the Website Intelligence API.
//! The connection is pinned to an address that `resolve_public` already validated,
//! and the chain is verified against the Mozilla root store.
use crate::{Error, net::resolve_public};
use serde::Serialize;
use std::{sync::Arc, time::Duration};
use tokio::net::TcpStream;
use tokio_rustls::{
    TlsConnector,
    rustls::{ClientConfig, RootCertStore, pki_types::ServerName},
};
use url::Url;
use x509_parser::prelude::*;

#[derive(Debug, Clone, Serialize)]
pub struct CertificateInfo {
    pub subject: String,
    pub issuer: String,
    pub serial: String,
    pub not_before: String,
    pub not_after: String,
    pub days_remaining: i64,
    pub subject_alt_names: Vec<String>,
    pub signature_algorithm: String,
    pub chain_length: usize,
    pub protocol: Option<String>,
    pub cipher_suite: Option<String>,
    pub verified: bool,
}

pub async fn inspect(url: &Url) -> Result<CertificateInfo, Error> {
    if url.scheme() != "https" {
        return Err(Error::Invalid("Certificate details require an https URL".into()));
    }
    let host = url.host_str().ok_or(Error::Forbidden)?.to_owned();
    let address = *resolve_public(url).await?.first().ok_or(Error::Forbidden)?;
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| Error::Upstream)?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = ServerName::try_from(host).map_err(|_| Error::Invalid("Invalid TLS server name".into()))?;
    let stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(address))
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(|_| Error::Upstream)?;
    let tls = tokio::time::timeout(Duration::from_secs(8), TlsConnector::from(Arc::new(config)).connect(name, stream))
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(|_| Error::Invalid("TLS handshake failed or certificate did not verify".into()))?;
    let (_, session) = tls.get_ref();
    let chain = session.peer_certificates().ok_or(Error::Upstream)?;
    let leaf = chain.first().ok_or(Error::Upstream)?;
    let (_, cert) = X509Certificate::from_der(leaf.as_ref()).map_err(|_| Error::Upstream)?;
    let validity = cert.validity();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let subject_alt_names = cert
        .subject_alternative_name()
        .ok()
        .flatten()
        .map(|ext| {
            ext.value
                .general_names
                .iter()
                .filter_map(|name| match name {
                    GeneralName::DNSName(dns) => Some((*dns).to_owned()),
                    _ => None,
                })
                .take(50)
                .collect()
        })
        .unwrap_or_default();
    Ok(CertificateInfo {
        subject: cert.subject().to_string(),
        issuer: cert.issuer().to_string(),
        serial: cert.raw_serial_as_string(),
        not_before: validity.not_before.to_string(),
        not_after: validity.not_after.to_string(),
        days_remaining: (validity.not_after.timestamp() - now) / 86_400,
        subject_alt_names,
        signature_algorithm: cert.signature_algorithm.algorithm.to_id_string(),
        chain_length: chain.len(),
        protocol: session.protocol_version().map(|v| format!("{v:?}")),
        cipher_suite: session.negotiated_cipher_suite().map(|s| format!("{:?}", s.suite())),
        verified: true,
    })
}
