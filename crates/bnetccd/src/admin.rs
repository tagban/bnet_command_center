//! Admin credentials and TLS material for the status/admin UI.
//!
//! On first run this generates a **random** admin password (shown once, in the startup
//! log), stores only its **Argon2** hash, and generates a **self-signed TLS certificate**
//! so the panel can be served over HTTPS. Everything lives in a data directory next to the
//! account database, with the secret files locked to `0600`. Nothing here ever logs or
//! persists the password in the clear after that first presentation.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use argon2::password_hash::rand_core::OsRng as SaltRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand::Rng;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Length of the generated first-run password. 24 base-58 chars ≈ 140 bits of entropy, and
/// stays within the 32-char ceiling the operator asked for.
const GENERATED_PASSWORD_LEN: usize = 24;

/// Unambiguous alphabet for the generated password (no `0/O/1/l/I`), so it can be read off
/// a console and typed without confusion.
const PASSWORD_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";

/// The on-disk admin state (never contains the plaintext password).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AdminState {
    /// Argon2 PHC hash string of the admin password.
    password_hash: String,
    /// Whether the operator must change the password before doing anything else (true right
    /// after the random first-run password is generated).
    must_change: bool,
    /// Whether non-localhost access is permitted (the "enable remote control" switch).
    remote_enabled: bool,
}

/// Admin credentials + TLS material, shared across the status server's connections.
#[derive(Debug)]
pub struct Admin {
    dir: PathBuf,
    state: Mutex<AdminState>,
    /// PEM-encoded self-signed certificate and its private key, for the TLS listener.
    cert_pem: String,
    key_pem: String,
}

impl Admin {
    /// Load the admin state from `dir`, or initialise it on first run.
    ///
    /// On first run: generate a random password, print it **once** to the log, store its
    /// Argon2 hash with `must_change = true`, and generate a self-signed cert. Returns an
    /// error only if the directory or files cannot be created/read.
    pub fn load_or_init(dir: impl AsRef<Path>) -> std::io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;
        let state_path = dir.join("admin.json");

        let state = if state_path.exists() {
            let raw = fs::read_to_string(&state_path)?;
            serde_json::from_str::<AdminState>(&raw).map_err(|e| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, e)
            })?
        } else {
            let password = generate_password();
            let state = AdminState {
                password_hash: hash_password(&password),
                must_change: true,
                remote_enabled: false,
            };
            write_secret(&state_path, serde_json::to_string_pretty(&state).unwrap_or_default())?;
            // The only time the password is ever shown. Loud and unmissable.
            info!(
                "\n==================================================================\n\
                   BNET Command Center — admin panel first-run password:\n\n    {password}\n\n\
                   Sign in at https://<host>:6114 and change it immediately.\n\
                 ==================================================================",
            );
            state
        };

        let (cert_pem, key_pem) = load_or_generate_cert(&dir)?;
        Ok(Self {
            dir,
            state: Mutex::new(state),
            cert_pem,
            key_pem,
        })
    }

    /// Verify a candidate password against the stored Argon2 hash. Constant-time within
    /// Argon2's verifier; a malformed stored hash fails closed.
    #[must_use]
    pub fn verify_password(&self, candidate: &str) -> bool {
        let hash = self.state.lock().expect("admin lock").password_hash.clone();
        let Ok(parsed) = PasswordHash::new(&hash) else {
            return false;
        };
        Argon2::default()
            .verify_password(candidate.as_bytes(), &parsed)
            .is_ok()
    }

    /// Whether the operator still needs to change the first-run password.
    #[must_use]
    pub fn must_change(&self) -> bool {
        self.state.lock().expect("admin lock").must_change
    }

    /// Whether non-localhost access is currently permitted.
    #[must_use]
    pub fn remote_enabled(&self) -> bool {
        self.state.lock().expect("admin lock").remote_enabled
    }

    /// Set a new password (clearing `must_change`) and persist. Rejects an over-long or
    /// empty password; 32 chars is the operator-requested ceiling.
    pub fn set_password(&self, new: &str) -> Result<(), &'static str> {
        if new.is_empty() {
            return Err("password must not be empty");
        }
        if new.chars().count() > 32 {
            return Err("password must be at most 32 characters");
        }
        let hashed = hash_password(new);
        let mut guard = self.state.lock().expect("admin lock");
        guard.password_hash = hashed;
        guard.must_change = false;
        self.persist(&guard);
        Ok(())
    }

    /// Enable or disable non-localhost access, and persist.
    pub fn set_remote_enabled(&self, on: bool) {
        let mut guard = self.state.lock().expect("admin lock");
        guard.remote_enabled = on;
        self.persist(&guard);
    }

    /// PEM certificate for the TLS listener.
    #[must_use]
    pub fn cert_pem(&self) -> &str {
        &self.cert_pem
    }

    /// PEM private key for the TLS listener.
    #[must_use]
    pub fn key_pem(&self) -> &str {
        &self.key_pem
    }

    fn persist(&self, state: &AdminState) {
        match serde_json::to_string_pretty(state) {
            Ok(s) => {
                if let Err(e) = write_secret(&self.dir.join("admin.json"), s) {
                    warn!(error = %e, "failed to persist admin state");
                }
            }
            Err(e) => warn!(error = %e, "failed to serialise admin state"),
        }
    }
}

