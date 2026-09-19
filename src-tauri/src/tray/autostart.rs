//! 开机自启：写入/移出 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`。
//!
//! 与参考实现一致，只有当前 Run 值与本 exe 的预期命令**完全一致**时才算已开启；
//! 陈旧或指向其它副本的项一律视为未开启。命令使用正确的引号包裹，因此
//! 含空格或中文的安装路径同样可用。
//!
//! 注册表访问通过可替换的后端进行，测试用模拟后端覆盖，不会触碰真实注册表。

/// 自启项所在的注册表子键。
pub const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// 自启项名称。
pub const RUN_VALUE: &str = "CodingToolsMcpDesktop";

/// 自启时传给自身的参数。
pub const AUTOSTART_FLAG: &str = "--autostart";

/// 当前进程的可执行文件路径；失败时返回 None。
pub fn current_exe() -> Option<String> {
    std::env::current_exe()
        .ok()
        .map(|path| path.to_string_lossy().to_string())
}

/// 为给定 exe 路径构造自启命令行。
///
/// 路径一律用双引号包裹，因此含空格、中文或较长路径都能正确启动。
pub fn command_for(exe: &str) -> String {
    format!("\"{exe}\" {AUTOSTART_FLAG}")
}

/// 期望写入 Run 项的命令行。
pub fn autostart_command() -> String {
    match current_exe() {
        Some(exe) => command_for(&exe),
        None => String::new(),
    }
}

/// 该平台是否支持开机自启（仅 Windows 的 HKCU Run）。
pub const fn is_supported() -> bool {
    cfg!(target_os = "windows")
}

/// 该命令是否属于本应用的自启命令（供启动参数判定复用）。
pub fn is_autostart_invocation() -> bool {
    std::env::args().any(|arg| arg == AUTOSTART_FLAG)
}

#[cfg(target_os = "windows")]
mod win {
    use super::*;

    /// 读取 Run 项。
    ///
    /// 区分「值不存在」（`Ok(None)`）与「读取失败」（`Err`）。两者语义完全不同：
    /// 权限错误若被当成「未启用」，关闭流程会错误地清空绑定。
    pub fn read_run_value() -> Result<Option<String>, String> {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
        use windows::Win32::System::Registry::{
            RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE,
            REG_SZ, REG_VALUE_TYPE,
        };

        let subkey: Vec<u16> = RUN_SUBKEY.encode_utf16().chain(std::iter::once(0)).collect();
        let name: Vec<u16> = RUN_VALUE.encode_utf16().chain(std::iter::once(0)).collect();

        unsafe {
            let mut key = HKEY::default();
            let status = RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                None,
                KEY_QUERY_VALUE,
                &mut key,
            );
            if status == ERROR_FILE_NOT_FOUND {
                // Run 键尚不存在 → 确实没有自启项。
                return Ok(None);
            }
            if status != ERROR_SUCCESS {
                return Err(format!("打开注册表 Run 键失败（错误码 {}）", status.0));
            }

            // 先问长度，再按实际字节数分配，避免固定缓冲截断长命令。
            let mut size = 0u32;
            let mut kind = REG_VALUE_TYPE::default();
            let status = RegQueryValueExW(
                key,
                PCWSTR(name.as_ptr()),
                None,
                Some(&mut kind),
                None,
                Some(&mut size),
            );
            if status == ERROR_FILE_NOT_FOUND {
                let _ = RegCloseKey(key);
                return Ok(None);
            }
            if status != ERROR_SUCCESS {
                let _ = RegCloseKey(key);
                return Err(format!("查询自启项长度失败（错误码 {}）", status.0));
            }
            if kind != REG_SZ {
                let _ = RegCloseKey(key);
                return Err("自启项类型不是 REG_SZ，数据已损坏".to_string());
            }
            if size == 0 || size > 64 * 1024 {
                let _ = RegCloseKey(key);
                return Err(format!("自启项长度异常（{size} 字节）"));
            }

            let mut buffer = vec![0u8; size as usize];
            let status = RegQueryValueExW(
                key,
                PCWSTR(name.as_ptr()),
                None,
                Some(&mut kind),
                Some(buffer.as_mut_ptr()),
                Some(&mut size),
            );
            let _ = RegCloseKey(key);
            if status != ERROR_SUCCESS {
                return Err(format!("读取自启项失败（错误码 {}）", status.0));
            }

            // 有界校验：必须偶数字节、以 UTF-16 NUL 结尾。
            let byte_len = size as usize;
            if byte_len == 0 || !byte_len.is_multiple_of(2) || byte_len > buffer.len() {
                return Err(format!("自启项字节长度异常（{byte_len}）"));
            }
            let units: Vec<u16> = buffer[..byte_len]
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            let text_units = match units.iter().position(|&u| u == 0) {
                Some(end) => &units[..end],
                None => return Err("自启项缺少结尾 NUL，数据已损坏".to_string()),
            };
            Ok(Some(String::from_utf16_lossy(text_units)))
        }
    }

    /// 写入（Some）或删除（None）Run 项。
    ///
    /// 删除时只把「值不存在」视为成功；其它错误必须上报，否则会留下
    /// 「仍启用但已清绑定」的不一致状态。
    pub fn write_run_value(value: Option<&str>) -> Result<(), String> {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
        use windows::Win32::System::Registry::{
            RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegSetValueExW, HKEY,
            HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
        };

        let subkey: Vec<u16> = RUN_SUBKEY.encode_utf16().chain(std::iter::once(0)).collect();
        let name: Vec<u16> = RUN_VALUE.encode_utf16().chain(std::iter::once(0)).collect();

        unsafe {
            let mut key = HKEY::default();
            let status = RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut key,
                None,
            );
            if status != ERROR_SUCCESS {
                return Err(format!("打开注册表 Run 键失败（错误码 {}）", status.0));
            }

            let outcome = match value {
                Some(text) => {
                    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
                    let bytes =
                        std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
                    let status =
                        RegSetValueExW(key, PCWSTR(name.as_ptr()), None, REG_SZ, Some(bytes));
                    if status == ERROR_SUCCESS {
                        Ok(())
                    } else {
                        Err(format!("写入自启项失败（错误码 {}）", status.0))
                    }
                }
                None => {
                    let status = RegDeleteValueW(key, PCWSTR(name.as_ptr()));
                    if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
                        Ok(())
                    } else {
                        Err(format!("删除自启项失败（错误码 {}）", status.0))
                    }
                }
            };
            let _ = RegCloseKey(key);
            outcome
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod win {
    use super::*;

    pub fn read_run_value() -> Result<Option<String>, String> {
        Ok(None)
    }

    pub fn write_run_value(_value: Option<&str>) -> Result<(), String> {
        Err("当前平台不支持开机自启".to_string())
    }
}

