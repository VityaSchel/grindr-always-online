use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use grindr::{DeviceInfo, GrindrClient, Method, Session};

const MIN_INTERVAL: Duration = Duration::from_secs(2 * 60);
const MAX_INTERVAL: Duration = Duration::from_secs(15 * 60);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = data_dir()?;
    ensure_data_dir(&data_dir)?;

    let geohash = std::env::var("GRINDR_GEOHASH").map_err(|_| "set GRINDR_GEOHASH")?;

    let cascade_query = format!("nearbyGeoHash={geohash}");

    let device = load_device(&data_dir).unwrap_or_else(|| {
        let d = DeviceInfo::generate();
        save_device(&data_dir, &d);
        println!(
            "generated a new device identity -> {}",
            device_path(&data_dir).display()
        );
        d
    });

    let saved = load_session(&data_dir);
    let had_session = saved.is_some();
    let client = GrindrClient::new(device, saved)?;

    if !had_session {
        let email = std::env::var("GRINDR_EMAIL")
            .map_err(|_| "set GRINDR_EMAIL (and GRINDR_PASSWORD) to log in")?;
        let password = std::env::var("GRINDR_PASSWORD")
            .map_err(|_| "set GRINDR_PASSWORD (and GRINDR_EMAIL) to log in")?;

        println!("logging in as {email} …");
        client.login(&email, &password).await?;
        persist_session(&data_dir, &client);
    }

    let once = std::env::args().any(|a| a == "once");

    loop {
        match run_cascade(&client, &cascade_query).await {
            Ok(()) => {}
            Err(e) => eprintln!("[{}] request failed: {e}", unix_now()),
        }

        persist_session(&data_dir, &client);

        if once {
            break;
        }

        let delay = random_delay();
        println!(
            "sleeping {}m{:02}s before the next one …",
            delay.as_secs() / 60,
            delay.as_secs() % 60
        );

        tokio::time::sleep(delay).await;
    }

    Ok(())
}

async fn run_cascade(
    client: &GrindrClient,
    cascade_query: &str,
) -> Result<(), grindr::GrindrError> {
    let path = if cascade_query.is_empty() {
        "/v4/cascade".to_owned()
    } else {
        format!("/v4/cascade?{cascade_query}")
    };

    let resp = client
        .request_authenticated_raw(Method::GET, &path, None)
        .await?;

    let body = String::from_utf8_lossy(&resp.body);
    let snippet: String = body.chars().take(200).collect();

    println!("[{}] GET {path} -> {}", unix_now(), resp.status);
    println!("    {snippet}");

    Ok(())
}

fn data_dir() -> Result<PathBuf, std::env::VarError> {
    match std::env::var("GRINDR_DATA_DIR") {
        Ok(dir) => Ok(PathBuf::from(dir)),
        Err(std::env::VarError::NotPresent) => Ok(PathBuf::from("./data")),
        Err(e) => Err(e),
    }
}

fn device_path(data_dir: &Path) -> PathBuf {
    data_dir.join("device.json")
}

fn session_path(data_dir: &Path) -> PathBuf {
    data_dir.join("session.json")
}

fn ensure_data_dir(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }

    Ok(())
}

fn persist_session(data_dir: &Path, client: &GrindrClient) {
    if let Some(session) = client.session_receiver().borrow().clone() {
        save_session(data_dir, &session);
    }
}

fn load_device(data_dir: &Path) -> Option<DeviceInfo> {
    serde_json::from_slice(&fs::read(device_path(data_dir)).ok()?).ok()
}

fn save_device(data_dir: &Path, device: &DeviceInfo) {
    match serde_json::to_vec_pretty(device) {
        Ok(bytes) => {
            let _ = write_secure_file(&device_path(data_dir), &bytes);
        }
        Err(e) => eprintln!("could not serialize device: {e}"),
    }
}

fn load_session(data_dir: &Path) -> Option<Session> {
    serde_json::from_slice(&fs::read(session_path(data_dir)).ok()?).ok()
}

fn save_session(data_dir: &Path, session: &Session) {
    match serde_json::to_vec_pretty(session) {
        Ok(bytes) => {
            let _ = write_secure_file(&session_path(data_dir), &bytes);
        }
        Err(e) => eprintln!("could not serialize session: {e}"),
    }
}

fn write_secure_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)?;

        file.write_all(bytes)?;
        file.sync_all()?;
        return Ok(());
    }

    #[cfg(not(unix))]
    {
        fs::write(path, bytes)?;
        return Ok(());
    }
}

fn random_delay() -> Duration {
    let secs = rand::random_range(MIN_INTERVAL.as_secs()..=MAX_INTERVAL.as_secs());
    Duration::from_secs(secs)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
