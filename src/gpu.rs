#[cfg(windows)]
#[path = "platform/windows/gpu.rs"]
mod windows_gpu;

#[cfg(windows)]
pub use windows_gpu::{host, state};

#[cfg(target_os = "linux")]
#[path = "platform/linux/gpu.rs"]
mod linux_gpu;

#[cfg(target_os = "linux")]
pub use linux_gpu::{host, state};

#[cfg(target_os = "macos")]
pub fn host() -> Vec<String> {
    Vec::new()
}

#[cfg(target_os = "macos")]
pub fn state() -> (Vec<f64>, Vec<crate::proto::StateGpu>) {
    (Vec::new(), Vec::new())
}
