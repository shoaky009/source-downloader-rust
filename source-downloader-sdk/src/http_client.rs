use crate::component::ComponentError;
use parking_lot::Mutex;
use rustls::crypto::CryptoProvider;
use rustls_platform_verifier::Verifier;
use std::sync::Arc;

static VERIFIER: Mutex<Option<Arc<Verifier>>> = Mutex::new(None);

fn shared_verifier(
    provider: Arc<CryptoProvider>,
) -> Result<Arc<Verifier>, rustls::Error> {
    let mut cached = VERIFIER.lock();
    if let Some(verifier) = cached.as_ref() {
        return Ok(verifier.clone());
    }
    // Serialize initialization so concurrent component creation loads roots only once.
    // Failed initialization is not cached, allowing a later attempt to retry.
    let verifier = Arc::new(Verifier::new(provider)?);
    *cached = Some(verifier.clone());
    Ok(verifier)
}

/// Creates an HTTP builder sharing platform certificate verification, while keeping
/// connection pools, TLS sessions, cookies and request settings client-local.
///
/// Uses a preconfigured rustls backend with HTTP/2 and HTTP/1.1 ALPN. Callers needing
/// custom TLS settings should use their own builder instead. Platform trust roots
/// are initialized on first use and retained for the lifetime of the process.
pub fn client_builder() -> Result<reqwest::ClientBuilder, ComponentError> {
    let provider = CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()));
    let verifier = shared_verifier(provider.clone()).map_err(tls_error)?;
    let mut tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(tls_error)?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    // Preconfigured TLS bypasses reqwest's default ALPN configuration.
    tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(reqwest::Client::builder().tls_backend_preconfigured(tls))
}

fn tls_error(error: rustls::Error) -> ComponentError {
    ComponentError::new(format!("Failed to initialize HTTP TLS verification: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_platform_verifier_is_shared() {
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let first = shared_verifier(provider.clone()).unwrap();
        let second = shared_verifier(provider).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
    }
}
