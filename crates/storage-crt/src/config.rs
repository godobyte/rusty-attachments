//! Shared configuration for AWS S3 clients.

use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::config::Region;

use rusty_attachments_storage::{StorageError, StorageSettings};

/// Shared configuration for constructing AWS S3 clients.
///
/// Extracts common configuration logic (region, credentials, bucket owner)
/// so that both `CrtStorageClient` and `TransferManagerClient` can reuse it.
pub struct S3Config {
    /// The loaded AWS SDK configuration.
    pub sdk_config: aws_config::SdkConfig,
    /// Expected bucket owner for security validation.
    pub expected_bucket_owner: Option<String>,
}

impl S3Config {
    /// Build configuration from StorageSettings.
    ///
    /// # Arguments
    /// * `settings` - Storage settings with region and optional credentials
    ///
    /// # Returns
    /// Configured S3Config ready for client construction.
    pub async fn from_settings(settings: StorageSettings) -> Result<Self, StorageError> {
        let config_loader = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(settings.region.clone()));

        let config_loader = if let Some(ref creds) = settings.credentials {
            let credentials = Credentials::new(
                &creds.access_key_id,
                &creds.secret_access_key,
                creds.session_token.clone(),
                None,
                "rusty-attachments",
            );
            config_loader.credentials_provider(credentials)
        } else {
            config_loader
        };

        let sdk_config: aws_config::SdkConfig = config_loader.load().await;

        Ok(Self {
            sdk_config,
            expected_bucket_owner: settings.expected_bucket_owner,
        })
    }
}
