use crate::app_data::OxenAppData;

use liboxen::command;
use liboxen::error::OxenError;
use liboxen::model::LocalRepository;

use serde::Serialize;
use std::borrow::Cow;
use std::path::{Path, PathBuf};

pub fn get_sync_dir() -> Result<PathBuf, OxenError> {
    let sync_dir = PathBuf::from(format!("data/test/runs/{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&sync_dir)?;
    Ok(sync_dir)
}

pub fn create_local_repo(sync_dir: &Path, name: &str) -> Result<LocalRepository, OxenError> {
    let repo_dir = sync_dir.join(name);
    std::fs::create_dir_all(&repo_dir)?;
    let repo = command::init(&repo_dir)?;
    Ok(repo)
}

pub fn run_empty_sync_dir_test<T>(test: T) -> Result<(), OxenError>
where
    T: FnOnce(&Path) -> Result<(), OxenError> + std::panic::UnwindSafe,
{
    let sync_dir = get_sync_dir()?;

    // Run test to see if it panic'd
    let result = std::panic::catch_unwind(|| match test(&sync_dir) {
        Ok(_) => {}
        Err(err) => {
            panic!("Error running test. Err: {}", err);
        }
    });

    // Remove repo dir
    std::fs::remove_dir_all(&sync_dir)?;

    // Assert everything okay after we cleanup the repo dir
    assert!(result.is_ok());

    Ok(())
}

pub fn request(sync_dir: &Path, uri: &str) -> actix_web::HttpRequest {
    actix_web::test::TestRequest::with_uri(uri)
        .app_data(OxenAppData {
            path: sync_dir.to_path_buf(),
        })
        .to_http_request()
}

pub fn request_with_param(
    sync_dir: &Path,
    uri: &str,
    key: impl Into<Cow<'static, str>>,
    val: impl Into<Cow<'static, str>>,
) -> actix_web::HttpRequest {
    actix_web::test::TestRequest::with_uri(uri)
        .app_data(OxenAppData {
            path: sync_dir.to_path_buf(),
        })
        .param(key, val)
        .to_http_request()
}

pub fn request_with_two_params(
    sync_dir: &Path,
    uri: &str,
    key_1: impl Into<Cow<'static, str>>,
    val_1: impl Into<Cow<'static, str>>,
    key_2: impl Into<Cow<'static, str>>,
    val_2: impl Into<Cow<'static, str>>,
) -> actix_web::HttpRequest {
    actix_web::test::TestRequest::with_uri(uri)
        .app_data(OxenAppData {
            path: sync_dir.to_path_buf(),
        })
        .param(key_1, val_1)
        .param(key_2, val_2)
        .to_http_request()
}

pub fn request_with_json(
    sync_dir: &Path,
    uri: &str,
    data: impl Serialize,
) -> actix_web::HttpRequest {
    actix_web::test::TestRequest::with_uri(uri)
        .app_data(OxenAppData {
            path: sync_dir.to_path_buf(),
        })
        .set_json(data)
        .to_http_request()
}

pub fn request_with_payload_and_entry(
    sync_dir: &Path,
    uri: &str,
    filename: impl Into<Cow<'static, str>>,
    hash: impl Into<Cow<'static, str>>,
    data: impl Into<actix_web::web::Bytes>,
) -> (actix_web::HttpRequest, actix_web::dev::Payload) {
    actix_web::test::TestRequest::with_uri(uri)
        .app_data(OxenAppData {
            path: sync_dir.to_path_buf(),
        })
        .param("filename", filename)
        .param("hash", hash)
        .set_payload(data)
        .to_http_parts()
}
