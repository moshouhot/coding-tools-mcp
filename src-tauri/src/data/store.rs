use std::sync::Mutex;

use crate::error::{AppError, AppResult};
use crate::settings::AppSettings;
use crate::workspace::legacy_import::import_legacy_profiles_if_empty;
use crate::workspace::WorkspaceProfile;

use super::migrate::{data_file_path, load_or_migrate, maybe_backup_legacy_files, save};
use super::model::AppData;

/// 登录启动时对自启绑定目标的解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutostartTarget {
    /// 绑定有效。
    Bound(String),
    /// 从未绑定。
    Unbound,
    /// 曾绑定，但工作区已被删除。
    Missing(String),
}

static DATA_FILE_LOCK: Mutex<()> = Mutex::new(());

const SHARED_KEYS: &[&str] = &[
    "oauth_client_id",
    "bearer_token",
    "oauth_client_secret",
    "oauth_password",
    "oauth_token_secret",
    "actions_api_key",
    "actions_oauth_client_secret",
    "actions_oauth_password",
    "actions_oauth_token_secret",
];

#[derive(Debug)]
pub struct DataStore {
    data: AppData,
}

impl DataStore {
    pub fn load() -> AppResult<Self> {
        let _guard = lock_data_file()?;
        let path = data_file_path()?;
        let existed_before = path.exists();
        let mut data = load_or_migrate()?;
        let imported = import_legacy_profiles_if_empty(&mut data)?;
        let store = Self { data };
        if !existed_before || imported > 0 {
            store.persist_unlocked()?;
        }
        if !existed_before {
            maybe_backup_legacy_files(&path)?;
        }
        Ok(store)
    }

    pub fn read_file<R>(f: impl FnOnce(&AppData) -> AppResult<R>) -> AppResult<R> {
        let _guard = lock_data_file()?;
        let data = load_or_migrate()?;
        f(&data)
    }

    pub fn update_file<R>(f: impl FnOnce(&mut AppData) -> AppResult<R>) -> AppResult<R> {
        let _guard = lock_data_file()?;
        let mut data = load_or_migrate()?;
        let result = f(&mut data)?;
        save(&data)?;
        Ok(result)
    }

    pub fn data(&self) -> &AppData {
        &self.data
    }

    pub fn save(&self) -> AppResult<()> {
        let _guard = lock_data_file()?;
        self.persist_unlocked()
    }

    fn persist_unlocked(&self) -> AppResult<()> {
        save(&self.data)
    }

    pub fn settings(&self) -> AppSettings {
        AppSettings::from_data(&self.data)
    }

    pub fn update_settings(&mut self, settings: AppSettings) -> AppResult<()> {
        settings.apply_to(&mut self.data);
        self.save()
    }

    pub fn list(&self) -> &[WorkspaceProfile] {
        &self.data.profiles
    }

    pub fn get(&self, id: &str) -> Option<&WorkspaceProfile> {
        self.data.profiles.iter().find(|profile| profile.id == id)
    }

    /// 已持久化的自启绑定目标，原样返回（可能为空或已失效）。
    /// 仅用于测试与诊断；业务逻辑请用 `autostart_launch_target`。
    #[allow(dead_code)]
    pub fn autostart_workspace_id(&self) -> String {
        self.data.autostart_workspace_id.clone()
    }

    /// 用户在界面中明确选中的工作区 id。
    ///
    /// **不**回退到首个工作区：开启自启必须由用户明确选择，
    /// 否则会在多工作区下默默绑定到任意一个。
    pub fn selected_workspace_id(&self) -> String {
        selected_workspace_id_of(&self.data)
    }

    /// 登录启动时解析绑定目标。**不回退**：未绑定或已失效都明确报告。
    pub fn autostart_launch_target(&self) -> AutostartTarget {
        autostart_launch_target_of(&self.data)
    }

    /// 绑定开机自启要启动的工作区（会校验工作区存在）。
    pub fn set_autostart_workspace(&mut self, id: &str) -> AppResult<()> {
        if self.get(id).is_none() {
            return Err(AppError::Message(format!("workspace not found: {id}")));
        }
        self.write_autostart_workspace(id)
    }

