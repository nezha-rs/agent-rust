use anyhow::Result;
use http::Uri;
use hyper_util::rt::TokioIo;
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
    DigitallySignedStruct, SignatureScheme,
};
use std::{io, sync::Arc};
use tokio_rustls::TlsConnector;
use tonic::transport::{Channel, Endpoint};
use tower::service_fn;

#[derive(Debug)]
struct SkipCertificateVerification;

impl ServerCertVerifier for SkipCertificateVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ED25519,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
        ]
    }
}

pub async fn connect(endpoint: Endpoint, dns: &[String]) -> Result<Channel> {
    let mut config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipCertificateVerification))
        .with_no_client_auth();
    config.alpn_protocols.push(b"h2".to_vec());
    let connector = TlsConnector::from(Arc::new(config));
    let servers = dns.to_vec();
    let channel = endpoint
        .connect_with_connector(service_fn(move |uri: Uri| {
            let connector = connector.clone();
            let servers = servers.clone();
            async move {
                let host = uri.host().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "missing TLS host")
                })?;
                let name = ServerName::try_from(host.to_owned()).map_err(|error| {
                    io::Error::new(io::ErrorKind::InvalidInput, error.to_string())
                })?;
                let authority = uri.authority().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "missing TLS authority")
                })?;
                let tcp = crate::platform::connect_tcp_host(
                    host,
                    authority.port_u16().unwrap_or(443),
                    &servers,
                )
                .await?;
                Ok::<_, io::Error>(TokioIo::new(connector.connect(name, tcp).await?))
            }
        }))
        .await?;
    Ok(channel)
}