pub use win::{read_run_value, write_run_value};

/// Run 项状态。
///
/// 必须区分「确认未启用」与「读取失败」：后者不能当作未启用，
/// 否则一次权限错误就会导致关闭流程错误地清空绑定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunState {
    /// 注册表项存在且与本 exe 的预期命令一致。
    Enabled,
    /// 确认未启用（值不存在，或存在但不指向本应用）。
    Disabled,
    /// 读取失败，无法判定。
    Unknown(String),
}

impl RunState {
    /// 仅用于菜单勾选展示：无法判定时显示为未勾选。
    pub fn is_enabled(&self) -> bool {
        matches!(self, RunState::Enabled)
    }
}

/// 读取并判定当前 Run 项状态。
pub fn run_state() -> RunState {
    match read_run_value() {
        Ok(None) => RunState::Disabled,
        Ok(Some(value)) => {
            let expected = autostart_command();
            if !expected.is_empty() && value.trim().eq_ignore_ascii_case(expected.trim()) {
                RunState::Enabled
            } else {
                // 陈旧项或指向其它副本 → 视为未启用。
                RunState::Disabled
            }
        }
        Err(error) => RunState::Unknown(error),
    }
}

/// 便捷判定：无法判定时返回 false（仅用于展示）。
pub fn is_enabled() -> bool {
    run_state().is_enabled()
}

/// 可注入的自启存储操作，便于用内存假后端测试真实行为。
pub trait AutostartOps {
    fn read_run(&self) -> Result<Option<String>, String>;
    fn write_run(&self, value: Option<&str>) -> Result<(), String>;
    /// 读取绑定。读取失败必须报错，不得当成空绑定（否则回滚会丢掉旧目标）。
    fn read_binding(&self) -> Result<String, String>;
    /// 写入绑定。`restore = true` 表示这是回滚，不校验目标是否存在。
    fn write_binding(&self, value: &str, restore: bool) -> Result<(), String>;
}