    /// 原样恢复绑定值，**不校验工作区是否存在**。
    ///
    /// 回滚时旧绑定可能指向一个已被删除的工作区，用 `set_autostart_workspace`
    /// 会因存在性校验而拒绝恢复，因此必须走这个不校验的入口。
    pub fn restore_autostart_workspace(&mut self, id: &str) -> AppResult<()> {
        self.write_autostart_workspace(id)
    }

    /// 解除绑定（关闭自启时使用）。
    pub fn clear_autostart_workspace(&mut self) -> AppResult<()> {
        self.write_autostart_workspace("")
    }

    /// 写入绑定并落盘；**落盘失败时恢复内存中的旧值**，避免内存与磁盘不一致。
    fn write_autostart_workspace(&mut self, id: &str) -> AppResult<()> {
        write_binding_with_rollback(&mut self.data, id, save)
    }

    pub fn add(&mut self, profile: WorkspaceProfile) -> AppResult<()> {
        self.data.profiles.push(profile);
        self.save()
    }

    pub fn update(&mut self, profile: WorkspaceProfile) -> AppResult<()> {
        let Some(index) = self
            .data
            .profiles
            .iter()
            .position(|item| item.id == profile.id)
        else {
            return Err(AppError::Message(format!(
                "workspace not found: {}",
                profile.id
            )));
        };
        self.data.profiles[index] = profile;
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> AppResult<Option<WorkspaceProfile>> {
        let Some(index) = self.data.profiles.iter().position(|item| item.id == id) else {
            return Ok(None);
        };
        let removed = self.data.profiles.remove(index);
        self.data.workspace_secrets.remove(id);
        self.save()?;
        Ok(Some(removed))
    }

    pub fn init_workspace_secrets(&mut self, profile_id: &str) -> AppResult<()> {
        // oauth_client_secret is optional for MCP OAuth (ChatGPT PKCE); not auto-generated.
        self.set_workspace_secret(profile_id, "oauth_password", &random_secret())?;
        self.set_workspace_secret(profile_id, "oauth_token_secret", &random_secret())?;
        self.set_workspace_secret(profile_id, "bearer_token", &random_secret())?;
        self.set_workspace_secret(profile_id, "actions_api_key", &random_secret())?;
        self.set_workspace_secret(profile_id, "actions_oauth_client_secret", &random_secret())?;
        self.set_workspace_secret(profile_id, "actions_oauth_password", &random_secret())?;
        self.set_workspace_secret(profile_id, "actions_oauth_token_secret", &random_secret())?;
        Ok(())
    }

    pub fn init_shared_secrets(&mut self) -> AppResult<()> {
        let mut changed = false;
        for key in SHARED_KEYS {
            if !self.data.shared_secrets.contains_key(*key) {
                self.data
                    .shared_secrets
                    .insert(key.to_string(), shared_value_for_key(key));
                changed = true;
            }
        }
        if changed {
            self.save()?;
        }
        Ok(())
    }

    pub fn get_workspace_secret(&self, profile_id: &str, key: &str) -> AppResult<Option<String>> {
        Ok(self
            .data
            .workspace_secrets
            .get(profile_id)
            .and_then(|secrets| secrets.get(key))
            .filter(|value| !value.is_empty())
            .cloned())
    }

    pub fn set_workspace_secret(
        &mut self,
        profile_id: &str,
        key: &str,
        value: &str,
    ) -> AppResult<()> {
        self.data
            .workspace_secrets
            .entry(profile_id.to_string())
            .or_default()
            .insert(key.to_string(), value.to_string());
        self.save()
    }

    pub fn regenerate_workspace_secret(&mut self, profile_id: &str, key: &str) -> AppResult<String> {
        let value = shared_value_for_key(key);
        self.set_workspace_secret(profile_id, key, &value)?;
        Ok(value)
    }

    pub fn remove_workspace_secrets(&mut self, profile_id: &str) -> AppResult<()> {
        self.data.workspace_secrets.remove(profile_id);
        self.save()
    }

    pub fn get_shared_secret(&self, key: &str) -> Option<String> {
        self.data.shared_secrets.get(key).cloned()
    }

    pub fn set_shared_secret(&mut self, key: &str, value: &str) -> AppResult<()> {
        self.data
            .shared_secrets
            .insert(key.to_string(), value.to_string());
        self.save()
    }

    pub fn regenerate_shared_secret(&mut self, key: &str) -> AppResult<String> {
        let value = random_secret();
        self.set_shared_secret(key, &value)?;
        Ok(value)
    }

    pub fn get_app_secret(&self, scope: &str, item_id: &str) -> Option<String> {
        self.data
            .app_secrets
            .get(scope)
            .and_then(|items| items.get(item_id))
            .filter(|value| !value.is_empty())
            .cloned()
    }

    pub fn set_app_secret(&mut self, scope: &str, item_id: &str, value: &str) -> AppResult<()> {
        self.data
            .app_secrets
            .entry(scope.to_string())
            .or_default()
            .insert(item_id.to_string(), value.to_string());
        self.save()
    }

    pub fn delete_app_secret(&mut self, scope: &str, item_id: &str) -> AppResult<()> {
        if let Some(items) = self.data.app_secrets.get_mut(scope) {
            items.remove(item_id);
            if items.is_empty() {
                self.data.app_secrets.remove(scope);
            }
        }
        self.save()
    }

}

fn lock_data_file() -> AppResult<std::sync::MutexGuard<'static, ()>> {
    DATA_FILE_LOCK
        .lock()
        .map_err(|_| AppError::Message("data file lock poisoned".into()))
}

