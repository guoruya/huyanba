use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::{Command, Output};

pub const PALACE_REFERER: &str = "https://www.dpm.org.cn/lights/royal.html";
pub const PALACE_ACCEPT: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";
pub const PALACE_ACCEPT_LANGUAGE: &str = "zh-CN,zh;q=0.9,en;q=0.8";
pub const DESKTOP_USER_AGENT: &str = "Huyanba/2.4.0 (desktop; wallpaper downloader)";

#[derive(Debug, Clone, Copy)]
pub struct DownloadHeaders<'a> {
    pub referer: &'a str,
    pub accept: &'a str,
    pub accept_language: &'a str,
    pub user_agent: &'a str,
}

impl Default for DownloadHeaders<'static> {
    fn default() -> Self {
        Self {
            referer: PALACE_REFERER,
            accept: PALACE_ACCEPT,
            accept_language: PALACE_ACCEPT_LANGUAGE,
            user_agent: DESKTOP_USER_AGENT,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DownloadTransport {
    Powershell,
    Curl,
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod unsupported;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
use macos as implementation;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
use unsupported as implementation;
#[cfg(target_os = "windows")]
use windows as implementation;

pub fn is_available(transport: DownloadTransport) -> bool {
    implementation::is_available(transport)
}

pub fn powershell_available() -> bool {
    is_available(DownloadTransport::Powershell)
}

pub fn curl_available() -> bool {
    is_available(DownloadTransport::Curl)
}

pub fn download_with_transport(
    transport: DownloadTransport,
    url: &str,
    output_path: &Path,
    headers: DownloadHeaders<'_>,
) -> Result<(), String> {
    implementation::download(transport, url, output_path, headers)
}

pub fn run_powershell_download(url: &str, output_path: &Path) -> Result<(), String> {
    download_with_transport(
        DownloadTransport::Powershell,
        url,
        output_path,
        DownloadHeaders::default(),
    )
}

pub fn run_curl_download(url: &str, output_path: &Path) -> Result<(), String> {
    download_with_transport(
        DownloadTransport::Curl,
        url,
        output_path,
        DownloadHeaders::default(),
    )
}

pub(crate) fn configure_curl(
    command: &mut Command,
    url: &str,
    output_path: &Path,
    headers: DownloadHeaders<'_>,
) {
    command.args([
        "-L",
        "-sS",
        "--fail",
        "--compressed",
        "--connect-timeout",
        "15",
        "-H",
        &format!("Referer: {}", headers.referer),
        "-H",
        "X-Requested-With: XMLHttpRequest",
        "-H",
        &format!("User-Agent: {}", headers.user_agent),
        "-H",
        &format!("Accept: {}", headers.accept),
        "-H",
        &format!("Accept-Language: {}", headers.accept_language),
        "-o",
        &output_path.to_string_lossy(),
        url,
    ]);
}

pub(crate) fn output_error(output: Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("命令退出状态 {}", output.status)
    }
}

#[cfg(test)]
mod tests {
    use super::{DownloadHeaders, DESKTOP_USER_AGENT};

    #[test]
    fn default_user_agent_is_platform_neutral() {
        let headers = DownloadHeaders::default();
        assert_eq!(headers.user_agent, DESKTOP_USER_AGENT);
        assert!(!headers.user_agent.contains("Windows NT"));
        assert!(!headers.user_agent.contains("Macintosh"));
    }
}