/// 开启自启：先写注册表，再保存绑定；任一步失败都回滚到旧状态。
///
/// 只有写入、绑定、读回校验全部成功才算成功。
pub fn enable(ops: &impl AutostartOps, target: &str) -> Result<(), String> {
    let command = autostart_command();
    if command.is_empty() {
        return Err("无法确定当前程序路径，已取消开启自启".to_string());
    }

    // 记录旧状态用于回滚。读取失败时不能继续，否则回滚会丢失旧值。
    let previous_run = ops.read_run()?;
    let previous_binding = ops.read_binding()?;

    ops.write_run(Some(&command))?;

    if let Err(error) = ops.write_binding(target, false) {
        // 绑定保存失败 → 恢复注册表与旧绑定，避免「已启用但无目标」。
        return Err(rollback(ops, previous_run.as_deref(), &previous_binding, error));
    }

    // 读回校验：写入不一定等于生效。
    match ops.read_run() {
        Ok(Some(value)) if value.trim().eq_ignore_ascii_case(command.trim()) => Ok(()),
        Ok(other) => {
            let detail = match other {
                Some(value) => format!("读回值不匹配（{value}）"),
                None => "读回时自启项已不存在".to_string(),
            };
            Err(rollback(ops, previous_run.as_deref(), &previous_binding, detail))
        }
        Err(error) => Err(rollback(
            ops,
            previous_run.as_deref(),
            &previous_binding,
            error,
        )),
    }
}

/// 关闭自启：先删除并**确认已不存在**，再清绑定。
///
/// 删除失败或读回仍存在时，保留绑定并返回错误。
pub fn disable(ops: &impl AutostartOps) -> Result<(), String> {
    ops.write_run(None)?;

    match ops.read_run() {
        Ok(None) => {}
        Ok(Some(value)) => {
            return Err(format!("移除后读回仍存在自启项（{value}），已保留绑定"));
        }
        Err(error) => {
            return Err(format!("移除后无法确认自启项状态（{error}），已保留绑定"));
        }
    }

    ops.write_binding("", false)?;
    Ok(())
}