/// 写入绑定并在落盘失败时回滚内存值。
///
/// 抽成纯函数以便注入失败的持久化实现，从而验证「内存不得与磁盘不一致」
/// 这一行为，而不是只测返回值。
fn write_binding_with_rollback(
    data: &mut AppData,
    id: &str,
    persist: impl FnOnce(&AppData) -> AppResult<()>,
) -> AppResult<()> {
    let previous = data.autostart_workspace_id.clone();
    data.autostart_workspace_id = id.to_string();
    if let Err(error) = persist(data) {
        // 落盘失败 → 恢复旧绑定，避免内存里留下一个没写进磁盘的值。
        data.autostart_workspace_id = previous;
        return Err(error);
    }
    Ok(())
}

/// 纯函数：用户明确选中的工作区 id（无效则空，不回退）。
fn selected_workspace_id_of(data: &AppData) -> String {
    let last = data.last_workspace_id.trim();
    if !last.is_empty() && data.profiles.iter().any(|p| p.id == last) {
        return last.to_string();
    }
    String::new()
}

/// 纯函数：登录启动时解析绑定目标，**不回退**。
fn autostart_launch_target_of(data: &AppData) -> AutostartTarget {
    let bound = data.autostart_workspace_id.trim();
    if bound.is_empty() {
        return AutostartTarget::Unbound;
    }
    if data.profiles.iter().any(|p| p.id == bound) {
        AutostartTarget::Bound(bound.to_string())
    } else {
        AutostartTarget::Missing(bound.to_string())
    }
}

fn random_secret() -> String {
    format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4()).replace('-', "")
}

