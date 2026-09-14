//! Unattended hourly updates, with an OS-held single-instance lock.
use serde_json::json;
use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

pub fn status(dir: &Path) -> Result<serde_json::Value, String> {
    if !dir.exists() {
        return Ok(json!({"running":false}));
    }
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("daemon.lock"))
        .map_err(|e| e.to_string())?;
    Ok(json!({"running":lock.try_lock().is_err(),"interval_seconds":3600,"state_directory":dir}))
}
pub fn start(dir: &Path) -> Result<(), String> {
    if status(dir)?["running"] == true {
        return Ok(());
    }
    private_directory(dir)?;
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("daemon.log"))
        .map_err(|e| e.to_string())?;
    Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
        .args(["daemon", "run"])
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log)
        .spawn()
        .map_err(|e| e.to_string())?;
    for _ in 0..40 {
        if status(dir)?["running"] == true {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("daemon did not start; inspect the private daemon.log".into())
}
pub async fn run(dir: &Path) -> Result<(), String> {
    private_directory(dir)?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("daemon.lock"))
        .map_err(|e| e.to_string())?;
    if lock.try_lock().is_err() {
        return Ok(());
    }
    let _ = fs::remove_file(dir.join("daemon.stop"));
    loop {
        if dir.join("daemon.stop").exists() {
            break;
        }
        if let Err(error) = super::updater::automatic(dir).await {
            eprintln!("Hourly update: {error}");
        }
        // Frequent local checks allow opt-out/stop changes; the updater owns the hourly throttle.
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    Ok(())
}
pub fn stop(dir: &Path) -> Result<(), String> {
    private_directory(dir)?;
    fs::write(dir.join("daemon.stop"), b"stop").map_err(|e| e.to_string())
}
pub fn install() -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or("HOME is not set")?;
    let silicon = super::default_home_dir();
    #[cfg(target_os = "macos")]
    {
        let escape = |v: &str| {
            v.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
        };
        let folder = home.join("Library/LaunchAgents");
        fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
        let file = folder.join("com.teamofsilicons.waveform.updater.plist");
        let content = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>com.teamofsilicons.waveform.updater</string><key>ProgramArguments</key><array><string>{}</string><string>daemon</string><string>run</string></array><key>EnvironmentVariables</key><dict><key>SILICON_HOME</key><string>{}</string><key>PATH</key><string>{}</string></dict><key>RunAtLoad</key><true/><key>KeepAlive</key><true/></dict></plist>",
            escape(&executable.to_string_lossy()),
            escape(&silicon.to_string_lossy()),
            escape(&std::env::var("PATH").unwrap_or_default())
        );
        fs::write(&file, content).map_err(|e| e.to_string())?;
        let uid = Command::new("id")
            .arg("-u")
            .output()
            .map_err(|e| e.to_string())?;
        let domain = format!("gui/{}", String::from_utf8_lossy(&uid.stdout).trim());
        let _ = Command::new("launchctl")
            .arg("bootout")
            .arg(&domain)
            .arg(&file)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let result = Command::new("launchctl")
            .arg("bootstrap")
            .arg(domain)
            .arg(file)
            .status()
            .map_err(|e| e.to_string())?;
        if !result.success() {
            return Err("launchctl could not install the updater; run waveform daemon start for this session".into());
        }
    }
    #[cfg(target_os = "linux")]
    {
        let folder = home.join(".config/systemd/user");
        fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
        let quote = |v: &str| {
            format!(
                "\"{}\"",
                v.replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('%', "%%")
            )
        };
        let content = format!(
            "[Unit]\nDescription=Waveform hourly updater\n[Service]\nExecStart={} daemon run\nEnvironment={}\nEnvironment={}\nRestart=on-failure\n[Install]\nWantedBy=default.target\n",
            quote(&executable.to_string_lossy()),
            quote(&format!("SILICON_HOME={}", silicon.display())),
            quote(&format!(
                "PATH={}",
                std::env::var("PATH").unwrap_or_default()
            ))
        );
        fs::write(folder.join("waveform-updater.service"), content).map_err(|e| e.to_string())?;
        for args in [
            vec!["--user", "daemon-reload"],
            vec!["--user", "enable", "--now", "waveform-updater.service"],
        ] {
            if !Command::new("systemctl")
                .args(args)
                .status()
                .map_err(|e| e.to_string())?
                .success()
            {
                return Err("systemd user service unavailable; run waveform daemon start".into());
            }
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        return Err("automatic service installation supports macOS and Linux; schedule waveform daemon run with your service manager".into());
    }
    Ok(())
}

fn private_directory(dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    Ok(())
}
