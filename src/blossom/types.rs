use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobDescriptor {
    pub url: String,
    pub sha256: String,
    pub size: u64,
    #[serde(rename = "type")]
    pub content_type: Option<String>,
    pub uploaded: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadRequirements {
    #[serde(rename = "X-SHA-256")]
    pub sha256: Option<String>,
    #[serde(rename = "X-Content-Length")]
    pub content_length: Option<u64>,
    #[serde(rename = "X-Content-Type")]
    pub content_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthEvent {
    pub id: String,
    pub kind: u64,
    pub pubkey: String,
    #[serde(rename = "created_at")]
    pub created_at: u64,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: String,
}

impl AuthEvent {
    pub fn to_base64(&self) -> Result<String, crate::error::BlossomLfsError> {
        let json =
            serde_json::to_string(self).map_err(crate::error::BlossomLfsError::Serialization)?;
        Ok(base64_url::encode(&json))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blob_descriptor_deserialization() {
        let json = r#"{
            "url": "https://cdn.example.com/abc123.pdf",
            "sha256": "abc123",
            "size": 1024,
            "type": "application/pdf",
            "uploaded": 1234567890
        }"#;

        let descriptor: BlobDescriptor = serde_json::from_str(json).unwrap();
        assert_eq!(descriptor.sha256, "abc123");
        assert_eq!(descriptor.size, 1024);
    }
}
