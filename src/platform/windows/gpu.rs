#[cfg(windows)]
mod windows {
    use crate::proto::StateGpu;
    use std::{process::Command, ptr, thread, time::Duration};
    use windows_sys::Win32::System::Performance::{
        PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
        PdhOpenQueryW, PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE_ITEM_W,
        PDH_FMT_DOUBLE, PDH_MORE_DATA,
    };

    pub fn host() -> Vec<String> {
        let output = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Get-CimInstance Win32_VideoController | Select-Object -ExpandProperty Name | ConvertTo-Json -Compress",
            ])
            .output();
        let Ok(output) = output else {
            return Vec::new();
        };
        if !output.status.success() {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
            return Vec::new();
        };
        match value {
            serde_json::Value::String(name) => vec![name],
            serde_json::Value::Array(names) => names
                .into_iter()
                .filter_map(|name| name.as_str().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        }
    }

    pub fn state() -> (Vec<f64>, Vec<StateGpu>) {
        let utilization = query_3d_utilization().unwrap_or_default();
        let detailed = utilization
            .iter()
            .map(|value| StateGpu {
                utilization: *value,
                memory_used: 0,
                memory_total: 0,
            })
            .collect();
        (utilization, detailed)
    }

    struct Query(*mut core::ffi::c_void);

    impl Drop for Query {
        fn drop(&mut self) {
            unsafe { PdhCloseQuery(self.0) };
        }
    }

    fn query_3d_utilization() -> Option<Vec<f64>> {
        let mut query = ptr::null_mut();
        if unsafe { PdhOpenQueryW(ptr::null(), 0, &mut query) } != 0 {
            return None;
        }
        let query = Query(query);
        let path: Vec<u16> = "\\GPU Engine(*engtype_3D)\\Utilization Percentage\0"
            .encode_utf16()
            .collect();
        let mut counter = ptr::null_mut();
        if unsafe { PdhAddEnglishCounterW(query.0, path.as_ptr(), 0, &mut counter) } != 0 {
            return None;
        }
        if unsafe { PdhCollectQueryData(query.0) } != 0 {
            return None;
        }
        thread::sleep(Duration::from_millis(10));
        if unsafe { PdhCollectQueryData(query.0) } != 0 {
            return None;
        }

        let mut size = 8192u32;
        loop {
            if size > 100 * 1024 * 1024 {
                return None;
            }
            let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
            let mut count = 0u32;
            let status = unsafe {
                PdhGetFormattedCounterArrayW(
                    counter,
                    PDH_FMT_DOUBLE,
                    &mut size,
                    &mut count,
                    buffer.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>(),
                )
            };
            if status == PDH_MORE_DATA {
                size = size.max((buffer.len() * 8) as u32 + 1);
                continue;
            }
            if status != 0
                || count as usize * std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>()
                    > buffer.len() * 8
            {
                return None;
            }
            let items = unsafe {
                std::slice::from_raw_parts(
                    buffer.as_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>(),
                    count as usize,
                )
            };
            let total: f64 = items
                .iter()
                .filter(|item| {
                    item.FmtValue.CStatus == PDH_CSTATUS_VALID_DATA
                        || item.FmtValue.CStatus == PDH_CSTATUS_NEW_DATA
                })
                .map(|item| unsafe { item.FmtValue.Anonymous.doubleValue })
                .filter(|value| value.is_finite())
                .sum();
            return Some(vec![total.min(100.0)]);
        }
    }
}

pub use windows::{host, state};

#[cfg(test)]
mod tests {
    #[test]
    fn windows_gpu_probe_is_callable() {
        let models = super::host();
        let (usage, detailed) = super::state();
        println!("GPU models: {models:?}, usage: {usage:?}, detailed: {detailed:?}");
        assert_eq!(usage.len(), detailed.len());
    }
}
