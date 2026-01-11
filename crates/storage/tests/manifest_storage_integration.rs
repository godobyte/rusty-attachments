//! Integration tests for manifest storage operations.
//!
//! These tests use a mock StorageClient to verify the manifest upload/download
//! logic without requiring actual S3 access.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rusty_attachments_model::{
    v2023_03_03::{AssetManifest, ManifestPath},
    Manifest,
};
use rusty_attachments_storage::{
    discover_output_manifest_keys, download_manifest, download_manifests_parallel,
    download_output_manifests_by_asset_root, filter_output_manifest_objects,
    find_manifests_by_session_action_id, match_manifests_to_roots, upload_input_manifest,
    upload_step_output_manifest, upload_task_output_manifest, JobAttachmentRoot,
    ManifestDownloadOptions, ManifestLocation, ObjectInfo, ObjectMetadata,
    OutputManifestDiscoveryOptions, OutputManifestScope, ProgressCallback, StepOutputManifestPath,
    StorageClient, StorageError, TaskOutputManifestPath,
};

/// Mock storage client for testing.
#[derive(Debug, Clone, Default)]
struct MockStorageClient {
    /// Stored objects: bucket -> key -> (data, metadata)
    objects: Arc<Mutex<HashMap<String, HashMap<String, (Vec<u8>, HashMap<String, String>)>>>>,
    /// Expected bucket owner for validation
    expected_owner: Option<String>,
}

impl MockStorageClient {
    fn new() -> Self {
        Self {
            objects: Arc::new(Mutex::new(HashMap::new())),
            expected_owner: None,
        }
    }

    fn get_stored_keys(&self, bucket: &str) -> Vec<String> {
        let objects = self.objects.lock().unwrap();
        objects
            .get(bucket)
            .map(|b| b.keys().cloned().collect())
            .unwrap_or_default()
    }
}

#[async_trait]
impl StorageClient for MockStorageClient {
    fn expected_bucket_owner(&self) -> Option<&str> {
        self.expected_owner.as_deref()
    }

    async fn head_object(&self, bucket: &str, key: &str) -> Result<Option<u64>, StorageError> {
        let objects = self.objects.lock().unwrap();
        Ok(objects
            .get(bucket)
            .and_then(|b| b.get(key))
            .map(|(data, _)| data.len() as u64))
    }

    async fn head_object_with_metadata(
        &self,
        bucket: &str,
        key: &str,
    ) -> Result<Option<ObjectMetadata>, StorageError> {
        let objects = self.objects.lock().unwrap();
        Ok(objects
            .get(bucket)
            .and_then(|b| b.get(key))
            .map(|(data, metadata)| ObjectMetadata {
                size: data.len() as u64,
                last_modified: Some(1000),
                content_type: Some("application/json".to_string()),
                etag: None,
                user_metadata: metadata.clone(),
            }))
    }

    async fn put_object(
        &self,
        bucket: &str,
        key: &str,
        data: &[u8],
        _content_type: Option<&str>,
        metadata: Option<&HashMap<String, String>>,
    ) -> Result<(), StorageError> {
        let mut objects = self.objects.lock().unwrap();
        let bucket_map = objects.entry(bucket.to_string()).or_default();
        bucket_map.insert(
            key.to_string(),
            (data.to_vec(), metadata.cloned().unwrap_or_default()),
        );
        Ok(())
    }

