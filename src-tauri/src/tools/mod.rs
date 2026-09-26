mod command_line;
mod exec_paths;
pub mod context;
pub mod dispatch;
pub mod exec;
pub mod file;
pub mod git;
pub mod history;
mod image_tool;
pub mod patch;
pub mod policy;
pub mod project;
pub mod registry;
pub mod session;
pub mod workspace;

pub use context::{SharedToolContext, ToolContext};
/// 工具执行仍唯一收敛到 call_tool；传输入口用 call_tool_with_audit 包装正常调用，
/// 提前拒绝用 record_tool_rejection_with_audit 补记，禁止 MCP/Actions 各自复制审计逻辑。
pub use dispatch::{call_tool, call_tool_with_audit, record_tool_rejection_with_audit};
pub use policy::{validate_actions_exposure, PolicySettings};
pub use registry::{
    exposed_tool_names, is_actions_tool, is_allowed_tool, list_tools, list_tools_for_profile,
    MUTATING_TOOLS,
};
pub use workspace::{wrap_mcp_tool_result, wrap_tool_result, Workspace};