fn shared_value_for_key(key: &str) -> String {
    if key == "oauth_client_id" {
        format!("chatgpt-client-{}", &uuid::Uuid::new_v4().to_string()[..12])
    } else {
        random_secret()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_secret_roundtrip() {
        let id = uuid::Uuid::new_v4().to_string().replace('-', "");
        let mut store = DataStore::load().expect("load");
        store
            .set_workspace_secret(&id, "oauth_client_secret", "roundtrip-secret")
            .expect("set");
        let loaded = store
            .get_workspace_secret(&id, "oauth_client_secret")
            .expect("get");
        assert_eq!(loaded.as_deref(), Some("roundtrip-secret"));
        store.remove_workspace_secrets(&id).expect("remove");
    }

    #[test]
    fn shared_oauth_client_id_uses_client_id_format() {
        let value = shared_value_for_key("oauth_client_id");
        assert!(value.starts_with("chatgpt-client-"));
        assert_eq!(value.len(), "chatgpt-client-".len() + 12);
    }

    fn data_with(ids: &[&str], last: &str, bound: &str) -> AppData {
        let profiles = ids
            .iter()
            .map(|id| {
                // id 在 WorkspaceProfile::new 中是随机 UUID，测试必须显式指定。
                let mut profile =
                    WorkspaceProfile::new(format!("F:\\proj\\{id}"), Some((*id).into()));
                profile.id = (*id).to_string();
                profile
            })
            .collect();
        AppData {
            profiles,
            last_workspace_id: last.to_string(),
            autostart_workspace_id: bound.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn selected_workspace_never_falls_back_to_first() {
        // 用户明确选中 b → 用 b。
        let a = data_with(&["a", "b"], "b", "");
        assert_eq!(selected_workspace_id_of(&a), "b");

        // 未选中 / 选中项失效 → 返回空，绝不回退到首个工作区。
        let b = data_with(&["a", "b"], "", "");
        assert_eq!(selected_workspace_id_of(&b), "", "不得回退到首个工作区");
        let c = data_with(&["a", "b"], "missing", "");
        assert_eq!(selected_workspace_id_of(&c), "", "失效选中不得回退");
    }

    #[test]
    fn launch_target_never_falls_back_to_another_workspace() {
        // 绑定有效 → 就用绑定项，而不是 last/first。
        let a = data_with(&["a", "b"], "b", "a");
        assert_eq!(
            autostart_launch_target_of(&a),
            AutostartTarget::Bound("a".into())
        );

        // 关键回归：绑定工作区被删除时，绝不回退启动另一个工作区。
        let b = data_with(&["a", "b"], "b", "deleted");
        assert_eq!(
            autostart_launch_target_of(&b),
            AutostartTarget::Missing("deleted".into()),
            "绑定失效必须报告 Missing，不能回退到 b"
        );

        // 从未绑定 → Unbound。
        let c = data_with(&["a"], "a", "");
        assert_eq!(autostart_launch_target_of(&c), AutostartTarget::Unbound);
    }

    #[test]
    fn binding_write_rolls_back_memory_when_persist_fails() {
        // 真实 DataStore 行为：内存先改、再落盘；落盘失败必须把内存改回旧值，
        // 否则内存会留下一个从未写入磁盘的绑定。
        let mut data = data_with(&["a"], "a", "old-binding");

        let error = write_binding_with_rollback(&mut data, "new-binding", |_| {
            Err(AppError::Message("disk write failed".into()))
        })
        .expect_err("persist failure must surface");

        assert!(error.to_string().contains("disk write failed"), "{error}");
        assert_eq!(
            data.autostart_workspace_id, "old-binding",
            "落盘失败后内存必须恢复旧绑定"
        );
    }

    #[test]
    fn binding_write_keeps_new_value_when_persist_succeeds() {
        let mut data = data_with(&["a"], "a", "old-binding");
        write_binding_with_rollback(&mut data, "new-binding", |_| Ok(()))
            .expect("persist should succeed");
        assert_eq!(data.autostart_workspace_id, "new-binding");
    }

    #[test]
    fn binding_rollback_can_restore_deleted_workspace_id() {
        // 回滚路径不校验工作区存在性：旧绑定可能指向已删除的工作区。
        let mut data = data_with(&["a"], "a", "ghost-workspace");
        write_binding_with_rollback(&mut data, "a", |_| {
            Err(AppError::Message("boom".into()))
        })
        .expect_err("must fail");
        assert_eq!(data.autostart_workspace_id, "ghost-workspace");
    }

    #[test]
    fn launch_target_ignores_whitespace_binding() {
        let d = data_with(&["a"], "a", "   ");
        assert_eq!(autostart_launch_target_of(&d), AutostartTarget::Unbound);
    }
}
