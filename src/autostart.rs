//! Starting Irminsul when the user logs on to Windows.
//!
//! This is a scheduled task rather than a Run key entry: Irminsul needs
//! administrator rights to capture packets, and a task with the highest run
//! level starts elevated without a UAC prompt.  Changing it needs the same
//! rights, which Irminsul has unless it was started with `--no-admin`.

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Output};

    use anyhow::{Context, Result, anyhow};

    const TASK_NAME: &str = "Irminsul";
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn schtasks(args: &[&str]) -> Result<Output> {
        Command::new("schtasks")
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .context("unable to run schtasks")
    }

    fn escape(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    }

    fn task_xml() -> Result<String> {
        let exe = std::env::current_exe().context("unable to find the Irminsul executable")?;
        let dir = exe.parent().map(|dir| dir.to_string_lossy().into_owned());
        let user = format!(
            "{}\\{}",
            std::env::var("USERDOMAIN").unwrap_or_default(),
            std::env::var("USERNAME").context("unable to find the user name")?
        );

        Ok(format!(
            r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Starts Irminsul in the tray when you log on.</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
      <Delay>PT15S</Delay>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{command}</Command>
      <Arguments>--minimized</Arguments>
      <WorkingDirectory>{dir}</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"#,
            user = escape(&user),
            command = escape(&exe.to_string_lossy()),
            dir = escape(&dir.unwrap_or_default()),
        ))
    }

    fn failure(what: &str, output: &Output) -> anyhow::Error {
        let mut message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if message.is_empty() {
            message = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        }
        anyhow!("{what}: {message}")
    }

    pub fn is_enabled() -> bool {
        schtasks(&["/Query", "/TN", TASK_NAME])
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    pub fn set_enabled(enabled: bool) -> Result<()> {
        if !enabled {
            let output = schtasks(&["/Delete", "/TN", TASK_NAME, "/F"])?;
            return if output.status.success() {
                Ok(())
            } else {
                Err(failure("unable to remove the startup task", &output))
            };
        }

        // schtasks only reads task definitions in UTF-16.
        let xml = task_xml()?;
        let mut bytes = vec![0xFF, 0xFE];
        for unit in std::ffi::OsString::from(xml).encode_wide() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let path = std::env::temp_dir().join("irminsul-startup-task.xml");
        std::fs::write(&path, bytes).context("unable to write the task definition")?;

        let output = schtasks(&[
            "/Create",
            "/TN",
            TASK_NAME,
            "/XML",
            &path.to_string_lossy(),
            "/F",
        ]);
        let _ = std::fs::remove_file(&path);
        let output = output?;
        if output.status.success() {
            Ok(())
        } else {
            Err(failure("unable to create the startup task", &output))
        }
    }
}

#[cfg(windows)]
pub use imp::{is_enabled, set_enabled};

#[cfg(not(windows))]
pub fn is_enabled() -> bool {
    false
}

#[cfg(not(windows))]
pub fn set_enabled(_enabled: bool) -> anyhow::Result<()> {
    Err(anyhow::anyhow!(
        "starting with the system is only supported on Windows"
    ))
}
