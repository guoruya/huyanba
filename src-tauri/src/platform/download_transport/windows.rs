use super::{configure_curl, output_error, DownloadHeaders, DownloadTransport};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

const CREATE_NO_WINDOW_FLAG: u32 = 0x08000000;
fn silent_command(program: &str) -> Command {
    let mut command = Command::new(program);
    command.creation_flags(CREATE_NO_WINDOW_FLAG);
    command
}

fn escape_powershell_single_quoted(value: &str) -> String {
    value.replace('\'', "''")
}

fn powershell_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        silent_command("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", "exit 0"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    })
}

fn curl_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        silent_command("curl.exe")
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
        DownloadTransport::Powershell => powershell_available(),
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
        DownloadTransport::Powershell => download_with_powershell(url, output_path, headers),
        DownloadTransport::Curl => download_with_curl(url, output_path, headers),
    }
}

fn download_with_powershell(
    url: &str,
    output_path: &Path,
    headers: DownloadHeaders<'_>,
) -> Result<(), String> {
    let script = format!(
        r#"$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$OutputEncoding = [System.Text.Encoding]::UTF8
[System.Net.ServicePointManager]::SecurityProtocol = [System.Net.SecurityProtocolType]::Tls12
$headers = @{{
  'Referer' = '{referer}'
  'X-Requested-With' = 'XMLHttpRequest'
  'User-Agent' = '{user_agent}'
  'Accept' = '{accept}'
  'Accept-Language' = '{accept_language}'
}}
Invoke-WebRequest -Uri '{url}' -Headers $headers -OutFile '{output_path}' -UseBasicParsing"#,
        referer = escape_powershell_single_quoted(headers.referer),
        user_agent = escape_powershell_single_quoted(headers.user_agent),
        accept = escape_powershell_single_quoted(headers.accept),
        accept_language = escape_powershell_single_quoted(headers.accept_language),
        url = escape_powershell_single_quoted(url),
        output_path = escape_powershell_single_quoted(&output_path.to_string_lossy()),
    );
    let output = silent_command("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(output_error(output))
    }
}

fn download_with_curl(
    url: &str,
    output_path: &Path,
    headers: DownloadHeaders<'_>,
) -> Result<(), String> {
    let mut command = silent_command("curl.exe");
    configure_curl(&mut command, url, output_path, headers);
    let output = command.output().map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(output_error(output))
    }
}

#[cfg(test)]
mod tests {
    use super::escape_powershell_single_quoted;

    #[test]
    fn powershell_single_quotes_are_doubled() {
        assert_eq!(escape_powershell_single_quoted("a'b"), "a''b");
    }
}
