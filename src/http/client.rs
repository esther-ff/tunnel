use super::request::RequestFuture;
use crate::tls_client::TlsClient;
use std::collections::HashMap;

pub struct Client<'c> {
    tls: TlsClient<'c>,
    user_agent: &'static str,
    headers: HashMap<&'c str, String>,
}

impl Client<'_> {
    pub fn new() -> Self {
        // create
        Self {}
    }
}
