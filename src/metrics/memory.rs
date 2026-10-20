//! 物理内存占用率：GlobalMemoryStatusEx 瞬时读数。系统已给出平滑量（GB 级惯性），
//! 无需 PDH 采样窗口——不为不存在的需求引入机制。

use std::mem::size_of;
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

pub fn memory_pct() -> Option<u32> {
    unsafe {
        let mut ms = MEMORYSTATUSEX::default();
        ms.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
        GlobalMemoryStatusEx(&mut ms).ok().map(|_| ms.dwMemoryLoad)
    }
}
