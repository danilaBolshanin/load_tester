use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::cli::HttpMethod;

#[derive(Debug, Deserialize, Serialize)]
pub struct UrlConfig {
    pub url: String,
    #[serde(default)]
    pub method: Option<HttpMethod>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default)]
    pub weight: Option<u32>,
    #[serde(default)]
    pub timeout: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct MultiTestConfig {
    #[serde(default)]
    pub name: Option<String>,
    pub urls: Vec<UrlConfig>,
    #[serde(default)]
    pub distribution: Option<String>,
    #[serde(default)]
    pub total_requests: Option<usize>,
    #[serde(default)]
    pub rps: Option<usize>,
    #[serde(default)]
    pub duration_seconds: Option<u64>,
    #[serde(default)]
    pub common_headers: Option<HashMap<String, String>>,
    #[serde(default)]
    pub common_body: Option<String>,
}

impl Default for MultiTestConfig {
    fn default() -> Self {
        Self {
            name: None,
            urls: Vec::new(),
            distribution: Some("round-robin".to_string()),
            total_requests: None,
            rps: Some(10),
            duration_seconds: Some(10),
            common_headers: None,
            common_body: None,
        }
    }
}