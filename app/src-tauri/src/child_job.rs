//! Windows Job Object（kill-on-close）：把使驾自己 spawn 的子进程挂进一个 Job，
//! 使驾进程无论正常退出还是被强杀（taskkill /F、更新重启），句柄关闭的瞬间
//! 整个 Job 内所有进程（含适配器再拉起的孙进程，如 codex/cloudflared）一并终止，
//! 杜绝托盘退出/强更后残留 node/cloudflared/codex 孤儿进程。
//!
//! 注意：Job 不设置 BREAKAWAY——所有经本程序派生的子树都应随宿主消亡；
//! 需要跨越宿主生命周期的外部进程（如更新脚本）必须由任务计划程序等
//! 本进程之外的执行器拉起，不能由本进程直接 spawn。

#[cfg(windows)]
mod imp {
    use std::sync::OnceLock;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(lpJobAttributes: *mut core::ffi::c_void, lpName: *const u16) -> *mut core::ffi::c_void;
        fn SetInformationJobObject(
            hJob: *mut core::ffi::c_void,
            job_object_information_class: u32,
            lpJobObjectInformation: *mut core::ffi::c_void,
            cbJobObjectInformationLength: u32,
        ) -> i32;
        fn AssignProcessToJobObject(
            hJob: *mut core::ffi::c_void,
            hProcess: *mut core::ffi::c_void,
        ) -> i32;
        fn TerminateJobObject(hJob: *mut core::ffi::c_void, uExitCode: u32) -> i32;
    }

    const JobObjectExtendedLimitInformation: u32 = 9;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;

    #[repr(C)]
    struct IoCounters {
        read_operation_count: u64,
        write_operation_count: u64,
        other_operation_count: u64,
        read_transfer_count: u64,
        write_transfer_count: u64,
        other_transfer_count: u64,
    }

    #[repr(C)]
    struct JobObjectBasicLimitInformation {
        per_process_user_time_limit: u64,
        per_job_user_time_limit: u64,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
        /// LimitFlags 是 BASIC 结构的最后一个字段（此前误把 kill 标志写到
        /// 首字段 PerProcessUserTimeLimit 上，导致 kill-on-close 从未生效）
        limit_flags: u32,
    }

    #[repr(C)]
    struct JobObjectExtendedLimitInformation {
        basic: JobObjectBasicLimitInformation,
        io_info: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    fn job() -> Option<*mut core::ffi::c_void> {
        static JOB: OnceLock<Option<usize>> = OnceLock::new();
        let v = *JOB.get_or_init(|| unsafe {
            let h = CreateJobObjectW(std::ptr::null_mut(), std::ptr::null());
            if h.is_null() {
                return None;
            }
            let mut info: JobObjectExtendedLimitInformation = std::mem::zeroed();
            info.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                h,
                JobObjectExtendedLimitInformation,
                &mut info as *mut _ as *mut core::ffi::c_void,
                std::mem::size_of::<JobObjectExtendedLimitInformation>() as u32,
            );
            if ok == 0 {
                return None;
            }
            Some(h as usize)
        });
        v.map(|x| x as *mut core::ffi::c_void)
    }

    /// 显式终结 Job 内全部进程（本机实测 TerminateJobObject 可靠；
    /// kill-on-close 标志在本机读回不符，不依赖）。幂等，可重复调用。
    pub fn terminate_all() {
        let Some(h) = job() else { return };
        unsafe {
            TerminateJobObject(h, 0);
        }
    }

    /// 把刚 spawn 的子进程挂进宿主 Job（失败静默：仅失去强杀回收保障，不影响功能）。
    pub fn attach(child: &impl AsRawHandle) {
        let Some(h) = job() else { return };
        let raw = child.as_raw_handle();
        if raw == 0 || raw == -1 {
            return;
        }
        unsafe {
            let _ = AssignProcessToJobObject(h, raw as *mut core::ffi::c_void);
        }
    }

    pub trait AsRawHandle {
        fn as_raw_handle(&self) -> isize;
    }

    impl AsRawHandle for std::process::Child {
        fn as_raw_handle(&self) -> isize {
            use std::os::windows::io::RawHandle;
            std::os::windows::io::AsRawHandle::as_raw_handle(self) as isize
        }
    }

    impl AsRawHandle for tokio::process::Child {
        fn as_raw_handle(&self) -> isize {
            // tokio Child 暴露 raw_handle()（Windows）；None 时（已回收）返回 0，attach 静默跳过
            self.raw_handle().map(|h| h as isize).unwrap_or(0)
        }
    }
}

#[cfg(windows)]
pub fn attach(child: &impl imp::AsRawHandle) {
    imp::attach(child);
}

#[cfg(windows)]
pub fn terminate_all() {
    imp::terminate_all();
}

#[cfg(not(windows))]
pub fn attach<T>(_: &T) {}

#[cfg(not(windows))]
pub fn terminate_all() {}