/// 回滚注册表与绑定；回滚本身失败也如实报告。
///
/// 恢复绑定使用 `restore = true`，因此旧绑定即使指向已删除的工作区也能原样写回。
fn rollback(
    ops: &impl AutostartOps,
    previous_run: Option<&str>,
    previous_binding: &str,
    cause: String,
) -> String {
    let mut problems = Vec::new();
    if let Err(error) = ops.write_run(previous_run) {
        problems.push(format!("恢复注册表失败：{error}"));
    }
    if let Err(error) = ops.write_binding(previous_binding, true) {
        problems.push(format!("恢复绑定失败：{error}"));
    }
    if problems.is_empty() {
        format!("开启自启失败，已恢复原状态：{cause}")
    } else {
        format!(
            "开启自启失败（{cause}），且回滚不完整：{}",
            problems.join("；")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// 内存假后端：可注入读写失败，用于验证真实回滚行为。
    #[derive(Default)]
    struct FakeOps {
        run: RefCell<Option<String>>,
        binding: RefCell<String>,
        /// 已知存在的工作区 id（模拟真实存储的存在性校验）。
        known: Vec<String>,
        /// 让 write_run 返回失败。
        fail_write_run: RefCell<bool>,
        /// 让 write_binding 返回失败。
        fail_write_binding: RefCell<bool>,
        /// 让 read_run 返回失败（模拟权限错误）。
        fail_read_run: RefCell<bool>,
        /// 让 read_binding 返回失败。
        fail_read_binding: RefCell<bool>,
        /// 让写入被静默丢弃（写成功但读回不生效）。
        swallow_write: RefCell<bool>,
        /// 记录写入顺序，验证回滚确实发生。
        log: RefCell<Vec<String>>,
    }

    impl FakeOps {
        fn with_run(value: Option<&str>) -> Self {
            let ops = Self::default();
            *ops.run.borrow_mut() = value.map(|v| v.to_string());
            ops
        }

        /// 声明存在的工作区 id，使非回滚写入能通过校验。
        fn with_known(mut self, ids: &[&str]) -> Self {
            self.known = ids.iter().map(|s| s.to_string()).collect();
            self
        }

        fn binding(&self) -> String {
            self.binding.borrow().clone()
        }
    }

    impl AutostartOps for FakeOps {
        fn read_run(&self) -> Result<Option<String>, String> {
            if *self.fail_read_run.borrow() {
                return Err("读取注册表失败（模拟权限错误）".into());
            }
            Ok(self.run.borrow().clone())
        }

        fn write_run(&self, value: Option<&str>) -> Result<(), String> {
            if *self.fail_write_run.borrow() {
                return Err("写入注册表失败".into());
            }
            self.log.borrow_mut().push(match value {
                Some(v) => format!("set:{v}"),
                None => "del".to_string(),
            });
            if !*self.swallow_write.borrow() {
                *self.run.borrow_mut() = value.map(|v| v.to_string());
            }
            Ok(())
        }

        fn read_binding(&self) -> Result<String, String> {
            if *self.fail_read_binding.borrow() {
                return Err("读取绑定失败".into());
            }
            Ok(self.binding.borrow().clone())
        }

        fn write_binding(&self, value: &str, restore: bool) -> Result<(), String> {
            if *self.fail_write_binding.borrow() {
                return Err("保存绑定失败".into());
            }
            // 非回滚路径模拟真实校验：目标必须存在。
            if !restore && !value.is_empty() && !self.known.iter().any(|k| k == value) {
                return Err(format!("workspace not found: {value}"));
            }
            self.log.borrow_mut().push(format!("bind:{value}"));
            *self.binding.borrow_mut() = value.to_string();
            Ok(())
        }
    }

    #[test]
    fn command_quotes_exe_path() {
        let command = autostart_command();
        assert!(command.starts_with('"'), "路径必须被引号包裹：{command}");
        assert!(command.ends_with(AUTOSTART_FLAG));
    }

    #[test]
    fn command_handles_spaces_chinese_and_long_paths() {
        // 安装目录实际形如 "...\AppData\Local\Coding Tools MCP\..."（含空格）。
        let long_dir = "很长的目录名\\".repeat(20);
        let cases = [
            r"C:\Users\me\AppData\Local\Coding Tools MCP\coding-tools-mcp-desktop.exe",
            r"D:\软件\编码 工具\应用.exe",
            &format!(r"C:\{long_dir}app.exe"),
        ];
        for exe in cases {
            let command = command_for(exe);
            assert!(
                command.starts_with(&format!("\"{exe}\"")),
                "路径必须整体被引号包裹：{command}"
            );
            assert!(command.ends_with(AUTOSTART_FLAG));
            assert_eq!(command.matches('"').count(), 2, "引号不成对：{command}");
        }
    }

    #[test]
    fn enable_writes_run_then_binding() {
        let ops = FakeOps::with_run(None).with_known(&["ws-1"]);
        enable(&ops, "ws-1").expect("enable should succeed");
        let log = ops.log.borrow().clone();
        assert_eq!(log.len(), 2, "{log:?}");
        assert!(log[0].starts_with("set:"), "{log:?}");
        assert_eq!(log[1], "bind:ws-1");
        assert_eq!(ops.binding(), "ws-1");
    }

    #[test]
    fn enable_rolls_back_when_binding_save_fails() {
        let ops = FakeOps::with_run(None).with_known(&["ws-1"]);
        *ops.fail_write_binding.borrow_mut() = true;

        let error = enable(&ops, "ws-1").expect_err("must fail");
        assert!(error.contains("恢复"), "错误应说明已回滚：{error}");
        assert_eq!(ops.run.borrow().clone(), None, "注册表应回滚为无自启项");
        assert_eq!(ops.binding(), "", "绑定不应被改动");
    }

    #[test]
    fn enable_rolls_back_when_readback_does_not_match() {
        let previous = format!("\"old.exe\" {AUTOSTART_FLAG}");
        let ops = FakeOps::with_run(Some(&previous)).with_known(&["ws-1"]);
        // 写入被静默丢弃 → 读回仍是旧值。
        *ops.swallow_write.borrow_mut() = true;

        let error = enable(&ops, "ws-1").expect_err("must fail");
        assert!(error.contains("读回值不匹配"), "{error}");
        assert_eq!(
            ops.run.borrow().clone().as_deref(),
            Some(previous.as_str()),
            "应恢复为写入前的旧值"
        );
        assert_eq!(ops.binding(), "", "绑定应回滚");
    }

    #[test]
    fn enable_reports_read_failure_instead_of_claiming_success() {
        let ops = FakeOps::with_run(None).with_known(&["ws-1"]);
        *ops.fail_read_run.borrow_mut() = true;
        let error = enable(&ops, "ws-1").expect_err("读取失败必须报错");
        assert!(error.contains("读取注册表失败"), "{error}");
    }

    #[test]
    fn enable_only_touches_this_apps_run_entry() {
        // 假后端只持有本应用的值；验证我们从不写入其它应用的项。
        let ops = FakeOps::with_run(Some("\"other-app.exe\" --whatever")).with_known(&["ws-1"]);
        enable(&ops, "ws-1").expect("enable should succeed");
        let log = ops.log.borrow().clone();
        assert_eq!(log.len(), 2, "只应写本应用项与绑定：{log:?}");
        assert!(!log.iter().any(|entry| entry.contains("other-app")));
    }

    #[test]
    fn disable_removes_run_then_binding() {
        let ops = FakeOps::with_run(Some(&autostart_command()));
        *ops.binding.borrow_mut() = "ws-1".into();

        disable(&ops).expect("disable should succeed");
        assert_eq!(ops.run.borrow().clone(), None);
        assert_eq!(ops.binding(), "", "确认删除后才清绑定");
    }

    #[test]
    fn disable_keeps_binding_when_delete_fails() {
        let ops = FakeOps::with_run(Some(&autostart_command()));
        *ops.binding.borrow_mut() = "ws-1".into();
        *ops.fail_write_run.borrow_mut() = true;

        let error = disable(&ops).expect_err("删除失败必须报错");
        assert!(error.contains("写入注册表失败"), "{error}");
        assert_eq!(ops.binding(), "ws-1", "删除失败必须保留绑定");
    }

    #[test]
    fn disable_keeps_binding_when_run_value_survives() {
        let ops = FakeOps::with_run(Some(&autostart_command()));
        *ops.binding.borrow_mut() = "ws-1".into();
        // 删除被静默忽略 → 读回仍存在。
        *ops.swallow_write.borrow_mut() = true;

        let error = disable(&ops).expect_err("读回仍存在必须报错");
        assert!(error.contains("仍存在"), "{error}");
        assert_eq!(ops.binding(), "ws-1", "未确认删除必须保留绑定");
    }

    #[test]
    fn disable_keeps_binding_when_readback_is_unknown() {
        let ops = FakeOps::with_run(Some(&autostart_command()));
        *ops.binding.borrow_mut() = "ws-1".into();
        *ops.fail_read_run.borrow_mut() = true;

        let error = disable(&ops).expect_err("无法判定必须报错");
        assert!(error.contains("无法确认"), "{error}");
        assert_eq!(
            ops.binding(),
            "ws-1",
            "读取失败不得当成「未启用」而清空绑定"
        );
    }

    #[test]
    fn run_state_distinguishes_missing_from_failure() {
        let ops = FakeOps::with_run(None);
        assert_eq!(ops.read_run(), Ok(None), "缺失必须可区分于失败");
        *ops.fail_read_run.borrow_mut() = true;
        assert!(ops.read_run().is_err(), "读取失败必须是 Err");
    }

    #[test]
    fn autostart_flag_is_recognised() {
        assert_eq!(AUTOSTART_FLAG, "--autostart");
        assert!(!is_autostart_invocation());
    }

    #[test]
    fn enable_restores_old_binding_pointing_at_deleted_workspace() {
        // 旧绑定指向一个已不存在的工作区。开启失败回滚时必须能原样写回，
        // 不能被存在性校验拦住（这正是 restore 标志存在的理由）。
        let ops = FakeOps::with_run(None).with_known(&["ws-new"]);
        *ops.binding.borrow_mut() = "ws-deleted".into();
        // 让读回校验失败（写入被静默丢弃）。
        *ops.swallow_write.borrow_mut() = true;

        let error = enable(&ops, "ws-new").expect_err("must fail");
        assert!(error.contains("已恢复原状态"), "{error}");
        assert_eq!(
            ops.binding(),
            "ws-deleted",
            "必须能恢复指向已删除工作区的旧绑定"
        );
    }

    #[test]
    fn enable_reports_rollback_failure_explicitly() {
        // 回滚本身也失败时，错误信息必须明确报告，不能假装已恢复。
        let ops = FakeOps::with_run(Some("\"old.exe\" --autostart")).with_known(&["ws-1"]);
        *ops.swallow_write.borrow_mut() = true;
        *ops.fail_write_binding.borrow_mut() = true;

        let error = enable(&ops, "ws-1").expect_err("must fail");
        assert!(
            error.contains("回滚不完整"),
            "回滚失败必须明确报告：{error}"
        );
        assert!(error.contains("恢复绑定失败"), "{error}");
    }

    #[test]
    fn enable_aborts_when_binding_cannot_be_read() {
        // 读不到旧绑定就无法回滚，此时必须直接取消，不写注册表。
        let ops = FakeOps::with_run(None).with_known(&["ws-1"]);
        *ops.fail_read_binding.borrow_mut() = true;

        let error = enable(&ops, "ws-1").expect_err("must fail");
        assert!(error.contains("读取绑定失败"), "{error}");
        assert_eq!(ops.run.borrow().clone(), None, "不得写入注册表");
    }
}