    async fn put_object_from_file(
        &self,
        _bucket: &str,
        _key: &str,
        _file_path: &str,
        _content_type: Option<&str>,
        _metadata: Option<&HashMap<String, String>>,
        _progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError> {
        unimplemented!("Not needed for manifest tests")
    }

    async fn put_object_from_file_range(
        &self,
        _bucket: &str,
        _key: &str,
        _file_path: &str,
        _offset: u64,
        _length: u64,
        _progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError> {
        unimplemented!("Not needed for manifest tests")
    }

    async fn get_object(&self, bucket: &str, key: &str) -> Result<Vec<u8>, StorageError> {
        let objects = self.objects.lock().unwrap();
        objects
            .get(bucket)
            .and_then(|b| b.get(key))
            .map(|(data, _)| data.clone())
            .ok_or_else(|| StorageError::NotFound {
                bucket: bucket.to_string(),
                key: key.to_string(),
            })
    }

    async fn get_object_to_file(
        &self,
        _bucket: &str,
        _key: &str,
        _file_path: &str,
        _progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError> {
        unimplemented!("Not needed for manifest tests")
    }

    async fn get_object_to_file_offset(
        &self,
        _bucket: &str,
        _key: &str,
        _file_path: &str,
        _offset: u64,
        _progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError> {
        unimplemented!("Not needed for manifest tests")
    }

    async fn list_objects(
        &self,
        bucket: &str,
        prefix: &str,
    ) -> Result<Vec<ObjectInfo>, StorageError> {
        let objects = self.objects.lock().unwrap();
        let result = objects
            .get(bucket)
            .map(|b| {
                b.iter()
                    .filter(|(k, _)| k.starts_with(prefix))
                    .map(|(k, (data, _))| ObjectInfo {
                        key: k.clone(),
                        size: data.len() as u64,
                        last_modified: Some(1000),
                        etag: None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(result)
    }
}

// ============================================================================
// Upload Tests
// ============================================================================

#[tokio::test]
async fn test_upload_input_manifest() {
    let client = MockStorageClient::new();
    let location = ManifestLocation::new("test-bucket", "DeadlineCloud", "farm-123", "queue-456");

    let paths = vec![
        ManifestPath::new("file1.txt", "hash1", 100, 1000),
        ManifestPath::new("file2.txt", "hash2", 200, 2000),
    ];
    let manifest = Manifest::V2023_03_03(AssetManifest::new(paths));

    let result = upload_input_manifest(&client, &location, &manifest, "/project/assets", None)
        .await
        .unwrap();

    // Verify the key format
    assert!(result
        .s3_key
        .starts_with("DeadlineCloud/Manifests/farm-123/queue-456/Inputs/"));
    assert!(result.s3_key.ends_with("_input"));

    // Verify the object was stored
    let keys = client.get_stored_keys("test-bucket");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0], result.s3_key);
}

#[tokio::test]
async fn test_upload_task_output_manifest() {
    let client = MockStorageClient::new();
    let location = ManifestLocation::new("test-bucket", "DeadlineCloud", "farm-123", "queue-456");

    let paths = vec![ManifestPath::new("output.txt", "hash1", 100, 1000)];
    let manifest = Manifest::V2023_03_03(AssetManifest::new(paths));

    let output_path = TaskOutputManifestPath {
        job_id: "job-789".to_string(),
        step_id: "step-abc".to_string(),
        task_id: "task-def".to_string(),
        session_action_id: "sessionaction-xyz-123".to_string(),
        timestamp: 1716414026.409012,
    };

    let result = upload_task_output_manifest(
        &client,
        &location,
        &output_path,
        &manifest,
        "/project/outputs",
        None,
    )
    .await
    .unwrap();

    // Verify the key format contains all expected components
    assert!(result.s3_key.contains("farm-123"));
    assert!(result.s3_key.contains("queue-456"));
    assert!(result.s3_key.contains("job-789"));
    assert!(result.s3_key.contains("step-abc"));
    assert!(result.s3_key.contains("task-def"));
    assert!(result.s3_key.contains("sessionaction-xyz-123"));
    assert!(result.s3_key.ends_with("_output"));
}

#[tokio::test]
async fn test_upload_step_output_manifest() {
    let client = MockStorageClient::new();
    let location = ManifestLocation::new("test-bucket", "DeadlineCloud", "farm-123", "queue-456");

    let paths = vec![ManifestPath::new("output.txt", "hash1", 100, 1000)];
    let manifest = Manifest::V2023_03_03(AssetManifest::new(paths));

    let output_path = StepOutputManifestPath {
        job_id: "job-789".to_string(),
        step_id: "step-abc".to_string(),
        session_action_id: "sessionaction-xyz-123".to_string(),
        timestamp: 1716414026.409012,
    };

    let result = upload_step_output_manifest(
        &client,
        &location,
        &output_path,
        &manifest,
        "/project/outputs",
        None,
    )
    .await
    .unwrap();

    // Verify the key format - should NOT contain task_id
    assert!(result.s3_key.contains("step-abc"));
    assert!(!result.s3_key.contains("task-"));
    assert!(result.s3_key.ends_with("_output"));
}

#[tokio::test]
async fn test_upload_with_file_system_location() {
    let client = MockStorageClient::new();
    let location = ManifestLocation::new("test-bucket", "DeadlineCloud", "farm-123", "queue-456");

    let paths = vec![ManifestPath::new("file.txt", "hash1", 100, 1000)];
    let manifest = Manifest::V2023_03_03(AssetManifest::new(paths));

    let result = upload_input_manifest(
        &client,
        &location,
        &manifest,
        "/project/assets",
        Some("SharedDrive"),
    )
    .await
    .unwrap();

    // Verify metadata was stored
    let objects = client.objects.lock().unwrap();
    let (_, metadata) = objects
        .get("test-bucket")
        .unwrap()
        .get(&result.s3_key)
        .unwrap();
    assert_eq!(
        metadata.get("file-system-location-name"),
        Some(&"SharedDrive".to_string())
    );
}

// ============================================================================
// Download Tests
// ============================================================================

#[tokio::test]
async fn test_download_manifest_roundtrip() {
    let client = MockStorageClient::new();
    let location = ManifestLocation::new("test-bucket", "DeadlineCloud", "farm-123", "queue-456");

    // Upload a manifest
    let paths = vec![
        ManifestPath::new("file1.txt", "hash1", 100, 1000),
        ManifestPath::new("file2.txt", "hash2", 200, 2000),
    ];
    let original = Manifest::V2023_03_03(AssetManifest::new(paths));

    let upload_result =
        upload_input_manifest(&client, &location, &original, "/project/assets", None)
            .await
            .unwrap();

    // Download it back
    let (downloaded, metadata) = download_manifest(&client, "test-bucket", &upload_result.s3_key)
        .await
        .unwrap();

    // Verify content
    assert_eq!(downloaded.file_count(), 2);
    assert_eq!(downloaded.total_size(), 300);
    assert_eq!(metadata.asset_root, "/project/assets");
}

// ============================================================================
// Discovery Tests
// ============================================================================

#[tokio::test]
async fn test_discover_output_manifest_keys() {
    let client = MockStorageClient::new();
    let location = ManifestLocation::new("test-bucket", "DeadlineCloud", "farm-123", "queue-456");

    // Upload some output manifests
    let paths = vec![ManifestPath::new("output.txt", "hash1", 100, 1000)];
    let manifest = Manifest::V2023_03_03(AssetManifest::new(paths));

    // Task 1, session 1
    let output_path1 = TaskOutputManifestPath {
        job_id: "job-789".to_string(),
        step_id: "step-abc".to_string(),
        task_id: "task-001".to_string(),
        session_action_id: "sessionaction-aaa-111".to_string(),
        timestamp: 1000.0,
    };
    upload_task_output_manifest(&client, &location, &output_path1, &manifest, "/root1", None)
        .await
        .unwrap();

    // Task 1, session 2 (later)
    let output_path2 = TaskOutputManifestPath {
        job_id: "job-789".to_string(),
        step_id: "step-abc".to_string(),
        task_id: "task-001".to_string(),
        session_action_id: "sessionaction-bbb-222".to_string(),
        timestamp: 2000.0,
    };
    upload_task_output_manifest(&client, &location, &output_path2, &manifest, "/root1", None)
        .await
        .unwrap();

    // Discover with select_latest_per_task = false (get all)
    let options = OutputManifestDiscoveryOptions {
        scope: OutputManifestScope::Step {
            job_id: "job-789".to_string(),
            step_id: "step-abc".to_string(),
        },
        select_latest_per_task: false,
    };

    let keys = discover_output_manifest_keys(&client, &location, &options)
        .await
        .unwrap();
    assert_eq!(keys.len(), 2);
}

#[tokio::test]
async fn test_filter_output_manifest_objects() {
    let objects = vec![
        ObjectInfo {
            key: "path/to/hash123_output".to_string(),
            size: 100,
            last_modified: Some(1000),
            etag: None,
        },
        ObjectInfo {
            key: "path/to/hash456_output.json".to_string(),
            size: 200,
            last_modified: Some(2000),
            etag: None,
        },
        ObjectInfo {
            key: "path/to/some_other_file.txt".to_string(),
            size: 50,
            last_modified: Some(500),
            etag: None,
        },
        ObjectInfo {
            key: "path/to/hash789_input".to_string(),
            size: 150,
            last_modified: Some(1500),
            etag: None,
        },
    ];

    let filtered = filter_output_manifest_objects(&objects);
    assert_eq!(filtered.len(), 2);
    assert!(filtered.iter().any(|o| o.key.contains("hash123_output")));
    assert!(filtered.iter().any(|o| o.key.contains("hash456_output")));
}

// ============================================================================
// Matching Tests
// ============================================================================

#[tokio::test]
async fn test_match_manifests_to_roots() {
    let client = MockStorageClient::new();
    let location = ManifestLocation::new("test-bucket", "DeadlineCloud", "farm-123", "queue-456");

    let paths = vec![ManifestPath::new("output.txt", "hash1", 100, 1000)];
    let manifest = Manifest::V2023_03_03(AssetManifest::new(paths));

    // Upload manifests for two different roots
    let output_path1 = TaskOutputManifestPath {
        job_id: "job-789".to_string(),
        step_id: "step-abc".to_string(),
        task_id: "task-001".to_string(),
        session_action_id: "sessionaction-aaa-111".to_string(),
        timestamp: 1000.0,
    };
    let result1 =
        upload_task_output_manifest(&client, &location, &output_path1, &manifest, "/root1", None)
            .await
            .unwrap();

    let output_path2 = TaskOutputManifestPath {
        job_id: "job-789".to_string(),
        step_id: "step-abc".to_string(),
        task_id: "task-001".to_string(),
        session_action_id: "sessionaction-aaa-111".to_string(),
        timestamp: 1000.0,
    };
    let result2 =
        upload_task_output_manifest(&client, &location, &output_path2, &manifest, "/root2", None)
            .await
            .unwrap();

    // Define job attachment roots
    let roots = vec![
        JobAttachmentRoot {
            root_path: "/root1".to_string(),
            file_system_location_name: None,
        },
        JobAttachmentRoot {
            root_path: "/root2".to_string(),
            file_system_location_name: None,
        },
    ];

    // Match manifests to roots
    let manifest_keys = vec![result1.s3_key.clone(), result2.s3_key.clone()];
    let matched = match_manifests_to_roots(&manifest_keys, &roots).unwrap();

    // Should have one session action with both roots matched
    assert_eq!(matched.len(), 1);
    let session_manifests = matched.get("sessionaction-aaa-111").unwrap();
    assert_eq!(session_manifests.len(), 2);
    assert!(session_manifests[0].is_some());
    assert!(session_manifests[1].is_some());
}

// ============================================================================
// Session Action ID Lookup Tests
// ============================================================================

#[tokio::test]
async fn test_find_manifests_by_session_action_id() {
    let client = MockStorageClient::new();
    let location = ManifestLocation::new("test-bucket", "DeadlineCloud", "farm-123", "queue-456");

    let paths = vec![ManifestPath::new("output.txt", "hash1", 100, 1000)];
    let manifest = Manifest::V2023_03_03(AssetManifest::new(paths));

    // Upload a manifest with specific session action ID
    let output_path = TaskOutputManifestPath {
        job_id: "job-789".to_string(),
        step_id: "step-abc".to_string(),
        task_id: "task-def".to_string(),
        session_action_id: "sessionaction-target-999".to_string(),
        timestamp: 1000.0,
    };
    upload_task_output_manifest(
        &client,
        &location,
        &output_path,
        &manifest,
        "/project/outputs",
        None,
    )
    .await
    .unwrap();

    // Upload another manifest with different session action ID
    let output_path2 = TaskOutputManifestPath {
        job_id: "job-789".to_string(),
        step_id: "step-abc".to_string(),
        task_id: "task-def".to_string(),
        session_action_id: "sessionaction-other-888".to_string(),
        timestamp: 2000.0,
    };
    upload_task_output_manifest(
        &client,
        &location,
        &output_path2,
        &manifest,
        "/project/outputs",
        None,
    )
    .await
    .unwrap();

    // Find by specific session action ID
    let found = find_manifests_by_session_action_id(
        &client,
        &location,
        "job-789",
        "step-abc",
        "task-def",
        "sessionaction-target-999",
    )
    .await
    .unwrap();

    assert_eq!(found.len(), 1);
    assert!(found[0].contains("sessionaction-target-999"));
}

// ============================================================================
// Download by Asset Root Tests
// ============================================================================

#[tokio::test]
async fn test_download_output_manifests_by_asset_root() {
    let client = MockStorageClient::new();
    let location = ManifestLocation::new("test-bucket", "DeadlineCloud", "farm-123", "queue-456");

    // Upload manifests for different asset roots
    let paths1 = vec![ManifestPath::new("file1.txt", "hash1", 100, 1000)];
    let manifest1 = Manifest::V2023_03_03(AssetManifest::new(paths1));

    let paths2 = vec![ManifestPath::new("file2.txt", "hash2", 200, 2000)];
    let manifest2 = Manifest::V2023_03_03(AssetManifest::new(paths2));

    let output_path1 = TaskOutputManifestPath {
        job_id: "job-789".to_string(),
        step_id: "step-abc".to_string(),
        task_id: "task-001".to_string(),
        session_action_id: "sessionaction-aaa-111".to_string(),
        timestamp: 1000.0,
    };
    upload_task_output_manifest(
        &client,
        &location,
        &output_path1,
        &manifest1,
        "/root/assets",
        None,
    )
    .await
    .unwrap();

    let output_path2 = TaskOutputManifestPath {
        job_id: "job-789".to_string(),
        step_id: "step-abc".to_string(),
        task_id: "task-002".to_string(),
        session_action_id: "sessionaction-bbb-222".to_string(),
        timestamp: 2000.0,
    };
    upload_task_output_manifest(
        &client,
        &location,
        &output_path2,
        &manifest2,
        "/root/textures",
        None,
    )
    .await
    .unwrap();

    // Download and group by asset root
    let discovery_options = OutputManifestDiscoveryOptions {
        scope: OutputManifestScope::Step {
            job_id: "job-789".to_string(),
            step_id: "step-abc".to_string(),
        },
        select_latest_per_task: false,
    };

    let by_root = download_output_manifests_by_asset_root(
        &client,
        &location,
        &discovery_options,
        &ManifestDownloadOptions::default(),
    )
    .await
    .unwrap();

    assert_eq!(by_root.len(), 2);
    assert!(by_root.contains_key("/root/assets"));
    assert!(by_root.contains_key("/root/textures"));
}

// ============================================================================
// Custom Download Options Tests
// ============================================================================

#[tokio::test]
async fn test_download_with_custom_concurrency() {
    let client = MockStorageClient::new();
    let location = ManifestLocation::new("test-bucket", "DeadlineCloud", "farm-123", "queue-456");

    // Upload a manifest
    let paths = vec![ManifestPath::new("file1.txt", "hash1", 100, 1000)];
    let manifest = Manifest::V2023_03_03(AssetManifest::new(paths));

    let output_path = TaskOutputManifestPath {
        job_id: "job-789".to_string(),
        step_id: "step-abc".to_string(),
        task_id: "task-001".to_string(),
        session_action_id: "sessionaction-aaa-111".to_string(),
        timestamp: 1000.0,
    };
    upload_task_output_manifest(
        &client,
        &location,
        &output_path,
        &manifest,
        "/root/assets",
        None,
    )
    .await
    .unwrap();

    // Download with custom concurrency options
    let discovery_options = OutputManifestDiscoveryOptions {
        scope: OutputManifestScope::Step {
            job_id: "job-789".to_string(),
            step_id: "step-abc".to_string(),
        },
        select_latest_per_task: false,
    };

    let download_options = ManifestDownloadOptions::default().with_max_concurrency(5);

    let by_root = download_output_manifests_by_asset_root(
        &client,
        &location,
        &discovery_options,
        &download_options,
    )
    .await
    .unwrap();

    assert_eq!(by_root.len(), 1);
    assert!(by_root.contains_key("/root/assets"));
}

#[tokio::test]
async fn test_download_manifests_parallel_with_options() {
    let client = MockStorageClient::new();
    let location = ManifestLocation::new("test-bucket", "DeadlineCloud", "farm-123", "queue-456");

    // Upload multiple manifests
    let paths = vec![ManifestPath::new("file.txt", "hash1", 100, 1000)];
    let manifest = Manifest::V2023_03_03(AssetManifest::new(paths));

    let mut keys = Vec::new();
    for i in 0..3 {
        let output_path = TaskOutputManifestPath {
            job_id: "job-789".to_string(),
            step_id: "step-abc".to_string(),
            task_id: format!("task-{:03}", i),
            session_action_id: format!("sessionaction-{:03}-111", i),
            timestamp: 1000.0 + i as f64,
        };
        let result = upload_task_output_manifest(
            &client,
            &location,
            &output_path,
            &manifest,
            "/root/assets",
            None,
        )
        .await
        .unwrap();
        keys.push(result.s3_key);
    }

    // Download with custom options
    let options = ManifestDownloadOptions::new().with_max_concurrency(2);
    let downloaded = download_manifests_parallel(&client, "test-bucket", &keys, &options)
        .await
        .unwrap();

    assert_eq!(downloaded.len(), 3);
    for dm in &downloaded {
        assert_eq!(dm.asset_root, "/root/assets");
    }
}
