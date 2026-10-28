use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use geohash::Coord;
use grindr::{
    Credentials, DeviceInfo, GrindrClient, GrindrError, Method, Session, SessionKind, SignInResult,
};

const MIN_INTERVAL: Duration = Duration::from_secs(2 * 60);
const MAX_INTERVAL: Duration = Duration::from_secs(9 * 60 + 59);

const JIGGLE_METERS: f64 = 5.0;

const REFRESH_AHEAD: Duration = Duration::from_secs(2 * 60);

const MISSING_AUTH: &str = "set GRINDR_AUTH_TOKEN with GRINDR_EMAIL (email account) or GRINDR_THIRD_PARTY_USER_ID (Google or Facebook account)";

enum SignIn {
    Resume(Credentials),
    Password { email: String, password: String },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = data_dir()?;
    ensure_data_dir(&data_dir)?;

    let base_geohash = env("GRINDR_GEOHASH").ok_or("set GRINDR_GEOHASH")?;

    let device = load_device(&data_dir).unwrap_or_else(|| {
        let d = DeviceInfo::generate();
        save_device(&data_dir, &d);
        println!(
            "generated a new device identity -> {}",
            device_path(&data_dir).display()
        );
        d
    });

    let saved = load_credentials(&data_dir);
    let resumed = saved.is_some();
    let sign_in = match saved {
        Some(credentials) => SignIn::Resume(credentials),
        None => sign_in_from_env()?,
    };

    let geohash = jiggled(&base_geohash);
    let client = start_session(device, sign_in, &geohash)
        .await
        .map_err(|e| {
            let message = redact(&e.to_string(), &[&geohash, &base_geohash]);
            if !resumed {
                format!("could not sign in: {message}")
            } else if is_refusal(&e) {
                refused(&data_dir, &message)
            } else {
                format!(
                    "could not resume the session saved in {}: {message}",
                    session_path(&data_dir).display()
                )
            }
        })?;
    persist_session(&data_dir, &client)?;

    let once = std::env::args().any(|a| a == "once");

    loop {
        let geohash = jiggled(&base_geohash);

        if session_expiring(&client) {
            match client.refresh_session_at_geohash(Some(&geohash)).await {
                Ok(result) => report_restriction(&result),
                Err(e) => {
                    let message = redact(&e.to_string(), &[&geohash, &base_geohash]);
                    if is_refusal(&e) {
                        return Err(refused(&data_dir, &message).into());
                    }
                    eprintln!("[{}] session refresh failed: {message}", unix_now());
                }
            }
        }

        match run_cascade(&client, &geohash).await {
            Ok(()) => {}
            Err(e) => eprintln!(
                "[{}] request failed: {}",
                unix_now(),
                redact(&e.to_string(), &[&geohash, &base_geohash])
            ),
        }

        persist_session(&data_dir, &client)?;

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

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

fn sign_in_from_env() -> Result<SignIn, &'static str> {
    let email = env("GRINDR_EMAIL");

    if let Some(auth_token) = env("GRINDR_AUTH_TOKEN") {
        return token_credentials(auth_token, email, env("GRINDR_THIRD_PARTY_USER_ID"))
            .map(SignIn::Resume);
    }

    let password = std::env::var("GRINDR_PASSWORD")
        .ok()
        .filter(|p| !p.is_empty());

    match (email, password) {
        (Some(email), Some(password)) => Ok(SignIn::Password { email, password }),
        _ => Err(MISSING_AUTH),
    }
}

/// Email accounts refresh with the email, Google and Facebook accounts with the
/// third-party user id; the auth token alone identifies neither.
fn token_credentials(
    auth_token: String,
    email: Option<String>,
    third_party_user_id: Option<String>,
) -> Result<Credentials, &'static str> {
    let (email, kind) = match &third_party_user_id {
        Some(id) if id.starts_with("facebook") => (id.clone(), SessionKind::Facebook),
        Some(id) => (id.clone(), SessionKind::Google),
        None => (email.ok_or(MISSING_AUTH)?, SessionKind::Email),
    };

    Ok(Credentials {
        email,
        profile_id: None,
        auth_token,
        kind,
        third_party_user_id,
    })
}

async fn start_session(
    device: DeviceInfo,
    sign_in: SignIn,
    geohash: &str,
) -> Result<GrindrClient, GrindrError> {
    let (client, result) = match sign_in {
        SignIn::Resume(credentials) => {
            let session = Session {
                credentials,
                token: None,
            };
            let client = GrindrClient::new(device, Some(session))?;
            println!("refreshing the session …");
            let result = client.refresh_session_at_geohash(Some(geohash)).await?;
            (client, result)
        }
        SignIn::Password { email, password } => {
            let client = GrindrClient::new(device, None)?;
            println!("signing in …");
            let result = client
                .sign_in_with_email_at_geohash(&email, &password, Some(geohash))
                .await?;
            (client, result)
        }
    };

    report_restriction(&result);

    Ok(client)
}

