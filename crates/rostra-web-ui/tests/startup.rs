use std::path::Path;

use rostra_client::Client;
use rostra_client::multiclient::MultiClient;
use rostra_util_bind_addr::BindAddr;
use rostra_web_ui::{Opts, UiServer, WebUiServerError};

async fn start(data_dir: &Path) -> Result<UiServer, WebUiServerError> {
    let clients = MultiClient::new(
        data_dir.to_path_buf(),
        10,
        false,
        Client::make_pkarr_client().expect("create pkarr client"),
    );
    let opts = Opts::new(
        BindAddr::Tcp(([127, 0, 0, 1], 0).into()),
        None,
        None,
        false,
        data_dir.to_path_buf(),
        None,
        10,
        None,
    )
    .with_source_assets();
    rostra_web_ui::start_ui(opts, clients).await
}

#[tokio::test]
async fn first_run_creates_nested_data_directory_and_can_restart() {
    let temp_dir = tempfile::tempdir().unwrap();
    let data_dir = temp_dir.path().join("missing/rostra");
    assert!(!data_dir.exists());

    let server = start(&data_dir).await.expect("first-run startup");
    assert!(data_dir.join("webui.redb").is_file());
    let response = reqwest::get(format!("http://{}/", server.local_addr()))
        .await
        .expect("serve first-run HTTP request");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    server.shutdown().await.unwrap();

    let server = start(&data_dir).await.expect("restart with existing store");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn data_directory_failure_preserves_path_and_io_error() {
    let temp_dir = tempfile::tempdir().unwrap();
    let data_dir = temp_dir.path().join("rostra");
    std::fs::write(&data_dir, b"not a directory").unwrap();

    let error = match start(&data_dir).await {
        Ok(server) => {
            server.shutdown().await.unwrap();
            panic!("startup should reject a file as its data directory");
        }
        Err(error) => error,
    };
    match error {
        WebUiServerError::DataDir { path, source } => {
            assert_eq!(path, data_dir);
            assert_eq!(source.kind(), std::io::ErrorKind::AlreadyExists);
        }
        error => panic!("unexpected startup error: {error}"),
    }
    assert_eq!(std::fs::read(&data_dir).unwrap(), b"not a directory");
}
