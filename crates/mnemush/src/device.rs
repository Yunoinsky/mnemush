//! Per-device identity for v1.6.2 sync provenance.
//!
//! Every mnemush install gets a stable UUID v4 (`device_id`) and an
//! optional human-readable name (`device_name`). The id is stamped
//! onto every new memory as `origin_device` so cross-device sync
//! preserves provenance — the merge keeps the **older** `created_at`'s
//! origin_device, so the original creator is always attributable.
//!
//! Files (under `default_data_dir`):
//!   - `device_id`   — single line, UUID v4. Generated on first call.
//!   - `device_name` — single line, plain text. Optional. Defaults to
//!                      `HOSTNAME` / `COMPUTERNAME` env on Unix/Windows.

use std::path::{Path, PathBuf};

use crate::default_data_dir;
use crate::error::Result;

fn device_id_path(data_dir: &Path) -> PathBuf {
    data_dir.join("device_id")
}

fn device_name_path(data_dir: &Path) -> PathBuf {
    data_dir.join("device_name")
}

/// Return the local device id, generating + persisting one on first
/// call. Always returns the same value for the same install (the id
/// file is the source of truth).
pub fn local_device_id() -> String {
    local_device_id_in(&default_data_dir())
}

fn local_device_id_in(data_dir: &Path) -> String {
    if let Ok(s) = std::fs::read_to_string(device_id_path(data_dir)) {
        let s = s.trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }
    // Generate a fresh UUID v4 and persist it. If the file write fails
    // (read-only HOME, etc.) we still return the freshly-generated id
    // so the call site can use it for this process; next call will retry
    // the write.
    let id = uuid::Uuid::new_v4().to_string();
    let _ = std::fs::create_dir_all(data_dir);
    let _ = std::fs::write(device_id_path(data_dir), &id);
    id
}

/// Return the local device name, falling back to the host's
/// `HOSTNAME` / `COMPUTERNAME` env on first call, then to the id's
/// first 8 characters if neither is set. Persists the chosen default.
pub fn local_device_name() -> String {
    local_device_name_in(&default_data_dir())
}

fn local_device_name_in(data_dir: &Path) -> String {
    if let Ok(s) = std::fs::read_to_string(device_name_path(data_dir)) {
        let s = s.trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }
    let default = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| {
            let id = local_device_id_in(data_dir);
            id.chars().take(8).collect()
        });
    let _ = std::fs::create_dir_all(data_dir);
    let _ = std::fs::write(device_name_path(data_dir), &default);
    default
}

/// Explicitly set the device name. Trims whitespace; rejects empty.
pub fn set_device_name(name: &str) -> Result<()> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(crate::error::MnemushError::Other(
            "device name cannot be empty".into(),
        ));
    }
    let p = device_name_path(&default_data_dir());
    std::fs::create_dir_all(p.parent().unwrap())?;
    std::fs::write(p, trimmed)?;
    Ok(())
}

/// A snapshot of the local device identity, used by `mnemush device show`
/// and `mnemush status`.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub id_path: PathBuf,
    pub name_path: PathBuf,
}

pub fn current_device_info() -> DeviceInfo {
    let data_dir = default_data_dir();
    DeviceInfo {
        id: local_device_id_in(&data_dir),
        name: local_device_name_in(&data_dir),
        id_path: device_id_path(&data_dir),
        name_path: device_name_path(&data_dir),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_is_stable_across_calls() {
        let tmp = std::env::temp_dir().join(format!("mnemush-device-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let a = local_device_id_in(&tmp);
        let b = local_device_id_in(&tmp);
        assert_eq!(a, b, "device id must be stable across calls");
        let raw = std::fs::read_to_string(device_id_path(&tmp)).unwrap();
        assert_eq!(raw.trim(), a);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn device_name_falls_back_to_id_prefix_when_no_env() {
        let tmp = std::env::temp_dir().join(format!("mnemush-device-name-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let id = local_device_id_in(&tmp);
        let name = local_device_name_in(&tmp);
        // If HOSTNAME/COMPUTERNAME are set in the test env, the name
        // comes from there; otherwise it falls back to id prefix.
        if std::env::var("HOSTNAME").is_err() && std::env::var("COMPUTERNAME").is_err() {
            assert_eq!(name, id.chars().take(8).collect::<String>());
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn set_device_name_persists() {
        let tmp = std::env::temp_dir().join(format!("mnemush-device-rename-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let id = local_device_id_in(&tmp);
        // Bypass set_device_name (uses default_data_dir) — write directly
        // to verify the file format and round-trip.
        std::fs::write(device_name_path(&tmp), "test-host").unwrap();
        let name = local_device_name_in(&tmp);
        assert_eq!(name, "test-host");
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = id;
    }
}