fn report_restriction(result: &SignInResult) {
    if let Some(restriction) = &result.restriction {
        eprintln!(
            "[{}] the account is restricted: {restriction:?}",
            unix_now()
        );
    }
}

/// Refreshing before the library's lazy refresh surfaces the real error; the
/// lazy one keeps a session the server refused and only logs it.
fn session_expiring(client: &GrindrClient) -> bool {
    let expires_at = client
        .session_receiver()
        .borrow()
        .as_ref()
        .and_then(|s| s.token.as_ref())
        .map(|t| t.expires_at);

    expires_at.is_none_or(|at| at <= unix_now() + REFRESH_AHEAD.as_secs())
}

/// A rejected token comes back as a `403` with code 4 rather than a `401`, and
/// code 8 means this client version is no longer accepted.
fn is_refusal(e: &GrindrError) -> bool {
    matches!(
        e,
        GrindrError::Unauthorized { .. }
            | GrindrError::Banned(_)
            | GrindrError::Auth(_)
            | GrindrError::Api { code: 4 | 8, .. }
    )
}

fn refused(data_dir: &Path, reason: &str) -> String {
    format!(
        "Grindr refused the session ({reason}); if the token is no longer valid, delete {} and set GRINDR_AUTH_TOKEN",
        session_path(data_dir).display()
    )
}

fn jiggled(base_geohash: &str) -> String {
    jiggle_geohash(base_geohash, JIGGLE_METERS).unwrap_or_else(|_| {
        eprintln!("[{}] geohash jiggle failed; using the base geohash", unix_now());
        base_geohash.to_owned()
    })
}

fn jiggle_geohash(base: &str, max_meters: f64) -> Result<String, geohash::GeohashError> {
    const METERS_PER_DEG_LAT: f64 = 111_320.0;

    let (coord, _, _) = geohash::decode(base)?;

    let bearing = rand::random_range(0.0..std::f64::consts::TAU);
    let distance = max_meters * rand::random_range(0.0..=1.0_f64).sqrt();

    let north = distance * bearing.cos();
    let east = distance * bearing.sin();

    let lat_rad = coord.y.to_radians();
    let d_lat = north / METERS_PER_DEG_LAT;
    let d_lon = east / (METERS_PER_DEG_LAT * lat_rad.cos().abs().max(1e-6));

    let jiggled = Coord {
        x: wrap_longitude(coord.x + d_lon),
        y: (coord.y + d_lat).clamp(-90.0, 90.0),
    };

    geohash::encode(jiggled, base.chars().count())
}

/// Wrap a longitude in degrees into `[-180, 180)` so a jiggle across the
/// antimeridian stays a coordinate `encode` accepts.
fn wrap_longitude(lon: f64) -> f64 {
    (lon + 540.0).rem_euclid(360.0) - 180.0
}

/// Logs only the status and size of each answer, never the location or the nearby profiles.
async fn run_cascade(client: &GrindrClient, geohash: &str) -> Result<(), GrindrError> {
    let path = format!("/v4/cascade?nearbyGeoHash={geohash}");

    let resp = client.request(Method::GET, &path).send().await?;

    println!(
        "[{}] GET /v4/cascade -> {} ({} bytes)",
        unix_now(),
        resp.status,
        resp.body.len()
    );

    Ok(())
}

