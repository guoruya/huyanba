use super::{DownloadHeaders, DownloadTransport};
use std::path::Path;

pub(super) fn is_available(_transport: DownloadTransport) -> bool {
    false
}

pub(super) fn download(
    _transport: DownloadTransport,
    _url: &str,
    _output_path: &Path,
    _headers: DownloadHeaders<'_>,
) -> Result<(), String> {
    Err("当前平台不支持系统命令下载回退".into())
}
