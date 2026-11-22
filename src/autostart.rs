//! Autostart via a per-user scheduled task ("Sidestep") that runs at logon with
//! highest privileges (so no UAC prompt) in the user's own desktop session.
//! The task only launches the tray app; it is visible in Task Scheduler and is
//! removed completely when the checkbox is cleared. There is no service.

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

const TASK: &str = "Sidestep";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn schtasks(args: &[&str]) -> Result<String, String> {
    let out = Command::new("schtasks")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("running schtasks: {e}"))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if out.status.success() {
        Ok(text)
    } else {
        Err(text.trim().to_string())
    }
}

pub fn is_registered() -> bool {
    schtasks(&["/Query", "/TN", TASK]).is_ok()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn register(exe: &Path) -> Result<(), String> {
    let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(d), Ok(u)) => format!("{d}\\{u}"),
        (_, Ok(u)) => u,
        _ => return Err("cannot determine current user".into()),
    };
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Starts the Sidestep tray app at logon. The tray icon is always visible while it runs.</Description></RegistrationInfo>
  <Triggers>
    <LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId><Delay>PT5S</Delay></LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Enabled>true</Enabled>
  </Settings>
  <Actions Context="Author">
    <Exec><Command>"{exe}"</Command><Arguments>--autostart</Arguments></Exec>
  </Actions>
</Task>"#,
        user = xml_escape(&user),
        exe = xml_escape(&exe.display().to_string()),
    );

    // schtasks wants UTF-16LE with BOM for /XML.
    let mut bytes = vec![0xFF, 0xFE];
    for u in xml.encode_utf16() {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    let tmp = std::env::temp_dir().join("Sidestep-task.xml");
    std::fs::write(&tmp, bytes).map_err(|e| format!("writing task xml: {e}"))?;
    let tmp_s = tmp.display().to_string();
    let res = schtasks(&["/Create", "/TN", TASK, "/XML", &tmp_s, "/F"]);
    let _ = std::fs::remove_file(&tmp);
    res.map(|_| ())
}

pub fn unregister() -> Result<(), String> {
    if !is_registered() {
        return Ok(());
    }
    schtasks(&["/Delete", "/TN", TASK, "/F"]).map(|_| ())
}

/// Removes a task by name (used to clean up the old "DpiToggle" task).
pub fn unregister_task(name: &str) -> Result<(), String> {
    if schtasks(&["/Query", "/TN", name]).is_err() {
        return Ok(());
    }
    schtasks(&["/Delete", "/TN", name, "/F"]).map(|_| ())
}