/// An error may quote the request URL, so the geohashes in it are hidden.
fn redact(message: &str, hidden: &[&str]) -> String {
    hidden
        .iter()
        .filter(|h| !h.is_empty())
        .fold(message.to_owned(), |m, h| m.replace(h, "…"))
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

/// A sign-in or refresh can return a new auth token, so the latest one is
/// saved after each of them.
fn persist_session(data_dir: &Path, client: &GrindrClient) -> Result<(), String> {
    let credentials = client
        .session_receiver()
        .borrow()
        .as_ref()
        .map(|s| s.credentials.clone());

    let Some(credentials) = credentials else {
        return Err(refused(data_dir, "signed out"));
    };

    save_credentials(data_dir, &credentials);
    Ok(())
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

fn load_credentials(data_dir: &Path) -> Option<Credentials> {
    serde_json::from_slice(&fs::read(session_path(data_dir)).ok()?).ok()
}

fn save_credentials(data_dir: &Path, credentials: &Credentials) {
    match serde_json::to_vec_pretty(credentials) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn haversine_meters(a: Coord<f64>, b: Coord<f64>) -> f64 {
        const R: f64 = 6_371_000.0;
        let (lat1, lat2) = (a.y.to_radians(), b.y.to_radians());
        let d_lat = (b.y - a.y).to_radians();
        let d_lon = (b.x - a.x).to_radians();
        let h = (d_lat / 2.0).sin().powi(2)
            + lat1.cos() * lat2.cos() * (d_lon / 2.0).sin().powi(2);
        2.0 * R * h.sqrt().asin()
    }

    #[test]
    fn jiggle_stays_within_bounds_and_varies() {
        let base = "gcpvj0duq2yk";
        let (origin, _, _) = geohash::decode(base).unwrap();

        let mut moved = 0;
        let mut max_dist = 0.0f64;
        for _ in 0..10_000 {
            let jiggled = jiggle_geohash(base, JIGGLE_METERS).unwrap();
            assert_eq!(jiggled.chars().count(), base.chars().count());

            let (point, _, _) = geohash::decode(&jiggled).unwrap();
            let dist = haversine_meters(origin, point);
            max_dist = max_dist.max(dist);
            if jiggled != base {
                moved += 1;
            }
        }

        assert!(max_dist <= JIGGLE_METERS * 1.1, "max displacement {max_dist} m exceeded budget");
        assert!(moved > 9_000, "geohash rarely changed ({moved}/10000)");
    }

    #[test]
    fn jiggle_across_antimeridian_stays_valid_and_close() {
        // Base geohash a fraction of a meter west of +180 longitude.
        let base = geohash::encode(Coord { x: 179.999_99, y: 0.0 }, 12).unwrap();
        let (origin, _, _) = geohash::decode(&base).unwrap();

        for _ in 0..10_000 {
            // Never falls back to Err: the wrap keeps longitude in range.
            let jiggled = jiggle_geohash(&base, JIGGLE_METERS).unwrap();
            let (point, _, _) = geohash::decode(&jiggled).unwrap();
            assert!((-180.0..180.0).contains(&point.x), "lon {} out of range", point.x);
            assert!(
                haversine_meters(origin, point) <= JIGGLE_METERS * 1.1,
                "displacement exceeded budget across antimeridian"
            );
        }
    }

    #[test]
    fn redact_hides_the_geohashes() {
        let message = "error sending request for url (https://grindr.mobi/v4/cascade?nearbyGeoHash=gcpvj0duq2yk)";
        assert_eq!(
            redact(message, &["gcpvj0duq2yk", ""]),
            "error sending request for url (https://grindr.mobi/v4/cascade?nearbyGeoHash=…)"
        );
    }

    #[test]
    fn session_saved_by_grindr_0_2_still_loads() {
        let saved = r#"{
            "email": "m@example.com",
            "expires_at": 1781234567,
            "profile_id": "123456789",
            "session_id": "header.payload.signature",
            "auth_token": "0123456789abcdef",
            "kind": "Email",
            "third_party_user_id": null
        }"#;

        let credentials: Credentials = serde_json::from_str(saved).unwrap();
        assert_eq!(credentials.email, "m@example.com");
        assert_eq!(credentials.profile_id.as_deref(), Some("123456789"));
        assert_eq!(credentials.auth_token, "0123456789abcdef");
        assert_eq!(credentials.kind, SessionKind::Email);
        assert_eq!(credentials.third_party_user_id, None);
    }

    #[test]
    fn token_with_email_resumes_an_email_account() {
        let credentials =
            token_credentials("tok".into(), Some("m@example.com".into()), None).unwrap();
        assert_eq!(credentials.kind, SessionKind::Email);
        assert_eq!(credentials.email, "m@example.com");
        assert_eq!(credentials.auth_token, "tok");
        assert_eq!(credentials.third_party_user_id, None);
    }

    #[test]
    fn token_with_third_party_id_resumes_a_google_or_facebook_account() {
        let google = token_credentials(
            "tok".into(),
            Some("m@example.com".into()),
            Some("1170".into()),
        )
        .unwrap();
        assert_eq!(google.kind, SessionKind::Google);
        assert_eq!(google.third_party_user_id.as_deref(), Some("1170"));

        let facebook = token_credentials("tok".into(), None, Some("facebook1221".into())).unwrap();
        assert_eq!(facebook.kind, SessionKind::Facebook);
        assert_eq!(
            facebook.third_party_user_id.as_deref(),
            Some("facebook1221")
        );
    }

    #[test]
    fn token_without_an_account_id_is_refused() {
        assert_eq!(
            token_credentials("tok".into(), None, None).err(),
            Some(MISSING_AUTH)
        );
    }

    #[test]
    fn wrap_longitude_maps_into_range() {
        assert!((wrap_longitude(180.00004) - -179.99996).abs() < 1e-6);
        assert!((wrap_longitude(-180.00004) - 179.99996).abs() < 1e-6);
        assert_eq!(wrap_longitude(0.0), 0.0);
        assert!((wrap_longitude(-0.09) - -0.09).abs() < 1e-9);
    }
}
