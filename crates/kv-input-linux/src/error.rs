//! Error types for Linux input backend.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum InputError {
    #[error("Failed to open device: {0}")]
    DeviceOpenFailed(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Device not found: {0}")]
    DeviceNotFound(String),

    #[error("Failed to enumerate devices: {0}")]
    EnumerationFailed(String),

    #[error("Device disconnected: {0}")]
    DeviceDisconnected(String),

    #[error("Event read error: {0}")]
    EventReadError(String),

    #[error("No keyboards found")]
    NoKeyboardsFound,

    #[error("Hotplug monitor failed: {0}")]
    HotplugFailed(String),

    #[error("Invalid device capabilities")]
    InvalidCapabilities,
}