/// Generate a random password from the unambiguous alphabet using the OS CSPRNG.
fn generate_password() -> String {
    let mut rng = rand::rngs::OsRng;
    (0..GENERATED_PASSWORD_LEN)
        .map(|_| PASSWORD_ALPHABET[rng.gen_range(0..PASSWORD_ALPHABET.len())] as char)
        .collect()
}

/// Argon2id hash of `password` with a fresh random salt, as a PHC string.
fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut SaltRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("argon2 hashing cannot fail on valid input")
        .to_string()
}

/// Load the persisted self-signed cert/key, or generate and persist one.
fn load_or_generate_cert(dir: &Path) -> std::io::Result<(String, String)> {
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    if let (Ok(cert), Ok(key)) = (fs::read_to_string(&cert_path), fs::read_to_string(&key_path)) {
        return Ok((cert, key));
    }
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    let cert_pem = certified.cert.pem();
    let key_pem = certified.key_pair.serialize_pem();
    fs::write(&cert_path, &cert_pem)?;
    write_secret(&key_path, key_pem.clone())?;
    Ok((cert_pem, key_pem))
}

/// Write a file and, on Unix, lock it to `0600` (owner read/write only) — for secrets.
fn write_secret(path: &Path, contents: String) -> std::io::Result<()> {
    fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut d = std::env::temp_dir();
        d.push(format!("bnetccd-admin-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn first_run_generates_a_changeable_password() {
        let dir = temp_dir("firstrun");
        let admin = Admin::load_or_init(&dir).expect("init");
        assert!(admin.must_change(), "first run must force a password change");
        assert!(!admin.remote_enabled(), "remote is off by default");
        // A wrong password is rejected; a changed password verifies and clears must_change.
        assert!(!admin.verify_password("definitely-not-it"));
        admin.set_password("a-brand-new-secret").expect("set");
        assert!(admin.verify_password("a-brand-new-secret"));
        assert!(!admin.must_change());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn state_and_cert_persist_across_reloads() {
        let dir = temp_dir("persist");
        {
            let admin = Admin::load_or_init(&dir).expect("init");
            admin.set_password("shared-secret").expect("set");
            admin.set_remote_enabled(true);
        }
        let cert_before = fs::read_to_string(dir.join("cert.pem")).expect("cert");
        // Reload: the password, flags, and cert must all survive.
        let admin = Admin::load_or_init(&dir).expect("reload");
        assert!(admin.verify_password("shared-secret"));
        assert!(!admin.must_change());
        assert!(admin.remote_enabled());
        assert_eq!(admin.cert_pem(), cert_before, "the cert must be stable across restarts");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversized_password_is_rejected() {
        let dir = temp_dir("toolong");
        let admin = Admin::load_or_init(&dir).expect("init");
        assert!(admin.set_password(&"x".repeat(33)).is_err());
        assert!(admin.set_password("").is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
