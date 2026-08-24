use super::{configure_curl, output_error, DownloadHeaders, DownloadTransport};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

const SYSTEM_CURL: &str = "/usr/bin/curl";
fn curl_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        Command::new(SYSTEM_CURL)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    })
}

pub(super) fn is_available(transport: DownloadTransport) -> bool {
    match transport {
        DownloadTransport::Powershell => false,
        DownloadTransport::Curl => curl_available(),
    }
}

pub(super) fn download(
    transport: DownloadTransport,
    url: &str,
    output_path: &Path,
    headers: DownloadHeaders<'_>,
) -> Result<(), String> {
    match transport {
        DownloadTransport::Powershell => Err("macOS 不支持 PowerShell 下载回退".into()),
        DownloadTransport::Curl => {
            let mut command = Command::new(SYSTEM_CURL);
            configure_curl(&mut command, url, output_path, headers);
            let output = command.output().map_err(|error| error.to_string())?;
            if output.status.success() {
                Ok(())
            } else {
                Err(output_error(output))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SYSTEM_CURL;

    #[test]
    fn macos_uses_the_fixed_system_curl() {
        assert_eq!(SYSTEM_CURL, "/usr/bin/curl");
    }
}
