use crate::error::NetworkError;
use crate::types::*;
use reqwest::Client as HttpClient;

/// HTTP client for communicating with a LooperNet cloud server.
#[derive(Debug, Clone)]
pub struct NetworkClient {
    base_url: String,
    token: Option<String>,
    http: HttpClient,
}

impl NetworkClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        NetworkClient { base_url: base_url.into(), token: None, http: HttpClient::new() }
    }

    pub fn with_token(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        NetworkClient { base_url: base_url.into(), token: Some(token.into()), http: HttpClient::new() }
    }

    pub fn set_token(&mut self, token: impl Into<String>) {
        self.token = Some(token.into());
    }

    fn auth_header(&self) -> Option<(&str, String)> {
        self.token.as_ref().map(|t| ("Authorization", format!("Bearer {}", t)))
    }

    async fn post<T: serde::Serialize, R: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<R, NetworkError> {
        let mut req = self.http.post(format!("{}{}", self.base_url, path)).json(body);
        if let Some((key, val)) = self.auth_header() {
            req = req.header(key, val);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let msg = resp.json::<ApiErrorResponse>().await.map(|e| e.message).unwrap_or_default();
            return Err(match status.as_u16() {
                401 => NetworkError::Unauthorized(msg),
                412 => NetworkError::StaleLeaseToken,
                _ => NetworkError::Api { status: status.as_u16(), message: msg },
            });
        }
        Ok(resp.json::<R>().await?)
    }

    async fn get<R: serde::de::DeserializeOwned>(&self, path: &str) -> Result<R, NetworkError> {
        let mut req = self.http.get(format!("{}{}", self.base_url, path));
        if let Some((key, val)) = self.auth_header() {
            req = req.header(key, val);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let msg = resp.json::<ApiErrorResponse>().await.map(|e| e.message).unwrap_or_default();
            return Err(match status.as_u16() {
                401 => NetworkError::Unauthorized(msg),
                _ => NetworkError::Api { status: status.as_u16(), message: msg },
            });
        }
        Ok(resp.json::<R>().await?)
    }

    pub async fn join(&self, req: &JoinRequest) -> Result<JoinResponse, NetworkError> {
        self.post("/v1/join", req).await
    }

    pub async fn heartbeat(&self, req: &HeartbeatRequest) -> Result<HeartbeatResponse, NetworkError> {
        self.post("/v1/heartbeat", req).await
    }

    pub async fn leave(&self) -> Result<(), NetworkError> {
        self.post::<_, serde_json::Value>("/v1/leave", &serde_json::json!({})).await?;
        Ok(())
    }

    pub async fn status(&self) -> Result<NodeStatusResponse, NetworkError> {
        self.get("/v1/status").await
    }

    pub async fn acquire_lease(&self, req: &CoordinatorLeaseAcquireRequest) -> Result<CoordinatorLease, NetworkError> {
        self.post("/v1/coordinator-lease/acquire", req).await
    }

    pub async fn renew_lease(&self, req: &CoordinatorLeaseRenewRequest) -> Result<CoordinatorLease, NetworkError> {
        self.post("/v1/coordinator-lease/renew", req).await
    }

    pub async fn handoff_lease(&self, req: &CoordinatorLeaseHandoffRequest) -> Result<CoordinatorLease, NetworkError> {
        self.post("/v1/coordinator-lease/handoff", req).await
    }

    pub async fn expire_lease(&self, req: &FencingTokenRequest) -> Result<CoordinatorLease, NetworkError> {
        self.post("/v1/coordinator-lease/expire", req).await
    }

    pub async fn revalidate_lease(&self, req: &CoordinatorLeaseRevalidateRequest) -> Result<(), NetworkError> {
        self.post::<_, serde_json::Value>("/v1/coordinator-lease/revalidate", req).await?;
        Ok(())
    }

    pub async fn webhook_secret(&self) -> Result<String, NetworkError> {
        let resp: WebhookSecretResponse = self.get("/v1/github/webhook-secret").await?;
        Ok(resp.secret)
    }

    pub async fn admin_create_join_key(&self) -> Result<String, NetworkError> {
        let resp: JoinKeyResponse = self.post("/v1/join-keys", &serde_json::json!({})).await?;
        Ok(resp.join_key)
    }
}
